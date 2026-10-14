#![allow(dead_code)]

use ocs_ai_answerer::answer::Question;
use ocs_ai_answerer::config::{AiConfig, ProviderKind};

pub fn ai_config(provider: ProviderKind, base_url: &str) -> AiConfig {
    AiConfig {
        provider,
        base_url: base_url.to_string(),
        api_key: "sk-test".into(),
        model: "test-model".into(),
        max_tokens: Some(256),
        timeout_secs: 5,
        connect_timeout_secs: 2,
        max_retries: 0,
        retry_backoff_ms: 10,
        ..AiConfig::default()
    }
}

pub fn question(title: &str, qtype: &str, options: &str) -> Question {
    Question::new(title, qtype, vec![options.to_string()])
}
