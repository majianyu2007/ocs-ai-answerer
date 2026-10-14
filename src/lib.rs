//! ocs-ai-answerer: an LLM-backed question bank service for OCS `AnswererWrapper`.

pub mod answer;
pub mod api;
pub mod config;
pub mod error;
pub mod provider;

use answer::Answerer;
use answer::cache::AnswerCache;
use api::AppState;
use config::Config;
use error::ProviderError;
use provider::{AiClient, build_http};

/// Builds the shared application state from a validated configuration.
pub fn build_state(config: Config) -> Result<AppState, ProviderError> {
    // One connection pool shared by every model.
    let http = build_http(config.ai.connect_timeout_secs)?;
    let client = AiClient::with_http(&config.ai, http.clone())?;

    let mut builder = Answerer::builder(client, &config.answer).time_budget(
        std::time::Duration::from_secs(config.server.request_timeout_secs),
    );
    if config.ensemble.enabled {
        let voters = config
            .ensemble
            .models
            .iter()
            .map(|m| AiClient::with_http(m, http.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let judge = config
            .ensemble
            .judge
            .as_ref()
            .map(|j| AiClient::with_http(j, http.clone()))
            .transpose()?;
        builder = builder.ensemble(&config.ensemble, voters, judge);
    }
    if config.cache.enabled {
        let cache = AnswerCache::open(&config.cache).map_err(|e| {
            ProviderError::Build(format!("无法打开答案缓存文件 {}: {e}", config.cache.file))
        })?;
        builder = builder.cache(Some(cache));
    }
    Ok(AppState::new(config, builder.build()))
}
