mod common;

use std::time::Duration;

use common::ai_config;
use ocs_ai_answerer::config::{AiConfig, ProviderKind};
use ocs_ai_answerer::error::ProviderError;
use ocs_ai_answerer::provider::{AiClient, ChatRequest};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn req() -> ChatRequest {
    ChatRequest {
        system: "sys".into(),
        user: "hello".into(),
    }
}

fn openai_ok(content: &str) -> Value {
    json!({
        "id": "chatcmpl-1",
        "model": "upstream-model",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": content }, "finish_reason": "stop" }]
    })
}

fn anthropic_ok(text: &str) -> Value {
    json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-test",
        "content": [
            { "type": "thinking", "thinking": "hmm", "signature": "x" },
            { "type": "text", "text": text }
        ],
        "stop_reason": "end_turn"
    })
}

#[tokio::test]
async fn openai_request_format() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .and(header("content-type", "application/json"))
        .and(header("x-custom", "yes"))
        .and(body_partial_json(json!({
            "model": "test-model",
            "temperature": 0.1,
            "max_tokens": 256,
            "stream": false,
            "top_p": 0.5,
            "messages": [
                { "role": "system", "content": "sys" },
                { "role": "user", "content": "hello" }
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_ok("{\"answer\":\"B\"}")))
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = ai_config(ProviderKind::OpenAi, &format!("{}/v1", server.uri()));
    cfg.headers.insert("X-Custom".into(), "yes".into());
    cfg.extra_body
        .insert("top_p".into(), toml::Value::Float(0.5));
    let client = AiClient::new(&cfg).unwrap();
    let resp = client.complete(&req()).await.unwrap();
    assert_eq!(resp.content, "{\"answer\":\"B\"}");
    assert_eq!(resp.model.as_deref(), Some("upstream-model"));
}

#[tokio::test]
async fn openai_omits_unset_optional_params() {
    let cfg = AiConfig {
        temperature: None,
        max_tokens: None,
        ..ai_config(ProviderKind::OpenAi, "http://localhost/v1")
    };
    let body = AiClient::new(&cfg).unwrap().request_body(&req());
    assert!(body.get("temperature").is_none());
    assert!(body.get("max_tokens").is_none());
}

#[tokio::test]
async fn openai_content_parts_array() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": [{ "type": "text", "text": "A" }, { "type": "text", "text": "B" }] } }]
        })))
        .mount(&server)
        .await;
    let client = AiClient::new(&ai_config(ProviderKind::OpenAi, &server.uri())).unwrap();
    assert_eq!(client.complete(&req()).await.unwrap().content, "AB");
}

#[tokio::test]
async fn anthropic_request_format() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(body_partial_json(json!({
            "model": "test-model",
            "max_tokens": 256,
            "system": "sys",
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hello" }] }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_ok("{\"answer\":true}")))
        .expect(1)
        .mount(&server)
        .await;

    let client = AiClient::new(&ai_config(ProviderKind::Anthropic, &server.uri())).unwrap();
    let resp = client.complete(&req()).await.unwrap();
    assert_eq!(resp.content, "{\"answer\":true}");
    assert_eq!(resp.finish_reason.as_deref(), Some("end_turn"));
}

#[tokio::test]
async fn anthropic_requires_max_tokens_and_clamps_temperature() {
    let cfg = AiConfig {
        max_tokens: None,
        temperature: Some(1.5),
        ..ai_config(ProviderKind::Anthropic, "http://localhost")
    };
    let body = AiClient::new(&cfg).unwrap().request_body(&req());
    assert_eq!(body["max_tokens"], 1024);
    assert_eq!(body["temperature"], 1.0);
    assert!(
        AiClient::new(&cfg)
            .unwrap()
            .request_body(&req())
            .get("stream")
            .is_none()
    );
}

#[tokio::test]
async fn custom_full_endpoint_is_used_verbatim() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/custom/proxy/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_ok("ok")))
        .expect(1)
        .mount(&server)
        .await;
    let cfg = AiConfig {
        endpoint: format!("{}/custom/proxy/chat", server.uri()),
        ..ai_config(ProviderKind::OpenAi, "https://ignored.example.com/v1")
    };
    let client = AiClient::new(&cfg).unwrap();
    assert_eq!(client.complete(&req()).await.unwrap().content, "ok");
}

#[test]
fn endpoint_resolution() {
    let mk = |kind, base: &str| ai_config(kind, base).resolved_endpoint();
    assert_eq!(
        mk(ProviderKind::OpenAi, "https://api.deepseek.com/v1/"),
        "https://api.deepseek.com/v1/chat/completions"
    );
    assert_eq!(
        mk(ProviderKind::OpenAi, "https://api.deepseek.com"),
        "https://api.deepseek.com/v1/chat/completions"
    );
    assert_eq!(
        mk(ProviderKind::OpenAi, "https://openrouter.ai/api/v1"),
        "https://openrouter.ai/api/v1/chat/completions"
    );
    assert_eq!(
        mk(ProviderKind::Anthropic, "https://api.anthropic.com"),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(
        mk(
            ProviderKind::Anthropic,
            "https://gw.example.com/anthropic/v1"
        ),
        "https://gw.example.com/anthropic/v1/messages"
    );
    assert_eq!(
        mk(ProviderKind::Anthropic, ""),
        "https://api.anthropic.com/v1/messages"
    );
}

#[tokio::test]
async fn openai_error_401_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": "Incorrect API key provided", "type": "invalid_request_error" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let cfg = AiConfig {
        max_retries: 3,
        ..ai_config(ProviderKind::OpenAi, &server.uri())
    };
    let err = AiClient::new(&cfg)
        .unwrap()
        .complete(&req())
        .await
        .unwrap_err();
    match err {
        ProviderError::Unauthorized { status, message } => {
            assert_eq!(status, 401);
            assert!(message.contains("Incorrect API key"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn anthropic_error_body_is_parsed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "error",
            "error": { "type": "invalid_request_error", "message": "max_tokens: field required" }
        })))
        .mount(&server)
        .await;
    let err = AiClient::new(&ai_config(ProviderKind::Anthropic, &server.uri()))
        .unwrap()
        .complete(&req())
        .await
        .unwrap_err();
    match err {
        ProviderError::Api { status, message } => {
            assert_eq!(status, 400);
            assert_eq!(
                message,
                "[invalid_request_error] max_tokens: field required"
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn forbidden_maps_to_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&server)
        .await;
    let err = AiClient::new(&ai_config(ProviderKind::OpenAi, &server.uri()))
        .unwrap()
        .complete(&req())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ProviderError::Unauthorized { status: 403, .. }
    ));
}

#[tokio::test]
async fn retries_on_429_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_json(json!({ "error": { "message": "slow down" } })),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_ok("B")))
        .expect(1)
        .mount(&server)
        .await;
    let cfg = AiConfig {
        max_retries: 2,
        ..ai_config(ProviderKind::OpenAi, &server.uri())
    };
    let resp = AiClient::new(&cfg).unwrap().complete(&req()).await.unwrap();
    assert_eq!(resp.content, "B");
}

#[tokio::test]
async fn server_error_exhausts_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>Bad Gateway</html>"))
        .expect(2)
        .mount(&server)
        .await;
    let cfg = AiConfig {
        max_retries: 1,
        ..ai_config(ProviderKind::OpenAi, &server.uri())
    };
    let err = AiClient::new(&cfg)
        .unwrap()
        .complete(&req())
        .await
        .unwrap_err();
    assert!(
        matches!(err, ProviderError::Server { status: 502, .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn timeout_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(openai_ok("late"))
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&server)
        .await;
    let cfg = AiConfig {
        timeout_secs: 1,
        ..ai_config(ProviderKind::OpenAi, &server.uri())
    };
    let err = AiClient::new(&cfg)
        .unwrap()
        .complete(&req())
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::Timeout), "{err:?}");
}

#[tokio::test]
async fn connection_refused_is_network_error() {
    // Bind then drop a listener to get a port nothing listens on.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let cfg = ai_config(ProviderKind::OpenAi, &format!("http://127.0.0.1:{port}/v1"));
    let err = AiClient::new(&cfg)
        .unwrap()
        .complete(&req())
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::Connect(_)), "{err:?}");
}

#[tokio::test]
async fn non_json_response_is_invalid() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>login page</html>"))
        .mount(&server)
        .await;
    let err = AiClient::new(&ai_config(ProviderKind::OpenAi, &server.uri()))
        .unwrap()
        .complete(&req())
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::InvalidResponse(_)), "{err:?}");
}

#[tokio::test]
async fn error_object_with_http_200() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": { "message": "model not found", "code": "model_not_found" }
        })))
        .mount(&server)
        .await;
    let err = AiClient::new(&ai_config(ProviderKind::OpenAi, &server.uri()))
        .unwrap()
        .complete(&req())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ProviderError::Api { message, .. } if message.contains("model not found")),
        "{err:?}"
    );
}

#[tokio::test]
async fn missing_choices_is_invalid() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "foo": 1 })))
        .mount(&server)
        .await;
    let err = AiClient::new(&ai_config(ProviderKind::OpenAi, &server.uri()))
        .unwrap()
        .complete(&req())
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::InvalidResponse(_)), "{err:?}");
}

#[tokio::test]
async fn empty_content_reports_length_hint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "" }, "finish_reason": "length" }]
        })))
        .mount(&server)
        .await;
    let err = AiClient::new(&ai_config(ProviderKind::OpenAi, &server.uri()))
        .unwrap()
        .complete(&req())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ProviderError::EmptyContent(h) if h.contains("max_tokens")),
        "{err:?}"
    );
}

#[tokio::test]
async fn anthropic_empty_text_blocks() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "thinking", "thinking": "..." }],
            "stop_reason": "max_tokens"
        })))
        .mount(&server)
        .await;
    let err = AiClient::new(&ai_config(ProviderKind::Anthropic, &server.uri()))
        .unwrap()
        .complete(&req())
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::EmptyContent(_)), "{err:?}");
}
