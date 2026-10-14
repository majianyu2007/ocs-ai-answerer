//! AI provider abstraction.
//!
//! [`AiClient`] owns everything protocols have in common: the pooled HTTP client, endpoint,
//! custom headers, `extra_body` merging, HTTP status mapping and the retry loop.
//! Each wire protocol only implements [`Protocol`]: how to authenticate, how to build the
//! JSON body and how to read text out of a successful response.

mod anthropic;
mod openai;

use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value};

pub use anthropic::AnthropicProtocol;
pub use openai::OpenAiProtocol;

use crate::config::{AiConfig, ProviderKind};
use crate::error::{ProviderError, truncate};

/// A provider-agnostic single-turn chat request.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub system: String,
    pub user: String,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    /// Model name reported by the upstream, if any.
    pub model: Option<String>,
    pub finish_reason: Option<String>,
}

pub trait Protocol: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn auth_headers(&self, cfg: &AiConfig) -> Result<HeaderMap, ProviderError>;
    fn build_body(&self, cfg: &AiConfig, req: &ChatRequest) -> Map<String, Value>;
    fn parse_response(&self, body: Value) -> Result<ChatResponse, ProviderError>;
}

pub fn protocol_for(kind: ProviderKind) -> Box<dyn Protocol> {
    match kind {
        ProviderKind::OpenAi => Box::new(OpenAiProtocol),
        ProviderKind::Anthropic => Box::new(AnthropicProtocol),
    }
}

#[derive(Clone)]
pub struct AiClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    protocol: Box<dyn Protocol>,
    endpoint: reqwest::Url,
    headers: HeaderMap,
    cfg: AiConfig,
}

impl AiClient {
    pub fn new(cfg: &AiConfig) -> Result<Self, ProviderError> {
        let protocol = protocol_for(cfg.provider);
        let endpoint_str = cfg.resolved_endpoint();
        let endpoint = reqwest::Url::parse(&endpoint_str)
            .map_err(|e| ProviderError::Build(format!("API 地址无效 {endpoint_str}: {e}")))?;

        let mut headers = protocol.auth_headers(cfg)?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        for (name, value) in &cfg.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| ProviderError::Build(format!("请求头名称无效 {name}: {e}")))?;
            let value = HeaderValue::from_str(value)
                .map_err(|e| ProviderError::Build(format!("请求头 {name} 的值无效: {e}")))?;
            headers.insert(name, value);
        }

        let http = reqwest::Client::builder()
            .user_agent(concat!("ocs-ai-answerer/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(cfg.connect_timeout_secs))
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .map_err(|e| ProviderError::Build(e.to_string()))?;

        Ok(Self {
            inner: Arc::new(Inner {
                http,
                protocol,
                endpoint,
                headers,
                cfg: cfg.clone(),
            }),
        })
    }

    pub fn kind(&self) -> ProviderKind {
        self.inner.protocol.kind()
    }

    pub fn model(&self) -> &str {
        &self.inner.cfg.model
    }

    pub fn endpoint(&self) -> &reqwest::Url {
        &self.inner.endpoint
    }

    /// Builds the JSON body that would be sent for `req`, including `extra_body` overrides.
    pub fn request_body(&self, req: &ChatRequest) -> Value {
        let mut body = self.inner.protocol.build_body(&self.inner.cfg, req);
        for (key, value) in &self.inner.cfg.extra_body {
            body.insert(key.clone(), toml_to_json(value));
        }
        Value::Object(body)
    }

    /// Sends the request, retrying transient failures up to `max_retries` times.
    pub async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = self.request_body(req);
        let max_retries = self.inner.cfg.max_retries;
        let mut attempt = 0;
        loop {
            let started = Instant::now();
            let result = self.send_once(&body).await;
            let elapsed_ms = started.elapsed().as_millis() as u64;
            match result {
                Ok(resp) => {
                    tracing::debug!(
                        provider = %self.kind(),
                        attempt = attempt + 1,
                        elapsed_ms,
                        finish_reason = resp.finish_reason.as_deref().unwrap_or("-"),
                        "上游模型响应成功"
                    );
                    return Ok(resp);
                }
                Err(err) if err.is_retryable() && attempt < max_retries => {
                    attempt += 1;
                    let backoff = self.inner.cfg.retry_backoff_ms * u64::from(attempt);
                    tracing::warn!(
                        provider = %self.kind(),
                        attempt,
                        max_retries,
                        elapsed_ms,
                        backoff_ms = backoff,
                        error = %err,
                        "上游请求失败，准备重试"
                    );
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                }
                Err(err) => {
                    tracing::warn!(
                        provider = %self.kind(),
                        attempts = attempt + 1,
                        elapsed_ms,
                        kind = err.kind(),
                        error = %err,
                        "上游请求失败"
                    );
                    return Err(err);
                }
            }
        }
    }

    async fn send_once(&self, body: &Value) -> Result<ChatResponse, ProviderError> {
        let inner = &self.inner;
        let response = inner
            .http
            .post(inner.endpoint.clone())
            .headers(inner.headers.clone())
            .json(body)
            .send()
            .await
            .map_err(map_transport_error)?;

        let status = response.status();
        let text = response.text().await.map_err(map_transport_error)?;

        if !status.is_success() {
            return Err(map_status_error(status.as_u16(), &text));
        }

        let json: Value = serde_json::from_str(&text).map_err(|e| {
            ProviderError::InvalidResponse(format!(
                "响应不是合法 JSON（{e}），请确认 API 地址是否正确: {}",
                truncate(&text, 200)
            ))
        })?;

        // Some gateways answer HTTP 200 with an error object.
        if let Some(message) = error_message_from_json(&json) {
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message,
            });
        }

        inner.protocol.parse_response(json)
    }
}

fn map_transport_error(err: reqwest::Error) -> ProviderError {
    if err.is_timeout() {
        ProviderError::Timeout
    } else if err.is_connect() || err.is_request() {
        ProviderError::Connect(error_chain(&err))
    } else if err.is_decode() || err.is_body() {
        ProviderError::InvalidResponse(error_chain(&err))
    } else {
        ProviderError::Connect(error_chain(&err))
    }
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

pub(crate) fn map_status_error(status: u16, body: &str) -> ProviderError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| error_message_from_json(&v))
        .unwrap_or_else(|| {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                "(空响应)".to_string()
            } else {
                truncate(trimmed, 300)
            }
        });
    match status {
        401 | 403 => ProviderError::Unauthorized { status, message },
        408 => ProviderError::Timeout,
        429 => ProviderError::RateLimited { message },
        500..=599 => ProviderError::Server { status, message },
        _ => ProviderError::Api { status, message },
    }
}

/// Extracts an error message from both OpenAI (`{"error":{"message"}}`) and
/// Anthropic (`{"type":"error","error":{"type","message"}}`) error shapes.
pub(crate) fn error_message_from_json(v: &Value) -> Option<String> {
    let err = v.get("error")?;
    if err.is_null() {
        return None;
    }
    if let Some(s) = err.as_str() {
        return Some(s.to_string());
    }
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("未知错误");
    match err
        .get("type")
        .or_else(|| err.get("code"))
        .and_then(Value::as_str)
    {
        Some(t) if !t.is_empty() => Some(format!("[{t}] {message}")),
        _ => Some(message.to_string()),
    }
}

fn toml_to_json(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

fn header_value(value: &str, what: &str) -> Result<HeaderValue, ProviderError> {
    HeaderValue::from_str(value).map_err(|_| ProviderError::Build(format!("{what} 包含非法字符")))
}
