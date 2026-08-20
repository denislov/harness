use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::PROTOCOL_VERSION;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireSideEffectClass {
    ReadOnly,
    IdempotentWrite,
    NonIdempotentWrite,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CapabilityDescriptor {
    Tool {
        name: String,
        version: String,
        #[serde(rename = "parallelSafe")]
        parallel_safe: bool,
        #[serde(rename = "sideEffect")]
        side_effect: WireSideEffectClass,
        #[serde(rename = "supportsIdempotencyKey")]
        supports_idempotency_key: bool,
    },
    Llm {
        models: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderManifest {
    pub provider_id: String,
    pub provider_version: String,
    pub protocol_version: String,
    pub capabilities: Vec<CapabilityDescriptor>,
}

impl ProviderManifest {
    pub fn validate(&self) -> Result<(), ManifestValidationError> {
        if self.provider_id.trim().is_empty() {
            return Err(ManifestValidationError::EmptyProviderId);
        }
        if self.provider_version.trim().is_empty() {
            return Err(ManifestValidationError::EmptyProviderVersion);
        }
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ManifestValidationError::UnsupportedProtocolVersion(
                self.protocol_version.clone(),
            ));
        }

        let mut tool_names = BTreeSet::new();
        let mut models = BTreeSet::new();
        for capability in &self.capabilities {
            match capability {
                CapabilityDescriptor::Tool {
                    name,
                    version,
                    side_effect,
                    supports_idempotency_key,
                    ..
                } => {
                    if name.trim().is_empty() {
                        return Err(ManifestValidationError::EmptyToolName);
                    }
                    if version.trim().is_empty() {
                        return Err(ManifestValidationError::EmptyToolVersion(name.clone()));
                    }
                    if *side_effect == WireSideEffectClass::IdempotentWrite
                        && !*supports_idempotency_key
                    {
                        return Err(ManifestValidationError::IdempotentWriteWithoutKeySupport(
                            name.clone(),
                        ));
                    }
                    if !tool_names.insert(name.clone()) {
                        return Err(ManifestValidationError::DuplicateTool(name.clone()));
                    }
                }
                CapabilityDescriptor::Llm {
                    models: capability_models,
                } => {
                    if capability_models.is_empty() {
                        return Err(ManifestValidationError::EmptyModelSet);
                    }
                    for model in capability_models {
                        if model.trim().is_empty() {
                            return Err(ManifestValidationError::EmptyModelName);
                        }
                        if !models.insert(model.clone()) {
                            return Err(ManifestValidationError::DuplicateModel(model.clone()));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ManifestValidationError {
    #[error("providerId must not be empty")]
    EmptyProviderId,

    #[error("providerVersion must not be empty")]
    EmptyProviderVersion,

    #[error("unsupported provider protocol version {0}")]
    UnsupportedProtocolVersion(String),

    #[error("tool capability name must not be empty")]
    EmptyToolName,

    #[error("tool capability {0} has an empty version")]
    EmptyToolVersion(String),

    #[error("tool capability {0} is declared more than once")]
    DuplicateTool(String),

    #[error("idempotent-write tool capability {0} must support idempotency keys")]
    IdempotentWriteWithoutKeySupport(String),

    #[error("LLM capability must declare at least one model")]
    EmptyModelSet,

    #[error("LLM model name must not be empty")]
    EmptyModelName,

    #[error("LLM model {0} is declared more than once")]
    DuplicateModel(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_tool_names_are_rejected() {
        let tool = CapabilityDescriptor::Tool {
            name: "read_file".to_owned(),
            version: "1".to_owned(),
            parallel_safe: true,
            side_effect: WireSideEffectClass::ReadOnly,
            supports_idempotency_key: false,
        };
        let manifest = ProviderManifest {
            provider_id: "prv_python".to_owned(),
            provider_version: "1.0.0".to_owned(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            capabilities: vec![tool.clone(), tool],
        };

        assert!(matches!(
            manifest.validate(),
            Err(ManifestValidationError::DuplicateTool(name)) if name == "read_file"
        ));
    }
}
