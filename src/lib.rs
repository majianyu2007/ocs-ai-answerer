//! ocs-ai-answerer: an LLM-backed question bank service for OCS `AnswererWrapper`.

pub mod answer;
pub mod api;
pub mod config;
pub mod error;
pub mod provider;

use answer::Answerer;
use api::AppState;
use config::Config;
use error::ProviderError;
use provider::AiClient;

/// Builds the shared application state from a validated configuration.
pub fn build_state(config: Config) -> Result<AppState, ProviderError> {
    let client = AiClient::new(&config.ai)?;
    let answerer = Answerer::new(client, &config.answer);
    Ok(AppState::new(config, answerer))
}
