mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::ai_config;
use http_body_util::BodyExt;
use ocs_ai_answerer::config::{Config, ProviderKind};
use ocs_ai_answerer::{api, build_state};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn app(server_uri: &str, provider: ProviderKind, tweak: impl FnOnce(&mut Config)) -> Router {
    let mut config = Config {
        ai: ai_config(provider, server_uri),
        ..Config::default()
    };
    tweak(&mut config);
    config.validate().expect("valid test config");
    api::router(build_state(config).unwrap())
}

async fn mock_openai(server: &MockServer, content: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "upstream-model",
            "choices": [{ "message": { "role": "assistant", "content": content }, "finish_reason": "stop" }]
        })))
        .mount(server)
        .await;
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| panic!("non-JSON body: {}", String::from_utf8_lossy(&bytes)));
    (status, json)
}

fn post_json(uri: &str, body: Value) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn post_single_choice() {
    let server = MockServer::start().await;
    mock_openai(&server, "```json\n{\"answer\": \"B\"}\n```").await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});

    let (status, body) = send(
        &app,
        post_json(
            "/api/answer",
            json!({
                "title": "下面哪种语言主要用于系统编程？",
                "type": "single",
                "options": "A. HTML\nB. Rust\nC. CSS\nD. SQL"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 1);
    assert_eq!(body["question"], "下面哪种语言主要用于系统编程？");
    assert_eq!(body["answer"], "Rust");
    assert_eq!(body["type"], "single");
    assert_eq!(body["provider"], "openai");
    assert_eq!(body["model"], "upstream-model");
    assert_eq!(body["ai"], true);
}

#[tokio::test]
async fn post_multiple_with_options_array() {
    let server = MockServer::start().await;
    mock_openai(&server, "{\"answer\": [\"A\", \"C\", \"D\"]}").await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});

    let (_, body) = send(
        &app,
        post_json(
            "/api/answer",
            json!({
                "title": "以下哪些是编程语言？",
                "type": "multiple",
                "options": ["A. Rust", "B. HTML", "C. Go", "D. Python"]
            }),
        ),
    )
    .await;
    assert_eq!(body["code"], 1);
    assert_eq!(body["answer"], "Rust#Go#Python");
}

#[tokio::test]
async fn get_judgement_shares_logic() {
    let server = MockServer::start().await;
    mock_openai(&server, "{\"answer\": false}").await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});

    let (_, body) = send(
        &app,
        get("/api/answer?title=%E5%9C%B0%E7%90%83%E6%98%AF%E5%B9%B3%E7%9A%84&type=judgement&options=A.%20%E5%AF%B9%0AB.%20%E9%94%99"),
    )
    .await;
    assert_eq!(body["code"], 1, "{body}");
    assert_eq!(body["question"], "地球是平的");
    let answer = body["answer"].as_str().unwrap();
    let options = vec!["A. 对".to_string(), "B. 错".to_string()];
    assert_eq!(
        ocs_ai_answerer::answer::ocs::resolve_judgement(answer, &options),
        Some(1),
        "{answer}"
    );
}

#[tokio::test]
async fn completion_via_anthropic() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_string_contains("填空题"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "claude-test",
            "content": [{ "type": "text", "text": "{\"answer\": [\"北京\", \"上海\"]}" }],
            "stop_reason": "end_turn"
        })))
        .mount(&server)
        .await;
    let app = app(&server.uri(), ProviderKind::Anthropic, |_| {});

    let (_, body) = send(
        &app,
        post_json(
            "/api/answer",
            json!({ "title": "中国首都是____，最大城市是____", "type": "completion", "options": "" }),
        ),
    )
    .await;
    assert_eq!(body["code"], 1, "{body}");
    assert_eq!(body["answer"], "北京#上海");
    assert_eq!(body["provider"], "anthropic");
}

#[tokio::test]
async fn missing_title_returns_code_0() {
    let server = MockServer::start().await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});

    let (status, body) = send(&app, post_json("/api/answer", json!({ "type": "single" }))).await;
    // HTTP 200 so OCS's GM_xmlhttpRequest still hands the body to the handler.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 0);
    assert_eq!(body["error_type"], "bad_request");
    assert!(body["msg"].as_str().unwrap().contains("title"));

    let (_, body) = send(&app, get("/api/answer")).await;
    assert_eq!(body["code"], 0);
}

#[tokio::test]
async fn malformed_json_body() {
    let server = MockServer::start().await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});
    let req = Request::post("/api/answer")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let (_, body) = send(&app, req).await;
    assert_eq!(body["code"], 0);
    assert_eq!(body["error_type"], "bad_request");
}

#[tokio::test]
async fn upstream_error_returns_code_0() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({ "error": { "message": "bad key" } })),
        )
        .mount(&server)
        .await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});

    let (status, body) = send(
        &app,
        post_json(
            "/api/answer",
            json!({ "title": "q", "type": "single", "options": "A. x\nB. y" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 0);
    assert_eq!(body["error_type"], "provider_unauthorized");
    assert!(body["msg"].as_str().unwrap().contains("bad key"));
    assert_eq!(body["question"], "q");
}

#[tokio::test]
async fn unparseable_model_output_returns_code_0() {
    let server = MockServer::start().await;
    mock_openai(&server, "I am not sure about this one.").await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});

    let (_, body) = send(
        &app,
        post_json(
            "/api/answer",
            json!({ "title": "q", "type": "single", "options": "A. x\nB. y" }),
        ),
    )
    .await;
    assert_eq!(body["code"], 0);
    assert_eq!(body["error_type"], "answer_unrecognized");
}

#[tokio::test]
async fn request_timeout_is_enforced() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "choices": [{ "message": { "content": "B" } }] }))
                .set_delay(std::time::Duration::from_secs(3)),
        )
        .mount(&server)
        .await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |c| {
        c.server.request_timeout_secs = 1;
    });
    let (_, body) = send(
        &app,
        post_json(
            "/api/answer",
            json!({ "title": "q", "type": "single", "options": "A. x\nB. y" }),
        ),
    )
    .await;
    assert_eq!(body["code"], 0);
    assert_eq!(body["error_type"], "timeout");
}

#[tokio::test]
async fn access_token_is_checked() {
    let server = MockServer::start().await;
    mock_openai(&server, "{\"answer\": \"A\"}").await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |c| {
        c.server.access_token = "secret".into();
    });
    let q = json!({ "title": "q", "type": "single", "options": "A. x\nB. y" });

    let (_, body) = send(&app, post_json("/api/answer", q.clone())).await;
    assert_eq!(body["error_type"], "unauthorized");

    let mut with_token = q.clone();
    with_token["token"] = json!("secret");
    let (_, body) = send(&app, post_json("/api/answer", with_token)).await;
    assert_eq!(body["code"], 1, "{body}");

    let req = Request::post("/api/answer")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::from(q.to_string()))
        .unwrap();
    let (_, body) = send(&app, req).await;
    assert_eq!(body["code"], 1);

    let (_, body) = send(&app, get("/ocs-config")).await;
    assert_eq!(body["code"], 0);
    let (_, body) = send(&app, get("/ocs-config?token=secret")).await;
    assert_eq!(body[0]["data"]["token"], "secret");
}

#[tokio::test]
async fn health_does_not_claim_upstream_ok_without_check() {
    let server = MockServer::start().await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});
    let (status, body) = send(&app, get("/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["provider"], "openai");
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["upstream"]["status"], "unchecked");
    // No request was made to the model.
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn health_check_probes_upstream() {
    let server = MockServer::start().await;
    mock_openai(&server, "ok").await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});
    let (_, body) = send(&app, get("/health?check=true")).await;
    assert_eq!(body["upstream"]["status"], "ok");

    let bad = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&bad)
        .await;
    let app = app_with(&bad.uri());
    let (status, body) = send(&app, get("/health?check=1")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["upstream"]["status"], "error");
}

fn app_with(uri: &str) -> Router {
    app(uri, ProviderKind::OpenAi, |_| {})
}

#[tokio::test]
async fn ocs_config_is_valid_answerer_wrapper() {
    let server = MockServer::start().await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});
    let (_, body) = send(&app, get("/ocs-config")).await;
    let w = &body[0];
    assert_eq!(w["url"], "http://localhost:3000/api/answer");
    assert_eq!(w["method"], "post");
    assert_eq!(w["type"], "GM_xmlhttpRequest");
    assert_eq!(w["contentType"], "json");
    assert_eq!(w["data"]["title"], "${title}");
    assert_eq!(w["data"]["options"], "${options}");
    assert_eq!(w["data"]["type"], "${type}");
    assert!(w["handler"].as_str().unwrap().starts_with("return (res)"));
    assert!(w["data"].get("token").is_none());

    let app = app_with_public(&server.uri());
    let (_, body) = send(&app, get("/ocs-config?method=get")).await;
    assert_eq!(body[0]["url"], "https://answer.example.com/api/answer");
    assert_eq!(body[0]["method"], "get");
}

fn app_with_public(uri: &str) -> Router {
    app(uri, ProviderKind::OpenAi, |c| {
        c.server.public_url = "https://answer.example.com/".into();
    })
}

#[tokio::test]
async fn shipped_ocs_config_matches_generator() {
    let shipped: Value = serde_json::from_str(include_str!("../ocs_config.json"))
        .expect("ocs_config.json is valid JSON");
    let generated =
        ocs_ai_answerer::api::ocs_config::answerer_wrapper("http://127.0.0.1:3000", "post", None);
    assert_eq!(shipped, generated);
}

#[tokio::test]
async fn unknown_route_is_json_404() {
    let server = MockServer::start().await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});
    let (status, body) = send(&app, get("/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], 0);
}

#[tokio::test]
async fn concurrent_requests_are_independent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("Q-ONE"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(
                    json!({ "choices": [{ "message": { "content": "{\"answer\":\"A\"}" } }] }),
                )
                .set_delay(std::time::Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("Q-TWO"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({ "choices": [{ "message": { "content": "{\"answer\":\"B\"}" } }] }),
        ))
        .mount(&server)
        .await;
    let app = app(&server.uri(), ProviderKind::OpenAi, |_| {});
    let opts = "A. alpha\nB. beta";
    let (a, b) = tokio::join!(
        send(
            &app,
            post_json(
                "/api/answer",
                json!({ "title": "Q-ONE", "type": "single", "options": opts })
            )
        ),
        send(
            &app,
            post_json(
                "/api/answer",
                json!({ "title": "Q-TWO", "type": "single", "options": opts })
            )
        ),
    );
    assert_eq!(a.1["answer"], "alpha");
    assert_eq!(b.1["answer"], "beta");
}
