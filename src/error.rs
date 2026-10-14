use std::path::PathBuf;

use thiserror::Error;

/// Errors raised while loading or validating the configuration file.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("配置文件不存在: {0}（可复制 config.example.toml 为 config.toml 后修改）")]
    NotFound(PathBuf),
    #[error("无法读取配置文件 {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("配置文件 {path} 格式错误: {source}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("环境变量 {name} 的值无效: {reason}")]
    Env { name: String, reason: String },
    #[error("配置校验失败:\n  - {}", .0.join("\n  - "))]
    Invalid(Vec<String>),
}

/// Errors returned by an AI provider call.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("初始化 HTTP 客户端失败: {0}")]
    Build(String),
    #[error("无法连接 AI 接口（DNS/网络/TLS 错误）: {0}")]
    Connect(String),
    #[error("AI 接口请求超时")]
    Timeout,
    #[error("AI 接口认证失败 (HTTP {status})，请检查 API Key 与权限: {message}")]
    Unauthorized { status: u16, message: String },
    #[error("AI 接口触发限流 (HTTP 429): {message}")]
    RateLimited {
        message: String,
        retry_after: Option<u64>,
    },
    #[error("AI 上游服务异常 (HTTP {status}): {message}")]
    Server { status: u16, message: String },
    #[error("AI 接口返回错误 (HTTP {status}): {message}")]
    Api { status: u16, message: String },
    #[error("AI 接口响应格式异常: {0}")]
    InvalidResponse(String),
    #[error("模型返回了空内容{0}")]
    EmptyContent(String),
}

impl ProviderError {
    /// Only transient failures are worth retrying; auth/validation errors would fail again.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Connect(_) | Self::Timeout | Self::RateLimited { .. } | Self::Server { .. }
        )
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Build(_) => "provider_build",
            Self::Connect(_) => "provider_connect",
            Self::Timeout => "provider_timeout",
            Self::Unauthorized { .. } => "provider_unauthorized",
            Self::RateLimited { .. } => "provider_rate_limited",
            Self::Server { .. } => "provider_server_error",
            Self::Api { .. } => "provider_api_error",
            Self::InvalidResponse(_) => "provider_invalid_response",
            Self::EmptyContent(_) => "provider_empty_content",
        }
    }
}

/// Errors raised while turning raw model output into an OCS answer.
#[derive(Debug, Error)]
pub enum AnswerError {
    #[error("模型输出为空")]
    EmptyOutput,
    #[error("无法从模型输出中识别答案（{reason}）: {raw}")]
    Unrecognized { reason: String, raw: String },
}

impl AnswerError {
    pub fn unrecognized(reason: impl Into<String>, raw: &str) -> Self {
        Self::Unrecognized {
            reason: reason.into(),
            raw: truncate(raw, 200),
        }
    }
}

/// Top level error for one answer request.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("请求参数错误: {0}")]
    BadRequest(String),
    #[error("访问令牌无效或缺失")]
    Unauthorized,
    #[error("答题处理超时（超过 {0} 秒）")]
    Timeout(u64),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Answer(#[from] AnswerError),
}

impl AppError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::Timeout(_) => "timeout",
            Self::Provider(e) => e.kind(),
            Self::Answer(AnswerError::EmptyOutput) => "answer_empty",
            Self::Answer(AnswerError::Unrecognized { .. }) => "answer_unrecognized",
        }
    }
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}
