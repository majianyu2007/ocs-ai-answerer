use serde::{Deserialize, Serialize};

/// `options` arrives as OCS's newline-joined string, but arrays are accepted too.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(untagged)]
pub enum OptionsInput {
    #[default]
    None,
    Text(String),
    List(Vec<String>),
}

impl OptionsInput {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            Self::None => Vec::new(),
            Self::Text(s) => vec![s],
            Self::List(v) => v,
        }
    }
}

/// Body of `POST /api/answer` and the query of `GET /api/answer`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AnswerRequest {
    #[serde(default, alias = "question")]
    pub title: Option<String>,
    #[serde(default, rename = "type")]
    pub qtype: Option<String>,
    #[serde(default)]
    pub options: OptionsInput,
    #[serde(default)]
    pub token: Option<String>,
}

/// Query form: every value is a plain string.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AnswerQuery {
    #[serde(default, alias = "question")]
    pub title: Option<String>,
    #[serde(default, rename = "type")]
    pub qtype: Option<String>,
    #[serde(default)]
    pub options: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

impl From<AnswerQuery> for AnswerRequest {
    fn from(q: AnswerQuery) -> Self {
        Self {
            title: q.title,
            qtype: q.qtype,
            options: q.options.map(OptionsInput::Text).unwrap_or_default(),
            token: q.token,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AnswerSuccess {
    pub code: u8,
    pub question: String,
    pub answer: String,
    #[serde(rename = "type")]
    pub qtype: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub model: String,
    pub provider: &'static str,
    pub elapsed_ms: u64,
    pub ai: bool,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: u8,
    pub msg: String,
    pub error_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HealthBody {
    pub status: &'static str,
    pub service: &'static str,
    pub version: &'static str,
    pub provider: &'static str,
    pub model: String,
    pub endpoint: String,
    pub uptime_secs: u64,
    pub upstream: UpstreamStatus,
}

#[derive(Debug, Serialize)]
pub struct UpstreamStatus {
    /// `unchecked` unless `?check=true` was requested; then `ok` or `error`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
}

#[derive(Debug, Default, Deserialize)]
pub struct HealthQuery {
    #[serde(default)]
    pub check: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct OcsConfigQuery {
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}
