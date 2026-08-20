use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    MAX_JSON_SAFE_INTEGER, WireContentBlock, WireErrorCode, WireMessage, WirePortableError,
    WireTokenUsage, common::is_utc_rfc3339,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireModelOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireModelToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireModelRequest {
    pub request_id: String,
    pub session_id: String,
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<WireMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireModelToolSpec>,
    #[serde(default)]
    pub options: WireModelOptions,
}

impl WireModelRequest {
    pub fn validate(&self) -> Result<(), LlmWireValidationError> {
        for (field, value) in [
            ("requestId", self.request_id.as_str()),
            ("sessionId", self.session_id.as_str()),
            ("provider", self.provider.as_str()),
            ("model", self.model.as_str()),
        ] {
            if value.is_empty() {
                return Err(LlmWireValidationError::EmptyField(field));
            }
        }
        if self.options.max_output_tokens == Some(0) {
            return Err(LlmWireValidationError::ZeroMaxOutputTokens);
        }

        for message in &self.messages {
            message
                .validate()
                .map_err(|error| LlmWireValidationError::InvalidMessage(error.to_string()))?;
        }

        let mut names = BTreeSet::new();
        for tool in &self.tools {
            if tool.name.is_empty() {
                return Err(LlmWireValidationError::EmptyToolName);
            }
            if !names.insert(tool.name.clone()) {
                return Err(LlmWireValidationError::DuplicateToolName(tool.name.clone()));
            }
            if !tool.input_schema.is_object() {
                return Err(LlmWireValidationError::ToolSchemaNotObject(
                    tool.name.clone(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmStartParams {
    pub operation_id: String,
    pub stream_id: String,
    pub request: WireModelRequest,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
}

impl LlmStartParams {
    pub fn validate(&self) -> Result<(), LlmWireValidationError> {
        if self.operation_id.is_empty() {
            return Err(LlmWireValidationError::EmptyField("operationId"));
        }
        if self.stream_id.is_empty() {
            return Err(LlmWireValidationError::EmptyField("streamId"));
        }
        if self.operation_id != self.request.request_id {
            return Err(LlmWireValidationError::OperationRequestMismatch);
        }
        if let Some(deadline) = &self.deadline {
            if deadline.is_empty() {
                return Err(LlmWireValidationError::EmptyDeadline);
            }
            if !is_utc_rfc3339(deadline) {
                return Err(LlmWireValidationError::InvalidDeadline);
            }
        }
        self.request.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmStartResult {
    pub accepted: bool,
    pub stream_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireBlockType {
    Text,
    Reasoning,
    ToolCall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireFinishReason {
    Completed,
    MaxTokens,
    Error,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum WireLlmStreamEvent {
    BlockStart {
        index: u32,
        #[serde(rename = "blockType")]
        block_type: WireBlockType,
    },
    TextDelta {
        index: u32,
        text: String,
    },
    ReasoningDelta {
        index: u32,
        text: String,
    },
    ToolCallDelta {
        index: u32,
        #[serde(rename = "callId")]
        call_id: String,
        name: Option<String>,
        #[serde(rename = "argumentsDelta")]
        arguments_delta: String,
    },
    BlockEnd {
        index: u32,
        block: WireContentBlock,
    },
    Usage {
        usage: WireTokenUsage,
    },
    Finish {
        reason: WireFinishReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        failure: Option<WirePortableError>,
    },
}

impl WireLlmStreamEvent {
    pub fn validate(&self) -> Result<(), LlmWireValidationError> {
        match self {
            Self::ToolCallDelta { call_id, .. } if call_id.is_empty() => {
                Err(LlmWireValidationError::EmptyToolCallId)
            }
            Self::ToolCallDelta {
                name: Some(name), ..
            } if name.is_empty() => Err(LlmWireValidationError::EmptyToolCallName),
            Self::BlockEnd { block, .. } => block
                .validate()
                .map_err(|error| LlmWireValidationError::InvalidContent(error.to_string())),
            Self::Usage { usage } => usage
                .validate()
                .map_err(|error| LlmWireValidationError::InvalidUsage(error.to_string())),
            Self::Finish { reason, failure } => {
                if let Some(failure) = failure {
                    failure.validate().map_err(|error| {
                        LlmWireValidationError::InvalidFailure(error.to_string())
                    })?;
                }
                match reason {
                    WireFinishReason::Completed | WireFinishReason::MaxTokens
                        if failure.is_some() =>
                    {
                        Err(LlmWireValidationError::SuccessFinishHasFailure)
                    }
                    WireFinishReason::Error if failure.is_none() => {
                        Err(LlmWireValidationError::FailureFinishMissingFailure)
                    }
                    WireFinishReason::Error
                        if failure
                            .as_ref()
                            .is_some_and(|failure| failure.code == WireErrorCode::Cancelled) =>
                    {
                        Err(LlmWireValidationError::ErrorFinishCancelledCode)
                    }
                    WireFinishReason::Cancelled => match failure {
                        Some(failure) if failure.code == WireErrorCode::Cancelled => Ok(()),
                        Some(_) => Err(LlmWireValidationError::CancelledFinishWrongCode),
                        None => Err(LlmWireValidationError::FailureFinishMissingFailure),
                    },
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    pub const fn is_finish(&self) -> bool {
        matches!(self, Self::Finish { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmEventParams {
    pub stream_id: String,
    pub seq: u64,
    pub event: WireLlmStreamEvent,
}

impl LlmEventParams {
    pub fn validate(&self) -> Result<(), LlmWireValidationError> {
        if self.stream_id.is_empty() {
            return Err(LlmWireValidationError::EmptyField("streamId"));
        }
        if self.seq == 0 {
            return Err(LlmWireValidationError::ZeroStreamSequence);
        }
        if self.seq > MAX_JSON_SAFE_INTEGER {
            return Err(LlmWireValidationError::StreamSequenceTooLarge);
        }
        self.event.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum LlmWireValidationError {
    #[error("LLM wire field {0} must not be empty")]
    EmptyField(&'static str),

    #[error("LLM maxOutputTokens must be greater than zero when specified")]
    ZeroMaxOutputTokens,

    #[error("LLM tool name must not be empty")]
    EmptyToolName,

    #[error("LLM tool {0} is declared more than once")]
    DuplicateToolName(String),

    #[error("LLM tool {0} inputSchema must be an object")]
    ToolSchemaNotObject(String),

    #[error("llm.start operationId must equal request.requestId in protocol v1")]
    OperationRequestMismatch,

    #[error("llm.start deadline must not be an empty string")]
    EmptyDeadline,

    #[error("llm.start deadline must be RFC3339 with a UTC offset")]
    InvalidDeadline,

    #[error("LLM stream seq must start at one")]
    ZeroStreamSequence,

    #[error("LLM stream seq exceeds the maximum safe JSON integer")]
    StreamSequenceTooLarge,

    #[error("LLM message is invalid: {0}")]
    InvalidMessage(String),

    #[error("LLM content block is invalid: {0}")]
    InvalidContent(String),

    #[error("LLM usage is invalid: {0}")]
    InvalidUsage(String),

    #[error("LLM failure is invalid: {0}")]
    InvalidFailure(String),

    #[error("tool-call-delta callId must not be empty")]
    EmptyToolCallId,

    #[error("tool-call-delta name must not be empty when present")]
    EmptyToolCallName,

    #[error("completed/max-tokens finish may not carry failure")]
    SuccessFinishHasFailure,

    #[error("error/cancelled finish must carry failure")]
    FailureFinishMissingFailure,

    #[error("error finish must not carry CANCELLED failure code")]
    ErrorFinishCancelledCode,

    #[error("cancelled finish must carry CANCELLED failure code")]
    CancelledFinishWrongCode,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WireErrorCode, WirePortableError};

    #[test]
    fn cancelled_finish_requires_cancelled_error_code() {
        let event = WireLlmStreamEvent::Finish {
            reason: WireFinishReason::Cancelled,
            failure: Some(WirePortableError {
                code: WireErrorCode::Internal,
                message: "wrong".to_owned(),
                details: Default::default(),
            }),
        };
        assert!(matches!(
            event.validate(),
            Err(LlmWireValidationError::CancelledFinishWrongCode)
        ));
    }
}
