use std::sync::Arc;

use harness_llm::{LlmProvider, ModelRequestConfig};
use harness_storage::BlobStore;
use harness_types::ProviderId;
use thiserror::Error;

pub const DEFAULT_LLM_TIMEOUT_MS: u64 = 120_000;

#[derive(Clone)]
pub struct AgentLlmRuntime {
    request_config: ModelRequestConfig,
    provider: Arc<dyn LlmProvider>,
    blob_store: Arc<dyn BlobStore>,
    timeout_ms: u64,
}

impl AgentLlmRuntime {
    pub fn new(
        request_config: ModelRequestConfig,
        provider: Arc<dyn LlmProvider>,
        blob_store: Arc<dyn BlobStore>,
    ) -> Result<Self, AgentLlmRuntimeError> {
        request_config
            .validate()
            .map_err(|error| AgentLlmRuntimeError::InvalidRequestConfig(error.to_string()))?;
        if provider.provider_id() != &request_config.provider {
            return Err(AgentLlmRuntimeError::ProviderMismatch {
                configured: request_config.provider.clone(),
                actual: provider.provider_id().clone(),
            });
        }
        Ok(Self {
            request_config,
            provider,
            blob_store,
            timeout_ms: DEFAULT_LLM_TIMEOUT_MS,
        })
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Result<Self, AgentLlmRuntimeError> {
        if timeout_ms == 0 {
            return Err(AgentLlmRuntimeError::ZeroTimeout);
        }
        self.timeout_ms = timeout_ms;
        Ok(self)
    }

    pub const fn request_config(&self) -> &ModelRequestConfig {
        &self.request_config
    }

    pub fn provider(&self) -> &Arc<dyn LlmProvider> {
        &self.provider
    }

    pub fn blob_store(&self) -> &Arc<dyn BlobStore> {
        &self.blob_store
    }

    pub const fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AgentLlmRuntimeError {
    #[error("invalid model request configuration: {0}")]
    InvalidRequestConfig(String),

    #[error("configured LLM provider {configured} does not match runtime provider {actual}")]
    ProviderMismatch {
        configured: ProviderId,
        actual: ProviderId,
    },

    #[error("LLM timeout must be greater than zero milliseconds")]
    ZeroTimeout,
}
