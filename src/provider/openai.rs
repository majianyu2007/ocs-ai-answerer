use reqwest::header::{AUTHORIZATION, HeaderMap};
use serde_json::{Map, Value, json};

use super::{ChatRequest, ChatResponse, Protocol, header_value};
use crate::config::{AiConfig, ProviderKind};
use crate::error::ProviderError;

/// OpenAI Chat Completions (`POST /v1/chat/completions`) and compatible services.
pub struct OpenAiProtocol;

impl Protocol for OpenAiProtocol {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAi
    }

    fn auth_headers(&self, cfg: &AiConfig) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            header_value(&format!("Bearer {}", cfg.api_key.trim()), "API Key")?,
        );
        Ok(headers)
    }

    fn build_body(&self, cfg: &AiConfig, req: &ChatRequest) -> Map<String, Value> {
        let mut messages = Vec::with_capacity(2);
        if !req.system.is_empty() {
            messages.push(json!({ "role": "system", "content": req.system }));
        }
        messages.push(json!({ "role": "user", "content": req.user }));

        let mut body = Map::new();
        body.insert("model".into(), json!(cfg.model));
        body.insert("messages".into(), Value::Array(messages));
        body.insert("stream".into(), json!(false));
        if let Some(t) = cfg.temperature {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(m) = cfg.max_tokens {
            body.insert("max_tokens".into(), json!(m));
        }
        body
    }

    fn parse_response(&self, body: Value) -> Result<ChatResponse, ProviderError> {
        let choice = body
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .ok_or_else(|| {
                ProviderError::InvalidResponse(format!(
                    "响应缺少 choices 字段，请确认接口是否为 OpenAI Chat Completions 协议: {}",
                    crate::error::truncate(&body.to_string(), 200)
                ))
            })?;

        let finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        let message = choice.get("message");
        let content = message
            .and_then(|m| m.get("content"))
            .map(content_to_text)
            // Legacy completions-style gateways.
            .or_else(|| {
                choice
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();

        if content.trim().is_empty() {
            let refusal = message
                .and_then(|m| m.get("refusal"))
                .and_then(Value::as_str);
            let hint = match (refusal, finish_reason.as_deref()) {
                (Some(r), _) => format!("（模型拒绝回答: {r}）"),
                (_, Some("length")) => {
                    "（finish_reason=length，max_tokens 可能过小，推理模型需要更大的值）".into()
                }
                (_, Some(reason)) => format!("（finish_reason={reason}）"),
                _ => String::new(),
            };
            return Err(ProviderError::EmptyContent(hint));
        }

        Ok(ChatResponse {
            content,
            model: body
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            finish_reason,
        })
    }
}

/// `content` is usually a string, but some gateways return an array of content parts.
fn content_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p {
                Value::String(s) => Some(s.as_str()),
                _ => p.get("text").and_then(Value::as_str),
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}
