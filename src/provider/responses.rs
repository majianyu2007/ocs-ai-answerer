use reqwest::header::{AUTHORIZATION, HeaderMap};
use serde_json::{Map, Value, json};

use super::{ChatRequest, ChatResponse, Protocol, Role, header_value};
use crate::config::{AiConfig, ProviderKind};
use crate::error::ProviderError;

/// OpenAI Responses (`POST /v1/responses`) and compatible services.
pub struct OpenAiResponsesProtocol;

impl Protocol for OpenAiResponsesProtocol {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAiResponses
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
        let input = req
            .messages
            .iter()
            .map(|message| {
                let (role, text_type, image_count) = match message.role {
                    Role::User => ("user", "input_text", message.images.len()),
                    Role::Assistant => ("assistant", "output_text", 0),
                };
                let mut content = Vec::with_capacity(1 + image_count);
                content.push(json!({ "type": text_type, "text": message.text }));
                if message.role == Role::User {
                    content.extend(message.images.iter().map(|image| {
                        let mut part = Map::new();
                        part.insert("type".into(), Value::from("input_image"));
                        part.insert("image_url".into(), Value::String(image.to_data_url()));
                        Value::Object(part)
                    }));
                }
                let mut item = Map::new();
                item.insert("type".into(), Value::from("message"));
                item.insert("role".into(), Value::from(role));
                item.insert("content".into(), Value::Array(content));
                Value::Object(item)
            })
            .collect();

        let mut body = Map::new();
        body.insert("model".into(), json!(cfg.model));
        if !req.system.is_empty() {
            body.insert("instructions".into(), json!(req.system));
        }
        body.insert("input".into(), Value::Array(input));
        body.insert("stream".into(), Value::Bool(false));
        body.insert("store".into(), Value::Bool(false));
        if let Some(temperature) = cfg.temperature {
            body.insert("temperature".into(), json!(temperature));
        }
        if let Some(max_tokens) = cfg.max_tokens {
            body.insert("max_output_tokens".into(), json!(max_tokens));
        }
        body
    }

    fn parse_response(&self, body: Value) -> Result<ChatResponse, ProviderError> {
        let Value::Object(mut body) = body else {
            return Err(ProviderError::InvalidResponse(
                "Responses 响应必须为对象".into(),
            ));
        };
        if body.get("status").and_then(Value::as_str) != Some("completed") {
            return Err(ProviderError::InvalidResponse(
                "Responses 响应未完成或缺少有效 status，不能使用部分输出".into(),
            ));
        }
        if ["error", "incomplete_details"]
            .iter()
            .any(|key| body.get(*key).is_some_and(|value| !value.is_null()))
        {
            return Err(ProviderError::InvalidResponse(
                "Responses 响应包含错误或未完成信息，不能使用输出".into(),
            ));
        }
        let Some(Value::Array(output)) = body.remove("output") else {
            return Err(ProviderError::InvalidResponse(
                "Responses 响应缺少有效 output 数组".into(),
            ));
        };

        let mut content = String::new();
        for item in output {
            let Value::Object(mut item) = item else {
                return Err(ProviderError::InvalidResponse(
                    "Responses output 项必须为对象".into(),
                ));
            };
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {}
                // Reasoning and tool output must never become the answer.
                Some(_) => continue,
                None => {
                    return Err(ProviderError::InvalidResponse(
                        "Responses output 项缺少有效 type".into(),
                    ));
                }
            }
            match item.get("role").and_then(Value::as_str) {
                Some("assistant") => {}
                Some(_) => continue,
                None => {
                    return Err(ProviderError::InvalidResponse(
                        "Responses message 缺少有效 role".into(),
                    ));
                }
            }
            if item.get("status").and_then(Value::as_str) != Some("completed") {
                return Err(ProviderError::InvalidResponse(
                    "Responses assistant message 未完成或缺少有效 status".into(),
                ));
            }
            if item.get("phase").and_then(Value::as_str) == Some("commentary") {
                continue;
            }
            let Some(Value::Array(parts)) = item.remove("content") else {
                return Err(ProviderError::InvalidResponse(
                    "Responses assistant message 缺少有效 content 数组".into(),
                ));
            };
            for part in parts {
                let Value::Object(mut part) = part else {
                    return Err(ProviderError::InvalidResponse(
                        "Responses content 项必须为对象".into(),
                    ));
                };
                match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => {
                        let Some(Value::String(text)) = part.remove("text") else {
                            return Err(ProviderError::InvalidResponse(
                                "Responses output_text 缺少有效 text 字符串".into(),
                            ));
                        };
                        if content.is_empty() {
                            content = text;
                        } else {
                            content.push_str(&text);
                        }
                    }
                    Some("refusal") => {
                        return Err(ProviderError::EmptyContent("（模型拒绝回答）".into()));
                    }
                    Some(_) => {}
                    None => {
                        return Err(ProviderError::InvalidResponse(
                            "Responses content 项缺少有效 type".into(),
                        ));
                    }
                }
            }
        }
        if content.trim().is_empty() {
            return Err(ProviderError::EmptyContent(
                "（Responses 响应没有 assistant output_text）".into(),
            ));
        }
        let model = match body.remove("model") {
            Some(Value::String(model)) => Some(model),
            None | Some(Value::Null) => None,
            Some(_) => {
                return Err(ProviderError::InvalidResponse(
                    "Responses model 必须为字符串".into(),
                ));
            }
        };
        Ok(ChatResponse {
            content,
            model,
            finish_reason: Some("completed".into()),
        })
    }
}
