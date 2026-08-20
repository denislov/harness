use harness_types::JsonText;
use thiserror::Error;

use crate::ToolDefinition;

/// Core-side argument validation seam.
///
/// The v0.1 architecture requires validation before provider dispatch, but does
/// not bind the domain crate to a specific JSON Schema implementation. Runtime
/// composition must supply a validator with the semantics it claims to enforce.
pub trait ToolArgumentValidator: Send + Sync {
    fn validate(
        &self,
        definition: &ToolDefinition,
        arguments_json: &JsonText,
    ) -> Result<(), ToolArgumentValidationError>;

    /// Stable identity included in durable execution-composition snapshots.
    ///
    /// The default identifies the concrete Rust type. Validators whose behavior
    /// depends on configuration should override this with a stable semantic
    /// version/config identity so in-flight recovery detects that change.
    fn composition_identity(&self) -> String {
        format!("rust-type:{}", std::any::type_name::<Self>())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("tool arguments do not satisfy the registered input schema: {message}")]
pub struct ToolArgumentValidationError {
    pub message: String,
}

impl ToolArgumentValidationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
