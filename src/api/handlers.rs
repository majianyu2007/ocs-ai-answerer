use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::AppState;
use super::ocs_config::answerer_wrapper;
use super::types::*;
use crate::answer::{Question, QuestionInput};
use crate::error::AppError;
use crate::provider::{AiClient, ChatRequest};

/// Errors are returned with HTTP 200 and `code: 0`: OCS's GM_xmlhttpRequest path rejects every
/// non-200 response and would discard the message, while `code: 0` reaches the handler.
fn error_response(err: &AppError, question: Option<String>) -> Response {
    Json(ErrorBody {
        code: 0,
        msg: err.to_string(),
        error_type: err.kind(),
        question,
    })
    .into_response()
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
}

fn check_token(state: &AppState, headers: &HeaderMap, provided: Option<&str>) -> bool {
    let expected = state.config.server.access_token.as_str();
    if expected.is_empty() {
        return true;
    }
    let provided = provided
        .filter(|t| !t.is_empty())
        .or_else(|| bearer_token(headers))
        .or_else(|| headers.get("x-access-token").and_then(|v| v.to_str().ok()));
    provided.is_some_and(|p| constant_time_eq(p.as_bytes(), expected.as_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub async fn answer_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<AnswerQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    match query {
        Ok(Query(q)) => handle_answer(state, headers, q.into()).await,
        Err(e) => error_response(&AppError::BadRequest(e.body_text()), None),
    }
}

pub async fn answer_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let req = if body.iter().all(u8::is_ascii_whitespace) {
        AnswerRequest::default()
    } else {
        match parse_post_body(&headers, &body) {
            Ok(r) => r,
            Err(msg) => return error_response(&AppError::BadRequest(msg), None),
        }
    };
    handle_answer(state, headers, req).await
}

/// Accepts JSON (OCS default) and form bodies; Content-Type is not trusted blindly because some
/// userscript managers send JSON as text/plain.
fn parse_post_body(headers: &HeaderMap, body: &[u8]) -> Result<AnswerRequest, String> {
    let is_form = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"));
    if is_form {
        return serde_urlencoded_form(body);
    }
    serde_json::from_slice::<AnswerRequest>(body).map_err(|e| format!("请求体不是合法 JSON: {e}"))
}

fn serde_urlencoded_form(body: &[u8]) -> Result<AnswerRequest, String> {
    let pairs = url::form_urlencoded::parse(body);
    let mut q = AnswerQuery::default();
    for (k, v) in pairs {
        let v = v.into_owned();
        match k.as_ref() {
            "title" | "question" => q.title = Some(v),
            "type" => q.qtype = Some(v),
            "options" => q.options = Some(v),
            "token" => q.token = Some(v),
            "no_cache" => q.no_cache = Some(v),
            _ => {}
        }
    }
    Ok(q.into())
}

async fn handle_answer(state: AppState, headers: HeaderMap, req: AnswerRequest) -> Response {
    if !check_token(&state, &headers, req.token.as_deref()) {
        tracing::warn!("拒绝访问：访问令牌无效或缺失");
        return error_response(&AppError::Unauthorized, None);
    }
    let title = req.title.unwrap_or_default();
    if title.trim().is_empty() {
        return error_response(&AppError::BadRequest("缺少题目内容 title".into()), None);
    }

    let question = Question::from_input(QuestionInput {
        title,
        qtype: req.qtype.unwrap_or_default(),
        options: req.options.into_vec(),
        images: req.images.into_vec(),
        suggestion_title: req.suggestion_title.unwrap_or_default(),
        suggestion_options: req.suggestion_options.unwrap_or_default(),
    });
    let started = Instant::now();
    let span = tracing::info_span!(
        "answer",
        qtype = question.qtype.as_str(),
        options = question.options.len(),
        images = question.images.len(),
    );

    let timeout_secs = state.config.server.request_timeout_secs;
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        tracing::Instrument::instrument(
            state.answerer.answer(&question, req.no_cache),
            span.clone(),
        ),
    )
    .await
    .unwrap_or(Err(AppError::Timeout(timeout_secs)));

    let elapsed_ms = started.elapsed().as_millis() as u64;
    let title_preview = crate::error::truncate(&question.title, 40);
    match result {
        Ok(outcome) => {
            tracing::info!(
                parent: &span,
                model = %outcome.model,
                elapsed_ms,
                cached = outcome.cached,
                confidence = outcome.confidence.map(|c| format!("{c:.2}")).unwrap_or_default(),
                title = %title_preview,
                answer = %outcome.answer,
                "答题成功"
            );
            Json(AnswerSuccess {
                code: 1,
                question: outcome.question,
                answer: outcome.answer,
                qtype: outcome.qtype.as_str(),
                explanation: outcome.explanation,
                model: outcome.model,
                provider: outcome.provider,
                elapsed_ms,
                ai: true,
                cached: outcome.cached,
                confidence: outcome.confidence,
                votes: outcome.votes,
            })
            .into_response()
        }
        Err(err) => {
            tracing::warn!(
                parent: &span,
                model = %state.answerer.client().name(),
                elapsed_ms,
                title = %title_preview,
                error_type = err.kind(),
                error = %err,
                "答题失败"
            );
            error_response(&err, Some(question.title))
        }
    }
}

async fn probe(client: &AiClient) -> UpstreamStatus {
    let started = Instant::now();
    let req = ChatRequest::single("", "ping，请只回复 ok");
    let latency = || Some(started.elapsed().as_millis() as u64);
    match client.complete(&req).await {
        Ok(_) => UpstreamStatus {
            status: "ok",
            latency_ms: latency(),
            error: None,
            hint: None,
        },
        Err(e) => UpstreamStatus {
            status: "error",
            latency_ms: latency(),
            error: Some(e.to_string()),
            hint: None,
        },
    }
}

fn model_status(client: &AiClient, upstream: Option<UpstreamStatus>) -> ModelStatus {
    ModelStatus {
        name: client.name().to_string(),
        provider: client.kind().as_str(),
        model: client.model().to_string(),
        endpoint: client.endpoint().to_string(),
        vision: client.accepts_images(),
        upstream,
    }
}

pub async fn health(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HealthQuery>,
) -> Response {
    let answerer = &state.answerer;
    let client = answerer.client();
    let want_check = matches!(
        q.check.as_deref().map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes")
    );
    if want_check && !check_token(&state, &headers, q.token.as_deref()) {
        return error_response(&AppError::Unauthorized, None);
    }

    let mut upstream = if want_check {
        probe(client).await
    } else {
        UpstreamStatus {
            status: "unchecked",
            latency_ms: None,
            error: None,
            hint: Some(
                "仅表示服务进程正常；访问 /health?check=true 会向模型发送一次真实请求以检测上游连通性",
            ),
        }
    };

    let extra = &answerer.voters()[1..];
    let mut models = Vec::with_capacity(extra.len());
    let judge;
    if want_check {
        let (voter_results, judge_result) = tokio::join!(
            futures_util::future::join_all(extra.iter().map(probe)),
            async {
                match answerer.judge() {
                    Some(j) => Some(probe(j).await),
                    None => None,
                }
            }
        );
        for (c, st) in extra.iter().zip(voter_results) {
            models.push(model_status(c, Some(st)));
        }
        judge = answerer.judge().map(|j| model_status(j, judge_result));
    } else {
        models.extend(extra.iter().map(|c| model_status(c, None)));
        judge = answerer.judge().map(|j| model_status(j, None));
    }
    if want_check
        && upstream.status == "ok"
        && (models.iter().chain(judge.iter()))
            .any(|m| m.upstream.as_ref().is_some_and(|u| u.status != "ok"))
    {
        upstream.hint = Some("主模型正常，但部分投票模型或裁判模型不可用，详见 ensemble 字段");
    }

    Json(HealthBody {
        status: "ok",
        service: "ocs-ai-answerer",
        version: env!("CARGO_PKG_VERSION"),
        provider: client.kind().as_str(),
        model: client.model().to_string(),
        endpoint: client.endpoint().to_string(),
        uptime_secs: state.started_at.elapsed().as_secs(),
        upstream,
        ensemble: EnsembleStatus {
            enabled: answerer.ensemble_enabled(),
            models,
            judge,
        },
        cache: CacheStatus {
            enabled: answerer.cache().is_some(),
            entries: answerer.cache().map(|c| c.len()).unwrap_or(0),
        },
    })
    .into_response()
}

/// `DELETE /api/cache`: clears every cached answer (memory and file).
pub async fn cache_clear(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !check_token(&state, &headers, q.token.as_deref()) {
        return error_response(&AppError::Unauthorized, None);
    }
    match state.answerer.cache() {
        None => Json(serde_json::json!({ "code": 1, "msg": "缓存未启用", "cleared": 0 }))
            .into_response(),
        Some(cache) => {
            let n = cache.len();
            match cache.clear() {
                Ok(()) => {
                    tracing::info!(entries = n, "已清空答案缓存");
                    Json(serde_json::json!({ "code": 1, "msg": "缓存已清空", "cleared": n }))
                        .into_response()
                }
                Err(e) => {
                    Json(serde_json::json!({ "code": 0, "msg": format!("清空缓存文件失败: {e}") }))
                        .into_response()
                }
            }
        }
    }
}

/// `POST /api/cache/forget`: removes one question (same body as `/api/answer`), e.g. after a
/// wrong answer was cached.
pub async fn cache_forget(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let req = match parse_post_body(&headers, &body) {
        Ok(r) => r,
        Err(msg) => return error_response(&AppError::BadRequest(msg), None),
    };
    if !check_token(&state, &headers, req.token.as_deref()) {
        return error_response(&AppError::Unauthorized, None);
    }
    let question = Question::from_input(QuestionInput {
        title: req.title.unwrap_or_default(),
        qtype: req.qtype.unwrap_or_default(),
        options: req.options.into_vec(),
        images: req.images.into_vec(),
        ..Default::default()
    });
    let removed = state
        .answerer
        .cache()
        .is_some_and(|c| c.remove(&question.fingerprint()));
    Json(serde_json::json!({ "code": 1, "removed": removed })).into_response()
}

pub async fn ocs_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<OcsConfigQuery>,
) -> Response {
    let server = &state.config.server;
    // The generated config embeds the access token, so it is only shown to token holders.
    if !check_token(&state, &headers, q.token.as_deref()) {
        return error_response(&AppError::Unauthorized, None);
    }
    let base = if server.public_url.trim().is_empty() {
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .unwrap_or_else(|| format!("127.0.0.1:{}", server.port));
        let scheme = headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("http");
        format!("{scheme}://{host}")
    } else {
        server.public_url.trim().to_string()
    };
    let method = match q.method.as_deref() {
        Some("get" | "GET") => "get",
        _ => "post",
    };
    let token = (!server.access_token.is_empty()).then_some(server.access_token.as_str());
    let value: Value = answerer_wrapper(&base, method, token);
    Json(value).into_response()
}

pub async fn index() -> Response {
    Json(serde_json::json!({
        "service": "ocs-ai-answerer",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": {
            "answer": "POST|GET /api/answer",
            "health": "GET /health  (?check=true 检测上游模型)",
            "cache_clear": "DELETE /api/cache",
            "cache_forget": "POST /api/cache/forget",
            "ocs_config": "GET /ocs-config  (?method=get 生成 GET 版本)"
        }
    }))
    .into_response()
}

pub async fn not_found() -> Response {
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(ErrorBody {
            code: 0,
            msg: "接口不存在，可用接口: /api/answer, /health, /ocs-config".into(),
            error_type: "not_found",
            question: None,
        }),
    )
        .into_response()
}
