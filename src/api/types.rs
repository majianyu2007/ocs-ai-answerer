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
    /// Base64 data URLs (or http URLs) from OCS image optimization (`env.images`).
    #[serde(default)]
    pub images: OptionsInput,
    #[serde(default)]
    pub suggestion_title: Option<String>,
    #[serde(default)]
    pub suggestion_options: Option<String>,
    /// Skip the cache lookup and ask the model again.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub no_cache: bool,
}

/// Accepts `true`, `"true"`, `"1"`, `1`.
fn lenient_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Bool(b) => b,
        serde_json::Value::Number(n) => n.as_i64().is_some_and(|n| n != 0),
        serde_json::Value::String(s) => matches!(s.trim(), "1" | "true" | "yes"),
        _ => false,
    })
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
    #[serde(default)]
    pub no_cache: Option<String>,
}

impl From<AnswerQuery> for AnswerRequest {
    fn from(q: AnswerQuery) -> Self {
        Self {
            title: q.title,
            qtype: q.qtype,
            options: q.options.map(OptionsInput::Text).unwrap_or_default(),
            token: q.token,
            no_cache: matches!(
                q.no_cache.as_deref().map(str::trim),
                Some("1" | "true" | "yes")
            ),
            ..Default::default()
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
    pub provider: String,
    pub elapsed_ms: u64,
    pub ai: bool,
    pub cached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub votes: Option<usize>,
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
    pub ensemble: EnsembleStatus,
    pub cache: CacheStatus,
}

#[derive(Debug, Serialize)]
pub struct EnsembleStatus {
    pub enabled: bool,
    pub models: Vec<ModelStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge: Option<ModelStatus>,
}

#[derive(Debug, Serialize)]
pub struct ModelStatus {
    pub name: String,
    pub provider: &'static str,
    pub model: String,
    pub endpoint: String,
    pub vision: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<UpstreamStatus>,
}

#[derive(Debug, Serialize)]
pub struct CacheStatus {
    pub enabled: bool,
    pub entries: usize,
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

#[derive(Debug, Default, Deserialize)]
pub struct TokenQuery {
    #[serde(default)]
    pub token: Option<String>,
}
