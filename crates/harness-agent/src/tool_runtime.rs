use std::sync::Arc;

use harness_llm::ModelToolSpec;
use harness_tools::{ToolPolicy, ToolRegistration, ToolRegistry};
use thiserror::Error;

#[derive(Clone)]
pub struct AgentToolRuntime {
    registry: Arc<ToolRegistry>,
    policy: Arc<dyn ToolPolicy>,
    max_automatic_attempts: u32,
}

impl AgentToolRuntime {
    pub fn new(
        registry: Arc<ToolRegistry>,
        policy: Arc<dyn ToolPolicy>,
        max_automatic_attempts: u32,
    ) -> Result<Self, AgentToolRuntimeError> {
        if max_automatic_attempts == 0 {
            return Err(AgentToolRuntimeError::ZeroMaxAutomaticAttempts);
        }
        Ok(Self {
            registry,
            policy,
            max_automatic_attempts,
        })
    }

    pub fn registry(&self) -> &Arc<ToolRegistry> {
        &self.registry
    }

    pub fn policy(&self) -> &Arc<dyn ToolPolicy> {
        &self.policy
    }

    pub const fn max_automatic_attempts(&self) -> u32 {
        self.max_automatic_attempts
    }

    pub fn resolve(&self, name: &str) -> Option<&ToolRegistration> {
        self.registry.resolve(name)
    }

    pub fn model_tool_specs(&self) -> Vec<ModelToolSpec> {
        self.registry
            .definitions()
            .map(|definition| ModelToolSpec {
                name: definition.name.clone(),
                description: definition.description.clone(),
                input_schema: definition.input_schema.clone(),
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AgentToolRuntimeError {
    #[error("maxAutomaticAttempts must be greater than zero")]
    ZeroMaxAutomaticAttempts,
}
