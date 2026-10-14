mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use common::ai_config;
use http_body_util::BodyExt;
use ocs_ai_answerer::config::{AiConfig, Config, ProviderKind};
use ocs_ai_answerer::{api, build_state};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request as WireRequest, Respond, ResponseTemplate};

const Q: &str = "下面哪种语言主要用于系统编程？";
const OPTS: &str = "A. HTML\nB. Rust\nC. CSS\nD. SQL";

fn openai_reply(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "model": "m",
        "choices": [{ "message": { "content": content }, "finish_reason": "stop" }]
    }))
}

fn app(config: Config) -> Router {
    config.validate().expect("valid config");
    api::router(build_state(config).unwrap())
}

fn base_config(uri: &str) -> Config {
    let mut c = Config {
        ai: ai_config(ProviderKind::OpenAi, uri),
        ..Config::default()
    };
    c.cache.enabled = false;
    c
}

async fn ask(app: &Router, body: Value) -> Value {
    let req = Request::post("/api/answer")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn single() -> Value {
    json!({ "title": Q, "type": "single", "options": OPTS })
}

/// Replies with the given contents in order, then repeats the last one.
struct Sequence {
    replies: Vec<String>,
    calls: Arc<AtomicUsize>,
}

impl Respond for Sequence {
    fn respond(&self, _: &WireRequest) -> ResponseTemplate {
        let i = self.calls.fetch_add(1, Ordering::SeqCst);
        openai_reply(&self.replies[i.min(self.replies.len() - 1)])
    }
}

async fn sequence(server: &MockServer, replies: &[&str]) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .respond_with(Sequence {
            replies: replies.iter().map(|s| s.to_string()).collect(),
            calls: calls.clone(),
        })
        .mount(server)
        .await;
    calls
}

// ---------- cache ----------

#[tokio::test]
async fn cache_avoids_second_model_call() {
    let server = MockServer::start().await;
    let calls = sequence(&server, &["{\"answer\":\"B\"}"]).await;
    let mut cfg = base_config(&server.uri());
    cfg.cache.enabled = true;
    let app = app(cfg);

    let first = ask(&app, single()).await;
    assert_eq!(first["answer"], "Rust");
    assert_eq!(first["cached"], false);

    // Same question with different label formatting hits the cache.
    let second = ask(
        &app,
        json!({ "title": Q, "type": "single", "options": "A、HTML\nB、Rust\nC、CSS\nD、SQL" }),
    )
    .await;
    assert_eq!(second["answer"], "Rust");
    assert_eq!(second["cached"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let mut forced = single();
    forced["no_cache"] = json!(true);
    let third = ask(&app, forced).await;
    assert_eq!(third["cached"], false);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cache_persists_to_file_and_reloads() {
    let server = MockServer::start().await;
    let calls = sequence(&server, &["{\"answer\":\"B\"}"]).await;
    let dir = std::env::temp_dir().join(format!("ocs-cache-test-{}", std::process::id()));
    let file = dir.join("cache.jsonl");
    let _ = std::fs::remove_file(&file);

    let mut cfg = base_config(&server.uri());
    cfg.cache.enabled = true;
    cfg.cache.file = file.to_string_lossy().into_owned();
    ask(&app(cfg.clone()), single()).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // A fresh process (new state) loads the file.
    let reloaded = app(cfg);
    let body = ask(&reloaded, single()).await;
    assert_eq!(body["cached"], true);
    assert_eq!(body["answer"], "Rust");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let req = Request::delete("/api/cache").body(Body::empty()).unwrap();
    let resp = reloaded.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["cleared"], 1);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn failed_answers_are_not_cached() {
    let server = MockServer::start().await;
    let calls = sequence(&server, &["我不知道", "还是不知道", "{\"answer\":\"B\"}"]).await;
    let mut cfg = base_config(&server.uri());
    cfg.cache.enabled = true;
    cfg.answer.self_repair_attempts = 1;
    let app = app(cfg);
    assert_eq!(ask(&app, single()).await["code"], 0);
    assert_eq!(ask(&app, single()).await["answer"], "Rust");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn cache_forget_removes_one_question() {
    let server = MockServer::start().await;
    let calls = sequence(&server, &["{\"answer\":\"A\"}", "{\"answer\":\"B\"}"]).await;
    let mut cfg = base_config(&server.uri());
    cfg.cache.enabled = true;
    let app = app(cfg);
    assert_eq!(ask(&app, single()).await["answer"], "HTML");

    let req = Request::post("/api/cache/forget")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(single().to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["removed"], true);

    assert_eq!(ask(&app, single()).await["answer"], "Rust");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn concurrent_identical_questions_share_one_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            openai_reply("{\"answer\":\"B\"}").set_delay(std::time::Duration::from_millis(300)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let app = app(base_config(&server.uri()));
    let (a, b, c) = tokio::join!(
        ask(&app, single()),
        ask(&app, single()),
        ask(&app, single())
    );
    for r in [a, b, c] {
        assert_eq!(r["answer"], "Rust");
    }
}

// ---------- self-repair ----------

#[tokio::test]
async fn unparseable_reply_is_repaired_in_conversation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("无法被程序解析"))
        .respond_with(openai_reply("{\"answer\":\"B\"}"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(openai_reply("这道题我认为应该选系统编程语言那个。"))
        .expect(1)
        .mount(&server)
        .await;
    let app = app(base_config(&server.uri()));
    let body = ask(&app, single()).await;
    assert_eq!(body["code"], 1, "{body}");
    assert_eq!(body["answer"], "Rust");

    let requests = server.received_requests().await.unwrap();
    let repair: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let messages = repair["messages"].as_array().unwrap();
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[3]["role"], "user");
}

#[tokio::test]
async fn repair_can_be_disabled() {
    let server = MockServer::start().await;
    let calls = sequence(&server, &["随便说说"]).await;
    let mut cfg = base_config(&server.uri());
    cfg.answer.self_repair_attempts = 0;
    let body = ask(&app(cfg), single()).await;
    assert_eq!(body["error_type"], "answer_unrecognized");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// ---------- images ----------

#[tokio::test]
async fn images_are_sent_to_openai_and_anthropic() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("\"image_url\""))
        .and(body_string_contains("data:image/png;base64,iVBORw0KGgo"))
        .respond_with(openai_reply("{\"answer\":\"B\"}"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_string_contains("\"type\":\"base64\""))
        .and(body_string_contains("\"media_type\":\"image/png\""))
        .and(body_string_contains("\"data\":\"iVBORw0KGgo\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "{\"answer\":\"B\"}" }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let body = json!({
        "title": "如图 https://x/a.png，结果是？",
        "type": "single",
        "options": "A. 1\nB. 2",
        "images": ["data:image/png;base64,iVBORw0KGgo"],
        "suggestion_title": "如图 [图片1]，结果是？",
        "suggestion_options": ""
    });
    assert_eq!(
        ask(&app(base_config(&server.uri())), body.clone()).await["answer"],
        "2"
    );
    let anthropic = Config {
        ai: ai_config(ProviderKind::Anthropic, &server.uri()),
        ..base_config(&server.uri())
    };
    assert_eq!(ask(&app(anthropic), body).await["answer"], "2");
}

#[tokio::test]
async fn text_only_model_falls_back_without_images() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("image_url"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "message": "image input is not supported" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(openai_reply("{\"answer\":\"B\"}"))
        .expect(2)
        .mount(&server)
        .await;
    let app = app(base_config(&server.uri()));
    let body = |t: &str| {
        json!({
            "title": t, "type": "single", "options": "A. 1\nB. 2",
            "images": ["data:image/png;base64,AAAA"]
        })
    };
    assert_eq!(ask(&app, body("q1")).await["answer"], "2");
    // Model is now known to be text-only: no second rejected attempt.
    assert_eq!(ask(&app, body("q2")).await["answer"], "2");
}

#[tokio::test]
async fn vision_disabled_never_sends_images() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("image_url"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(openai_reply("{\"answer\":\"A\"}"))
        .mount(&server)
        .await;
    let mut cfg = base_config(&server.uri());
    cfg.ai.vision = false;
    let body = json!({
        "title": "t", "type": "single", "options": "A. 1\nB. 2",
        "images": ["data:image/png;base64,AAAA"]
    });
    assert_eq!(ask(&app(cfg), body).await["answer"], "1");
}

// ---------- ensemble ----------

fn voter(uri: &str, name: &str) -> AiConfig {
    AiConfig {
        name: name.into(),
        endpoint: format!("{uri}/{name}"),
        ..ai_config(ProviderKind::OpenAi, uri)
    }
}

#[tokio::test]
async fn ensemble_early_stop_when_unanimous() {
    let server = MockServer::start().await;
    for name in ["v1", "v2"] {
        Mock::given(method("POST"))
            .and(path(format!("/{name}")))
            .respond_with(openai_reply("{\"answer\":\"B\"}"))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(openai_reply("{\"answer\":\"B\"}"))
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = base_config(&server.uri());
    cfg.ensemble.enabled = true;
    cfg.ensemble.samples = 3;
    cfg.ensemble.models = vec![voter(&server.uri(), "v1"), voter(&server.uri(), "v2")];
    let body = ask(&app(cfg), single()).await;
    assert_eq!(body["answer"], "Rust");
    assert_eq!(body["confidence"], 1.0);
    assert_eq!(body["votes"], 3);
}

#[tokio::test]
async fn ensemble_majority_wins() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1"))
        .respond_with(openai_reply("{\"answer\":\"A\"}"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v2"))
        .respond_with(openai_reply("{\"answer\":\"B\"}"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(openai_reply("{\"answer\":\"B\"}"))
        .mount(&server)
        .await;
    let mut cfg = base_config(&server.uri());
    cfg.ensemble.enabled = true;
    cfg.ensemble.samples = 1;
    cfg.ensemble.min_agreement = 0.5;
    cfg.ensemble.models = vec![voter(&server.uri(), "v1"), voter(&server.uri(), "v2")];
    let body = ask(&app(cfg), single()).await;
    assert_eq!(body["answer"], "Rust");
    let conf = body["confidence"].as_f64().unwrap();
    assert!((conf - 2.0 / 3.0).abs() < 1e-9, "{conf}");
}

#[tokio::test]
async fn ensemble_judge_resolves_disagreement() {
    let server = MockServer::start().await;
    for (name, ans) in [("v1", "A"), ("v2", "C")] {
        Mock::given(method("POST"))
            .and(path(format!("/{name}")))
            .respond_with(openai_reply(&format!("{{\"answer\":\"{ans}\"}}")))
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(openai_reply("{\"answer\":\"D\"}"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/judge"))
        .and(body_string_contains("候选答案"))
        .respond_with(openai_reply("分析……\n{\"answer\":\"B\"}"))
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = base_config(&server.uri());
    cfg.ensemble.enabled = true;
    cfg.ensemble.samples = 1;
    cfg.ensemble.models = vec![voter(&server.uri(), "v1"), voter(&server.uri(), "v2")];
    cfg.ensemble.judge = Some(voter(&server.uri(), "judge"));
    let body = ask(&app(cfg), single()).await;
    assert_eq!(body["answer"], "Rust");
    assert_eq!(body["confidence"], 0.0);
}

#[tokio::test]
async fn ensemble_survives_failing_voter() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(openai_reply("{\"answer\":\"B\"}"))
        .mount(&server)
        .await;
    let mut cfg = base_config(&server.uri());
    cfg.ensemble.enabled = true;
    cfg.ensemble.samples = 1;
    cfg.ensemble.models = vec![voter(&server.uri(), "v1")];
    let body = ask(&app(cfg), single()).await;
    assert_eq!(body["code"], 1, "{body}");
    assert_eq!(body["answer"], "Rust");
}

#[tokio::test]
async fn extra_samples_use_higher_temperature() {
    let server = MockServer::start().await;
    let calls = sequence(
        &server,
        &[
            "{\"answer\":\"A\"}",
            "{\"answer\":\"B\"}",
            "{\"answer\":\"B\"}",
        ],
    )
    .await;
    let mut cfg = base_config(&server.uri());
    cfg.ensemble.enabled = true;
    cfg.ensemble.samples = 3;
    cfg.ensemble.models = vec![voter(&server.uri(), "v1")];
    let _ = ask(&app(cfg), single()).await;
    // Round 1: two voters disagree -> 4 more samples.
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    let temps: Vec<f64> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| {
            serde_json::from_slice::<Value>(&r.body).unwrap()["temperature"]
                .as_f64()
                .unwrap()
        })
        .collect();
    assert_eq!(temps.iter().filter(|t| **t == 0.7).count(), 4, "{temps:?}");
}

// ---------- concurrency limit ----------

#[tokio::test]
async fn max_concurrency_queues_requests() {
    use std::time::{Duration, Instant};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(openai_reply("{\"answer\":\"B\"}").set_delay(Duration::from_millis(200)))
        .mount(&server)
        .await;
    let mut cfg = base_config(&server.uri());
    cfg.ai.max_concurrency = 1;
    let app = app(cfg);
    let started = Instant::now();
    let q = |t: &str| json!({ "title": t, "type": "single", "options": OPTS });
    let _ = tokio::join!(ask(&app, q("q1")), ask(&app, q("q2")), ask(&app, q("q3")));
    assert!(
        started.elapsed() >= Duration::from_millis(600),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn retry_after_header_is_respected() {
    use std::time::{Duration, Instant};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(openai_reply("{\"answer\":\"B\"}"))
        .mount(&server)
        .await;
    let mut cfg = base_config(&server.uri());
    cfg.ai.max_retries = 1;
    let started = Instant::now();
    assert_eq!(ask(&app(cfg), single()).await["answer"], "Rust");
    assert!(started.elapsed() >= Duration::from_millis(950));
}

#[tokio::test]
async fn slow_voter_does_not_lose_fast_votes() {
    use std::time::Duration;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/slow"))
        .respond_with(openai_reply("{\"answer\":\"A\"}").set_delay(Duration::from_secs(10)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(openai_reply("{\"answer\":\"B\"}"))
        .mount(&server)
        .await;
    let mut cfg = base_config(&server.uri());
    cfg.server.request_timeout_secs = 4;
    cfg.ensemble.enabled = true;
    cfg.ensemble.samples = 1;
    cfg.ensemble.models = vec![AiConfig {
        timeout_secs: 30,
        ..voter(&server.uri(), "slow")
    }];
    let started = std::time::Instant::now();
    let body = ask(&app(cfg), single()).await;
    assert_eq!(body["code"], 1, "{body}");
    assert_eq!(body["answer"], "Rust");
    assert!(started.elapsed() < Duration::from_secs(4));
}
