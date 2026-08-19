use harness_types::SideEffectClass;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: String,
    pub version: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    pub parallel_safe: bool,
    pub side_effect: SideEffectClass,
    pub default_timeout_ms: u64,
}

impl ToolDefinition {
    pub fn validate(&self) -> Result<(), ToolDefinitionError> {
        if self.name.is_empty() {
            return Err(ToolDefinitionError::EmptyName);
        }
        if self.version.is_empty() {
            return Err(ToolDefinitionError::EmptyVersion);
        }
        if self.description.is_empty() {
            return Err(ToolDefinitionError::EmptyDescription);
        }
        if self.default_timeout_ms == 0 {
            return Err(ToolDefinitionError::ZeroTimeout);
        }
        if !self.input_schema.is_object() {
            return Err(ToolDefinitionError::InputSchemaNotObject);
        }
        if self
            .output_schema
            .as_ref()
            .is_some_and(|schema| !schema.is_object())
        {
            return Err(ToolDefinitionError::OutputSchemaNotObject);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ToolDefinitionError {
    #[error("tool name must not be empty")]
    EmptyName,

    #[error("tool version must not be empty")]
    EmptyVersion,

    #[error("tool description must not be empty")]
    EmptyDescription,

    #[error("tool defaultTimeoutMs must be greater than zero")]
    ZeroTimeout,

    #[error("tool inputSchema must be a JSON object")]
    InputSchemaNotObject,

    #[error("tool outputSchema must be a JSON object when present")]
    OutputSchemaNotObject,
}

#[cfg(test)]
mod tests {
    use harness_types::SideEffectClass;

    use super::*;

    #[test]
    fn valid_definition_is_accepted() {
        let definition = ToolDefinition {
            name: "read_file".to_owned(),
            version: "1".to_owned(),
            description: "Read one file".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            parallel_safe: true,
            side_effect: SideEffectClass::ReadOnly,
            default_timeout_ms: 30_000,
        };

        assert!(definition.validate().is_ok());
    }
}
