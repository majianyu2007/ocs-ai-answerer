use serde_json::{Value, json};

pub const SERVICE_NAME: &str = "OCS AI Answerer";

/// Parses the JSON returned by `/api/answer`. On failure the message goes in the question slot so
/// OCS shows it in the search result panel instead of a silent "no answer".
pub const OCS_HANDLER: &str = "return (res) => res && res.code === 1 ? [res.question, res.answer, { ai: true }] : [res && res.msg ? res.msg : 'AI 题库请求失败', undefined]";

/// Builds the AnswererWrapper array that users paste into OCS.
pub fn answerer_wrapper(base_url: &str, method: &str, token: Option<&str>) -> Value {
    let url = format!("{}/api/answer", base_url.trim_end_matches('/'));
    let mut data = json!({
        "title": "${title}",
        "type": "${type}",
        "options": "${options}",
    });
    // Image questions (OCS「图片题优化」) can only be uploaded via POST. A `${images}` placeholder
    // would stringify the array, so a field handler passes the array through unchanged.
    if method == "post" {
        data["images"] = json!({ "handler": "return (env) => env.images" });
        data["suggestion_title"] = json!("${suggestion_title}");
        data["suggestion_options"] = json!("${suggestion_options}");
    }
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        data["token"] = json!(token);
    }
    let mut wrapper = json!({
        "name": SERVICE_NAME,
        "url": url,
        "method": method,
        "type": "GM_xmlhttpRequest",
        "contentType": "json",
        "headers": {},
        "data": data,
        "handler": OCS_HANDLER,
    });
    if method == "post" {
        wrapper["headers"] = json!({ "Content-Type": "application/json" });
    }
    json!([wrapper])
}
