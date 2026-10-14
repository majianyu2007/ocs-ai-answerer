pub mod handlers;
pub mod ocs_config;
pub mod types;

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::get;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::answer::Answerer;
use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub answerer: Answerer,
    pub started_at: Instant,
}

impl AppState {
    pub fn new(config: Config, answerer: Answerer) -> Self {
        Self {
            config: Arc::new(config),
            answerer,
            started_at: Instant::now(),
        }
    }
}

pub fn router(state: AppState) -> Router {
    let cors = state.config.server.cors;
    let router = Router::new()
        .route("/", get(handlers::index))
        .route(
            "/api/answer",
            get(handlers::answer_get).post(handlers::answer_post),
        )
        .route("/health", get(handlers::health))
        .route("/api/cache", axum::routing::delete(handlers::cache_clear))
        .route(
            "/api/cache/forget",
            axum::routing::post(handlers::cache_forget),
        )
        .route("/ocs-config", get(handlers::ocs_config))
        .fallback(handlers::not_found)
        // Image questions carry base64 images in the body.
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    // `fetch`-type wrappers run in the page origin and need CORS; GM_xmlhttpRequest does not.
    if cors {
        router.layer(CorsLayer::permissive())
    } else {
        router
    }
}
