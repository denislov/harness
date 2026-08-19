use std::sync::Arc;

use harness_llm::{LlmProvider, ModelRequestConfig};
use harness_storage::BlobStore;
use harness_types::ProviderId;
use thiserror::Error;

#[derive(Clone)]
pub struct AgentLlmRuntime {
    request_config: ModelRequestConfig,
    provider: Arc<dyn LlmProvider>,
    blob_store: Arc<dyn BlobStore>,
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
        })
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
}
