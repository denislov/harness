use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{WireCancelCauseKind, WireContentBlock, common::is_utc_rfc3339};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInvokeParams {
    pub operation_id: String,
    pub invocation_id: String,
    pub call_id: String,
    pub session_id: String,
    pub tool: String,
    pub arguments_json: String,
    pub attempt: u32,
    pub idempotency_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
}

impl ToolInvokeParams {
    pub fn validate(&self) -> Result<(), ToolInvokeValidationError> {
        for (field, value) in [
            ("operationId", self.operation_id.as_str()),
            ("invocationId", self.invocation_id.as_str()),
            ("callId", self.call_id.as_str()),
            ("sessionId", self.session_id.as_str()),
            ("tool", self.tool.as_str()),
            ("idempotencyKey", self.idempotency_key.as_str()),
        ] {
            if value.is_empty() {
                return Err(ToolInvokeValidationError::EmptyField(field));
            }
        }
        if self.operation_id != self.invocation_id {
            return Err(ToolInvokeValidationError::OperationInvocationMismatch);
        }
        if self.attempt == 0 {
            return Err(ToolInvokeValidationError::ZeroAttempt);
        }
        serde_json::from_str::<serde_json::Value>(&self.arguments_json)
            .map_err(|error| ToolInvokeValidationError::InvalidArgumentsJson(error.to_string()))?;
        if let Some(deadline) = &self.deadline {
            if deadline.is_empty() {
                return Err(ToolInvokeValidationError::EmptyDeadline);
            }
            if !is_utc_rfc3339(deadline) {
                return Err(ToolInvokeValidationError::InvalidDeadline);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ProviderToolOutcome {
    Success {
        content: Vec<WireContentBlock>,
    },
    Error {
        code: String,
        message: String,
        #[serde(default)]
        content: Vec<WireContentBlock>,
    },
    Cancelled {
        cause: WireCancelCauseKind,
    },
}

impl ProviderToolOutcome {
    pub fn validate(&self) -> Result<(), ToolInvokeValidationError> {
        let content = match self {
            Self::Success { content } => content,
            Self::Error {
                code,
                message,
                content,
            } => {
                if code.is_empty() {
                    return Err(ToolInvokeValidationError::EmptyOutcomeErrorCode);
                }
                if message.is_empty() {
                    return Err(ToolInvokeValidationError::EmptyOutcomeErrorMessage);
                }
                content
            }
            Self::Cancelled { .. } => return Ok(()),
        };
        for block in content {
            block.validate().map_err(|error| {
                ToolInvokeValidationError::InvalidOutcomeContent(error.to_string())
            })?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolInvokeResult {
    pub outcome: ProviderToolOutcome,
}

impl ToolInvokeResult {
    pub fn validate(&self) -> Result<(), ToolInvokeValidationError> {
        self.outcome.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ToolInvokeValidationError {
    #[error("tool.invoke field {0} must not be empty")]
    EmptyField(&'static str),

    #[error("tool.invoke operationId must equal invocationId in protocol v1")]
    OperationInvocationMismatch,

    #[error("tool.invoke attempt must be greater than zero")]
    ZeroAttempt,

    #[error("tool.invoke argumentsJson is invalid JSON: {0}")]
    InvalidArgumentsJson(String),

    #[error("tool.invoke deadline must not be an empty string")]
    EmptyDeadline,

    #[error("tool.invoke deadline must be RFC3339 with a UTC offset")]
    InvalidDeadline,

    #[error("tool result error outcome code must not be empty")]
    EmptyOutcomeErrorCode,

    #[error("tool result error outcome message must not be empty")]
    EmptyOutcomeErrorMessage,

    #[error("tool result content is invalid: {0}")]
    InvalidOutcomeContent(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_utc_deadline_is_rejected() {
        let params = ToolInvokeParams {
            operation_id: "inv_1".to_owned(),
            invocation_id: "inv_1".to_owned(),
            call_id: "call_1".to_owned(),
            session_id: "ses_1".to_owned(),
            tool: "read_file".to_owned(),
            arguments_json: "{}".to_owned(),
            attempt: 1,
            idempotency_key: "idem_1".to_owned(),
            deadline: Some("2026-08-20T04:00:00+01:00".to_owned()),
        };
        assert!(matches!(
            params.validate(),
            Err(ToolInvokeValidationError::InvalidDeadline)
        ));
    }

    #[test]
    fn invalid_arguments_json_is_rejected_without_rewriting_the_string() {
        let params = ToolInvokeParams {
            operation_id: "inv_1".to_owned(),
            invocation_id: "inv_1".to_owned(),
            call_id: "call_1".to_owned(),
            session_id: "ses_1".to_owned(),
            tool: "read_file".to_owned(),
            arguments_json: "{".to_owned(),
            attempt: 1,
            idempotency_key: "idem_1".to_owned(),
            deadline: None,
        };
        assert!(matches!(
            params.validate(),
            Err(ToolInvokeValidationError::InvalidArgumentsJson(_))
        ));
    }
}
