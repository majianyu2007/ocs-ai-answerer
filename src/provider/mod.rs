//! AI provider abstraction.
//!
//! [`AiClient`] owns everything protocols have in common: the pooled HTTP client, endpoint,
//! custom headers, `extra_body` merging, HTTP status mapping and the retry loop.
//! Each wire protocol only implements [`Protocol`]: how to authenticate, how to build the
//! JSON body and how to read text out of a successful response.
//!
//! A per-client semaphore bounds concurrent upstream calls so bursts from OCS (or ensemble
//! fan-out) queue locally instead of tripping the provider's rate limits.

mod anthropic;
mod openai;
mod responses;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value};

pub use anthropic::AnthropicProtocol;
pub use openai::OpenAiProtocol;
pub use responses::OpenAiResponsesProtocol;

use crate::config::{AiConfig, ProviderKind};
use crate::error::{ProviderError, truncate};

/// An image attached to a user message, already decoded from a `data:` URL or as a remote URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageInput {
    Base64 { media_type: String, data: String },
    Url(String),
}

impl ImageInput {
    /// Accepts `data:image/png;base64,...` or an http(s) URL; anything else is ignored.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("data:") {
            let (meta, data) = rest.split_once(',')?;
            let media_type = meta.strip_suffix(";base64")?.split(';').next()?.trim();
            let media_type = if media_type.starts_with("image/") {
                media_type.to_string()
            } else {
                "image/png".to_string()
            };
            let data = data.trim();
            (!data.is_empty()).then(|| Self::Base64 {
                media_type,
                data: data.to_string(),
            })
        } else if s.starts_with("http://") || s.starts_with("https://") {
            Some(Self::Url(s.to_string()))
        } else {
            None
        }
    }

    pub fn to_data_url(&self) -> String {
        match self {
            Self::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
            Self::Url(u) => u.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub text: String,
    pub images: Vec<ImageInput>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
            images: Vec::new(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
            images: Vec::new(),
        }
    }
}

/// A provider-agnostic chat request.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    /// Overrides the configured temperature for this request (used for diverse voting samples).
    pub temperature: Option<f64>,
}

impl ChatRequest {
    pub fn single(system: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            messages: vec![Message::user(user)],
            temperature: None,
        }
    }

    pub fn has_images(&self) -> bool {
        self.messages.iter().any(|m| !m.images.is_empty())
    }

    pub fn without_images(&self) -> Self {
        Self {
            system: self.system.clone(),
            messages: self
                .messages
                .iter()
                .map(|m| Message {
                    images: Vec::new(),
                    ..m.clone()
                })
                .collect(),
            temperature: self.temperature,
        }
    }

    /// Text of the first user message (handy in logs and tests).
    pub fn user_text(&self) -> &str {
        self.messages
            .iter()
            .find(|m| m.role == Role::User)
            .map(|m| m.text.as_str())
            .unwrap_or("")
    }
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
        ProviderKind::OpenAiResponses => Box::new(OpenAiResponsesProtocol),
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
    name: String,
    permits: Option<Semaphore>,
    /// Set once the upstream rejects image input, so later questions skip it.
    vision_rejected: AtomicBool,
}

impl AiClient {
    /// Builds a client sharing an existing connection pool (used for ensemble voters).
    pub fn with_http(cfg: &AiConfig, http: reqwest::Client) -> Result<Self, ProviderError> {
        Self::build(cfg, Some(http))
    }

    pub fn new(cfg: &AiConfig) -> Result<Self, ProviderError> {
        Self::build(cfg, None)
    }

    fn build(cfg: &AiConfig, http: Option<reqwest::Client>) -> Result<Self, ProviderError> {
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

        let http = match http {
            Some(h) => h,
            None => build_http(cfg.connect_timeout_secs)?,
        };

        Ok(Self {
            inner: Arc::new(Inner {
                http,
                protocol,
                endpoint,
                headers,
                cfg: cfg.clone(),
                name: cfg.display_name(),
                permits: (cfg.max_concurrency > 0).then(|| Semaphore::new(cfg.max_concurrency)),
                vision_rejected: AtomicBool::new(false),
            }),
        })
    }

    pub fn kind(&self) -> ProviderKind {
        self.inner.protocol.kind()
    }

    pub fn model(&self) -> &str {
        &self.inner.cfg.model
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn http(&self) -> reqwest::Client {
        self.inner.http.clone()
    }

    /// Whether images should be attached for this model.
    pub fn accepts_images(&self) -> bool {
        self.inner.cfg.vision && !self.inner.vision_rejected.load(Ordering::Relaxed)
    }

    pub fn endpoint(&self) -> &reqwest::Url {
        &self.inner.endpoint
    }

    /// Builds the JSON body that would be sent for `req`, including `extra_body` overrides.
    pub fn request_body(&self, req: &ChatRequest) -> Value {
        let mut body = match req.temperature {
            // Only adjust when the model has a temperature configured (some reject the field).
            Some(t) if self.inner.cfg.temperature.is_some() => {
                let cfg = AiConfig {
                    temperature: Some(t),
                    ..self.inner.cfg.clone()
                };
                self.inner.protocol.build_body(&cfg, req)
            }
            _ => self.inner.protocol.build_body(&self.inner.cfg, req),
        };
        for (key, value) in &self.inner.cfg.extra_body {
            body.insert(key.clone(), toml_to_json(value));
        }
        Value::Object(body)
    }

    /// Sends the request, retrying transient failures up to `max_retries` times.
    ///
    /// When the request carries images and the upstream rejects them with a 4xx, the images are
    /// dropped and the request is retried once as text only; the model is then marked as text-only.
    pub async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let _permit = match &self.inner.permits {
            Some(sem) => Some(
                sem.acquire()
                    .await
                    .map_err(|_| ProviderError::Build("并发控制已关闭".into()))?,
            ),
            None => None,
        };
        if req.has_images() && !self.accepts_images() {
            return self.complete_inner(&req.without_images()).await;
        }
        match self.complete_inner(req).await {
            Err(ProviderError::Api { status, message })
                if req.has_images() && (400..500).contains(&status) && status != 429 =>
            {
                tracing::warn!(
                    model = %self.name(),
                    status,
                    error = %message,
                    "模型似乎不支持图片输入，已改为纯文本请求，并对该模型停用图片（可设置 vision = false 关闭此提示）"
                );
                self.inner.vision_rejected.store(true, Ordering::Relaxed);
                self.complete_inner(&req.without_images()).await
            }
            other => other,
        }
    }

    async fn complete_inner(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
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
                        model = %self.name(),
                        attempt = attempt + 1,
                        elapsed_ms,
                        finish_reason = resp.finish_reason.as_deref().unwrap_or("-"),
                        "上游模型响应成功"
                    );
                    return Ok(resp);
                }
                Err(err) if err.is_retryable() && attempt < max_retries => {
                    attempt += 1;
                    let backoff = match &err {
                        ProviderError::RateLimited {
                            retry_after: Some(secs),
                            ..
                        } => (*secs).min(20) * 1000,
                        _ => self.inner.cfg.retry_backoff_ms * u64::from(attempt),
                    };
                    tracing::warn!(
                        model = %self.name(),
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
                        model = %self.name(),
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
            .timeout(Duration::from_secs(inner.cfg.timeout_secs))
            .json(body)
            .send()
            .await
            .map_err(map_transport_error)?;

        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let text = response.text().await.map_err(map_transport_error)?;

        if !status.is_success() {
            let mut err = map_status_error(status.as_u16(), &text);
            if let ProviderError::RateLimited { retry_after: r, .. } = &mut err {
                *r = retry_after;
            }
            return Err(err);
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
        429 => ProviderError::RateLimited {
            message,
            retry_after: None,
        },
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

/// One pooled HTTP client; per-request timeouts come from each model's config.
pub fn build_http(connect_timeout_secs: u64) -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .user_agent(concat!("ocs-ai-answerer/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(connect_timeout_secs))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(60))
        .build()
        .map_err(|e| ProviderError::Build(e.to_string()))
}

fn header_value(value: &str, what: &str) -> Result<HeaderValue, ProviderError> {
    HeaderValue::from_str(value).map_err(|_| ProviderError::Build(format!("{what} 包含非法字符")))
}
