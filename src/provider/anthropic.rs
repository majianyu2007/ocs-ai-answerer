use reqwest::header::{HeaderMap, HeaderName};
use serde_json::{Map, Value, json};

use super::{ChatRequest, ChatResponse, ImageInput, Protocol, Role, header_value};
use crate::config::{AiConfig, ProviderKind};
use crate::error::ProviderError;

/// Anthropic requires `max_tokens`; used when the config leaves it unset.
pub const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Anthropic Messages (`POST /v1/messages`) and compatible services.
pub struct AnthropicProtocol;

impl Protocol for AnthropicProtocol {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn auth_headers(&self, cfg: &AiConfig) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-api-key"),
            header_value(cfg.api_key.trim(), "API Key")?,
        );
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            header_value(cfg.anthropic_version.trim(), "anthropic_version")?,
        );
        Ok(headers)
    }

    fn build_body(&self, cfg: &AiConfig, req: &ChatRequest) -> Map<String, Value> {
        let mut body = Map::new();
        body.insert("model".into(), json!(cfg.model));
        body.insert(
            "max_tokens".into(),
            json!(cfg.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
        );
        if !req.system.is_empty() {
            body.insert("system".into(), json!(req.system));
        }
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                // Images first, as Anthropic recommends.
                let mut content: Vec<Value> = m
                    .images
                    .iter()
                    .map(|img| match img {
                        ImageInput::Base64 { media_type, data } => json!({
                            "type": "image",
                            "source": { "type": "base64", "media_type": media_type, "data": data }
                        }),
                        ImageInput::Url(url) => json!({
                            "type": "image",
                            "source": { "type": "url", "url": url }
                        }),
                    })
                    .collect();
                content.push(json!({ "type": "text", "text": m.text }));
                json!({ "role": role, "content": content })
            })
            .collect();
        body.insert("messages".into(), Value::Array(messages));
        if let Some(t) = cfg.temperature {
            // Anthropic accepts 0..=1; clamp so a shared OpenAI-style config keeps working.
            body.insert("temperature".into(), json!(t.clamp(0.0, 1.0)));
        }
        body
    }

    fn parse_response(&self, body: Value) -> Result<ChatResponse, ProviderError> {
        let blocks = body
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ProviderError::InvalidResponse(format!(
                    "响应缺少 content 数组，请确认接口是否为 Anthropic Messages 协议: {}",
                    crate::error::truncate(&body.to_string(), 200)
                ))
            })?;

        // Only `text` blocks carry the answer; `thinking`/`tool_use` etc. are skipped.
        let content: String = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("");

        let stop_reason = body
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_string);

        if content.trim().is_empty() {
            let hint = match stop_reason.as_deref() {
                Some("max_tokens") => {
                    "（stop_reason=max_tokens，max_tokens 可能过小，开启 thinking 时需要更大的值）"
                        .to_string()
                }
                Some("refusal") => "（模型拒绝回答）".to_string(),
                Some(reason) => format!("（stop_reason={reason}）"),
                None => String::new(),
            };
            return Err(ProviderError::EmptyContent(hint));
        }

        Ok(ChatResponse {
            content,
            model: body
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            finish_reason: stop_reason,
        })
    }
}
