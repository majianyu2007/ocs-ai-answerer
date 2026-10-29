use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

pub const DEFAULT_CONFIG_PATH: &str = "config.toml";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub ai: AiConfig,
    pub answer: AnswerConfig,
    pub cache: CacheConfig,
    pub ensemble: EnsembleConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// tracing filter, e.g. "info" or "ocs_ai_answerer=debug,info".
    pub log_level: String,
    /// Upper bound for answering one question, including retries. Keep it below OCS's 60s.
    pub request_timeout_secs: u64,
    /// When non-empty, requests must carry this token (body/query `token` or `Authorization: Bearer`).
    pub access_token: String,
    /// Externally reachable base URL used by `/ocs-config`; empty means derive it from the Host header.
    pub public_url: String,
    pub cors: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 3000,
            log_level: "info".into(),
            request_timeout_secs: 55,
            access_token: String::new(),
            public_url: String::new(),
            cors: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[serde(alias = "openai-compatible", alias = "openai_compatible")]
    OpenAi,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[serde(
        alias = "anthropic-compatible",
        alias = "anthropic_compatible",
        alias = "claude"
    )]
    Anthropic,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenAiResponses => "openai-responses",
            Self::Anthropic => "anthropic",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProviderKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "openai" | "openai-compatible" | "openai_compatible" => Ok(Self::OpenAi),
            "openai-responses" => Ok(Self::OpenAiResponses),
            "anthropic" | "anthropic-compatible" | "anthropic_compatible" | "claude" => {
                Ok(Self::Anthropic)
            }
            other => Err(format!(
                "不支持的 provider \"{other}\"，可选值: openai, openai-responses, anthropic"
            )),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiConfig {
    pub provider: ProviderKind,
    /// API root, e.g. `https://api.openai.com/v1` or `https://api.anthropic.com`.
    pub base_url: String,
    /// Full request URL; overrides the path derived from `base_url` when non-empty.
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
    /// Omitted from the request when unset (some reasoning models reject it).
    pub temperature: Option<f64>,
    /// Omitted for OpenAI when unset; Responses maps it to `max_output_tokens`.
    /// Anthropic requires it and falls back to 1024.
    pub max_tokens: Option<u32>,
    pub timeout_secs: u64,
    pub connect_timeout_secs: u64,
    /// Extra attempts after the first one, only for network errors, timeouts, 429 and 5xx.
    pub max_retries: u32,
    pub retry_backoff_ms: u64,
    pub anthropic_version: String,
    pub headers: BTreeMap<String, String>,
    /// Merged into the top level of the JSON request body, overriding generated fields.
    pub extra_body: toml::Table,
    /// Send question images to the model (requires a vision-capable model).
    pub vision: bool,
    /// Maximum simultaneous upstream requests for this model; 0 = unlimited.
    pub max_concurrency: usize,
    /// Optional display name used in logs and voting results.
    pub name: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: ProviderKind::OpenAi,
            base_url: String::new(),
            endpoint: String::new(),
            api_key: String::new(),
            model: String::new(),
            temperature: None,
            max_tokens: Some(2048),
            timeout_secs: 40,
            connect_timeout_secs: 10,
            max_retries: 1,
            retry_backoff_ms: 800,
            anthropic_version: "2023-06-01".into(),
            headers: BTreeMap::new(),
            extra_body: toml::Table::new(),
            vision: true,
            max_concurrency: 8,
            name: String::new(),
        }
    }
}

impl AiConfig {
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            format!("{}:{}", self.provider, self.model)
        } else {
            self.name.trim().to_string()
        }
    }

    fn validate_into(&self, prefix: &str, errors: &mut Vec<String>) {
        if self.api_key.trim().is_empty() {
            errors.push(format!(
                "{prefix}.api_key 未配置（可在 config.toml 中填写，主模型也可设置环境变量 OCS_AI_API_KEY）"
            ));
        }
        if self.model.trim().is_empty() {
            errors.push(format!(
                "{prefix}.model 未配置（例如 gpt-4o-mini、deepseek-chat、claude-sonnet-4-5）"
            ));
        }
        if !self.base_url.trim().is_empty() {
            check_http_url(&format!("{prefix}.base_url"), &self.base_url, errors);
        }
        if !self.endpoint.trim().is_empty() {
            check_http_url(&format!("{prefix}.endpoint"), &self.endpoint, errors);
        }
        if let Some(t) = self.temperature
            && !(0.0..=2.0).contains(&t)
        {
            errors.push(format!(
                "{prefix}.temperature 必须在 0 ~ 2 之间，当前为 {t}"
            ));
        }
        if self.max_tokens == Some(0) {
            errors.push(format!("{prefix}.max_tokens 必须大于 0"));
        }
        if self.timeout_secs == 0 {
            errors.push(format!("{prefix}.timeout_secs 必须大于 0"));
        }
        if self.connect_timeout_secs == 0 {
            errors.push(format!("{prefix}.connect_timeout_secs 必须大于 0"));
        }
        if self.max_retries > 5 {
            errors.push(format!("{prefix}.max_retries 不应超过 5"));
        }
        for (name, value) in &self.headers {
            if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
                errors.push(format!("{prefix}.headers 中的请求头名称无效: {name}"));
            }
            if reqwest::header::HeaderValue::from_str(value).is_err() {
                errors.push(format!("{prefix}.headers.{name} 的值包含非法字符"));
            }
        }
        if reqwest::header::HeaderValue::from_str(self.api_key.trim()).is_err() {
            errors.push(format!(
                "{prefix}.api_key 包含非法字符（请检查是否有换行或中文字符）"
            ));
        }
    }
    pub fn default_base_url(&self) -> &'static str {
        match self.provider {
            ProviderKind::OpenAi | ProviderKind::OpenAiResponses => "https://api.openai.com/v1",
            ProviderKind::Anthropic => "https://api.anthropic.com",
        }
    }

    /// Resolves the final request URL.
    ///
    /// OpenAI-style base URLs conventionally include the version (`.../v1`), so
    /// `/chat/completions` or `/responses` is appended, with `/v1` added for bare hosts.
    /// Anthropic-style base URLs conventionally omit it, so `/v1/messages` is appended
    /// unless the base already ends in `/v1`.
    pub fn resolved_endpoint(&self) -> String {
        if !self.endpoint.trim().is_empty() {
            return self.endpoint.trim().to_string();
        }
        let base = if self.base_url.trim().is_empty() {
            self.default_base_url()
        } else {
            self.base_url.trim()
        };
        let base = base.trim_end_matches('/');
        let has_path = Url::parse(base)
            .map(|u| !u.path().trim_matches('/').is_empty())
            .unwrap_or(false);
        match self.provider {
            ProviderKind::OpenAi if has_path => format!("{base}/chat/completions"),
            ProviderKind::OpenAi => format!("{base}/v1/chat/completions"),
            ProviderKind::OpenAiResponses if has_path => format!("{base}/responses"),
            ProviderKind::OpenAiResponses => format!("{base}/v1/responses"),
            ProviderKind::Anthropic if base.ends_with("/v1") => format!("{base}/messages"),
            ProviderKind::Anthropic => format!("{base}/v1/messages"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ChoiceFormat {
    /// Option text, falling back to letters when the text cannot be matched safely by OCS.
    #[default]
    Auto,
    Text,
    Letter,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnswerConfig {
    /// Empty values fall back to the built-in prompts.
    pub system_prompt: String,
    pub single_prompt: String,
    pub multiple_prompt: String,
    pub judgement_prompt: String,
    pub completion_prompt: String,
    pub unknown_prompt: String,
    /// Ask the model for a short explanation and return it in the response metadata.
    pub include_explanation: bool,
    pub choice_format: ChoiceFormat,
    pub multiple_separator: String,
    pub completion_separator: String,
    pub judgement_true: String,
    pub judgement_false: String,
    /// When the model's reply cannot be parsed, send it back once with a correction request.
    pub self_repair_attempts: u32,
    /// Ask the model to reason briefly before the final JSON. Improves accuracy of non-reasoning
    /// models at the cost of more output tokens; turn off for reasoning models.
    pub step_by_step: bool,
}

impl Default for AnswerConfig {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            single_prompt: String::new(),
            multiple_prompt: String::new(),
            judgement_prompt: String::new(),
            completion_prompt: String::new(),
            unknown_prompt: String::new(),
            include_explanation: false,
            choice_format: ChoiceFormat::Auto,
            multiple_separator: "#".into(),
            completion_separator: "#".into(),
            judgement_true: "正确".into(),
            judgement_false: "错误".into(),
            self_repair_attempts: 1,
            step_by_step: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    pub enabled: bool,
    /// Maximum number of cached answers kept in memory.
    pub capacity: usize,
    /// 0 = never expire.
    pub ttl_secs: u64,
    /// JSON Lines file used to persist answers across restarts; empty = memory only.
    pub file: String,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            capacity: 20_000,
            ttl_secs: 0,
            file: String::new(),
        }
    }
}

/// Self-consistency voting across several samples and/or models.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EnsembleConfig {
    pub enabled: bool,
    /// Samples requested from each voter (main model + `models`).
    pub samples: u32,
    /// Additional voters besides `[ai]`; each entry has the same fields as `[ai]`.
    pub models: Vec<AiConfig>,
    /// Asked to decide when votes disagree; falls back to the plurality answer when absent.
    pub judge: Option<AiConfig>,
    /// Share of votes the leading answer needs to skip the judge, e.g. 0.6.
    pub min_agreement: f64,
    /// Stop sampling early once the first responses all agree.
    pub early_stop: bool,
}

impl Default for EnsembleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            samples: 3,
            models: Vec::new(),
            judge: None,
            min_agreement: 0.6,
            early_stop: true,
        }
    }
}

/// Where the configuration came from, for startup logging.
#[derive(Debug, Clone)]
pub enum ConfigSource {
    File(PathBuf),
    DefaultsAndEnv,
}

impl Config {
    /// Loads the TOML file, applies environment overrides and validates the result.
    ///
    /// A missing file is only tolerated for the default path so that container deployments
    /// can be configured purely through environment variables.
    pub fn load(path: &Path, explicit: bool) -> Result<(Self, ConfigSource), ConfigError> {
        let (mut config, source) = if path.exists() {
            let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?;
            (
                Self::from_toml(&text, path)?,
                ConfigSource::File(path.to_path_buf()),
            )
        } else if explicit {
            return Err(ConfigError::NotFound(path.to_path_buf()));
        } else {
            (Self::default(), ConfigSource::DefaultsAndEnv)
        };
        config.apply_env(|k| std::env::var(k).ok())?;
        config.validate()?;
        Ok((config, source))
    }

    pub fn from_toml(text: &str, path: &Path) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(e),
        })
    }

    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) -> Result<(), ConfigError> {
        let get = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        fn parse<T: FromStr>(name: &str, v: String) -> Result<T, ConfigError>
        where
            T::Err: fmt::Display,
        {
            v.trim().parse().map_err(|e: T::Err| ConfigError::Env {
                name: name.into(),
                reason: e.to_string(),
            })
        }

        if let Some(v) = get("OCS_SERVER_HOST") {
            self.server.host = v;
        }
        if let Some(v) = get("OCS_SERVER_PORT") {
            self.server.port = parse("OCS_SERVER_PORT", v)?;
        }
        if let Some(v) = get("OCS_LOG_LEVEL") {
            self.server.log_level = v;
        }
        if let Some(v) = get("OCS_ACCESS_TOKEN") {
            self.server.access_token = v;
        }
        if let Some(v) = get("OCS_PUBLIC_URL") {
            self.server.public_url = v;
        }
        if let Some(v) = get("OCS_AI_PROVIDER") {
            self.ai.provider = parse("OCS_AI_PROVIDER", v)?;
        }
        if let Some(v) = get("OCS_AI_BASE_URL") {
            self.ai.base_url = v;
        }
        if let Some(v) = get("OCS_AI_ENDPOINT") {
            self.ai.endpoint = v;
        }
        if let Some(v) = get("OCS_AI_API_KEY") {
            self.ai.api_key = v;
        }
        if let Some(v) = get("OCS_AI_MODEL") {
            self.ai.model = v;
        }
        if let Some(v) = get("OCS_AI_TEMPERATURE") {
            self.ai.temperature = Some(parse("OCS_AI_TEMPERATURE", v)?);
        }
        if let Some(v) = get("OCS_AI_MAX_TOKENS") {
            self.ai.max_tokens = Some(parse("OCS_AI_MAX_TOKENS", v)?);
        }
        if let Some(v) = get("OCS_AI_TIMEOUT_SECS") {
            self.ai.timeout_secs = parse("OCS_AI_TIMEOUT_SECS", v)?;
        }
        if let Some(v) = get("OCS_AI_MAX_RETRIES") {
            self.ai.max_retries = parse("OCS_AI_MAX_RETRIES", v)?;
        }
        if let Some(v) = get("OCS_AI_VISION") {
            self.ai.vision = parse_bool("OCS_AI_VISION", &v)?;
        }
        if let Some(v) = get("OCS_CACHE_ENABLED") {
            self.cache.enabled = parse_bool("OCS_CACHE_ENABLED", &v)?;
        }
        if let Some(v) = get("OCS_CACHE_FILE") {
            self.cache.file = v;
        }
        if let Some(v) = get("OCS_STEP_BY_STEP") {
            self.answer.step_by_step = parse_bool("OCS_STEP_BY_STEP", &v)?;
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut errors = Vec::new();
        self.ai.validate_into("ai", &mut errors);
        for (i, m) in self.ensemble.models.iter().enumerate() {
            m.validate_into(&format!("ensemble.models[{i}]"), &mut errors);
        }
        if let Some(j) = &self.ensemble.judge {
            j.validate_into("ensemble.judge", &mut errors);
        }
        if self.ensemble.enabled && self.ensemble.samples == 0 {
            errors.push("ensemble.samples 必须大于 0".into());
        }
        if self.ensemble.samples > 9 {
            errors.push("ensemble.samples 不应超过 9".into());
        }
        if !(self.ensemble.min_agreement > 0.0 && self.ensemble.min_agreement <= 1.0) {
            errors.push("ensemble.min_agreement 必须在 (0, 1] 之间".into());
        }
        if self.answer.self_repair_attempts > 3 {
            errors.push("answer.self_repair_attempts 不应超过 3".into());
        }

        if self.server.request_timeout_secs == 0 {
            errors.push("server.request_timeout_secs 必须大于 0".into());
        }
        if self.server.host.trim().is_empty() {
            errors.push("server.host 不能为空".into());
        }
        if !self.server.public_url.trim().is_empty() {
            check_http_url("server.public_url", &self.server.public_url, &mut errors);
        }
        if tracing_subscriber::EnvFilter::try_new(&self.server.log_level).is_err() {
            errors.push(format!(
                "server.log_level 无效: {}（可选 trace/debug/info/warn/error）",
                self.server.log_level
            ));
        }

        let a = &self.answer;
        if a.multiple_separator.trim().is_empty() {
            errors.push("answer.multiple_separator 不能为空".into());
        }
        if a.completion_separator.trim().is_empty() {
            errors.push("answer.completion_separator 不能为空".into());
        }
        if a.judgement_true.trim().is_empty() || a.judgement_false.trim().is_empty() {
            errors.push("answer.judgement_true / answer.judgement_false 不能为空".into());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(errors))
        }
    }

    /// Non-fatal configuration smells worth surfacing at startup.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        let models = std::iter::once(("ai", &self.ai))
            .chain(self.ensemble.models.iter().map(|m| ("ensemble.models", m)))
            .chain(self.ensemble.judge.iter().map(|j| ("ensemble.judge", j)));
        for (label, m) in models {
            if m.timeout_secs >= self.server.request_timeout_secs {
                out.push(format!(
                    "{label}（{}）的 timeout_secs = {}s 不小于 server.request_timeout_secs = {}s，慢请求会先触发整体超时、无法重试",
                    m.display_name(),
                    m.timeout_secs,
                    self.server.request_timeout_secs
                ));
            }
        }
        if self.server.request_timeout_secs >= 60 {
            out.push(
                "server.request_timeout_secs ≥ 60s：OCS 默认 60 秒判定题库超时，建议设置为 55 以内"
                    .into(),
            );
        }
        if self.server.access_token.is_empty() && !is_loopback_host(&self.server.host) {
            out.push(format!(
                "服务监听在 {} 且未设置 server.access_token，任何能访问该端口的人都能消耗你的 API 额度",
                self.server.host
            ));
        }
        if self.ensemble.enabled {
            let voters = 1 + self.ensemble.models.len() as u32;
            out.push(format!(
                "已启用多模型投票：每道题最多请求 {} 次模型（{} 个模型 × {} 次采样{}），费用会相应增加",
                voters * self.ensemble.samples + u32::from(self.ensemble.judge.is_some()),
                voters,
                self.ensemble.samples,
                if self.ensemble.judge.is_some() { " + 裁判" } else { "" }
            ));
        }
        out
    }
}

fn parse_bool(name: &str, v: &str) -> Result<bool, ConfigError> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(ConfigError::Env {
            name: name.into(),
            reason: format!("无法识别的布尔值 \"{other}\"（可用 true/false）"),
        }),
    }
}

fn check_http_url(field: &str, value: &str, errors: &mut Vec<String>) {
    match Url::parse(value.trim()) {
        Ok(u) if (u.scheme() == "http" || u.scheme() == "https") && u.host().is_some() => {}
        Ok(_) => errors.push(format!(
            "{field} 必须是 http:// 或 https:// 开头的地址: {value}"
        )),
        Err(e) => errors.push(format!("{field} 不是合法的 URL（{e}）: {value}")),
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host.trim(), "127.0.0.1" | "localhost" | "::1")
}
