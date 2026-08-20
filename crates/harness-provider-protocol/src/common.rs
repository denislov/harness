use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::MAX_JSON_SAFE_INTEGER;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WireMessageSource {
    User,
    Model { provider: String, model: String },
    Plugin,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireBlobRef {
    pub id: String,
    pub sha256: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

impl WireBlobRef {
    pub fn validate(&self) -> Result<(), CommonWireValidationError> {
        if self.id.is_empty() {
            return Err(CommonWireValidationError::EmptyField("blob.id"));
        }
        let valid_sha = self.sha256.len() == 64
            && self
                .sha256
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte));
        if !valid_sha {
            return Err(CommonWireValidationError::InvalidSha256);
        }
        if self.size > MAX_JSON_SAFE_INTEGER {
            return Err(CommonWireValidationError::UnsafeJsonInteger("blob.size"));
        }
        if self.media_type.as_ref().is_some_and(String::is_empty) {
            return Err(CommonWireValidationError::EmptyField("blob.mediaType"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum WireContentBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Image {
        blob: WireBlobRef,
    },
    ToolCall {
        id: String,
        name: String,
        #[serde(rename = "argumentsJson")]
        arguments_json: String,
    },
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        content: Vec<WireContentBlock>,
        #[serde(rename = "isError")]
        is_error: bool,
    },
    Blob {
        blob: WireBlobRef,
    },
}

impl WireContentBlock {
    pub fn validate(&self) -> Result<(), CommonWireValidationError> {
        match self {
            Self::Text { .. } | Self::Reasoning { .. } => Ok(()),
            Self::Image { blob } | Self::Blob { blob } => blob.validate(),
            Self::ToolCall {
                id,
                name,
                arguments_json,
            } => {
                if id.is_empty() {
                    return Err(CommonWireValidationError::EmptyField("toolCall.id"));
                }
                if name.is_empty() {
                    return Err(CommonWireValidationError::EmptyField("toolCall.name"));
                }
                serde_json::from_str::<Value>(arguments_json).map_err(|error| {
                    CommonWireValidationError::InvalidArgumentsJson(error.to_string())
                })?;
                Ok(())
            }
            Self::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                if tool_call_id.is_empty() {
                    return Err(CommonWireValidationError::EmptyField(
                        "toolResult.toolCallId",
                    ));
                }
                validate_content(content)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMessage {
    pub id: String,
    pub role: WireRole,
    pub source: WireMessageSource,
    pub content: Vec<WireContentBlock>,
}

impl WireMessage {
    pub fn validate(&self) -> Result<(), CommonWireValidationError> {
        if self.id.is_empty() {
            return Err(CommonWireValidationError::EmptyField("message.id"));
        }
        if let WireMessageSource::Model { provider, model } = &self.source {
            if provider.is_empty() {
                return Err(CommonWireValidationError::EmptyField(
                    "message.source.provider",
                ));
            }
            if model.is_empty() {
                return Err(CommonWireValidationError::EmptyField(
                    "message.source.model",
                ));
            }
        }
        validate_content(&self.content)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WireErrorCode {
    InvalidArgument,
    NotFound,
    Conflict,
    PermissionDenied,
    Cancelled,
    DeadlineExceeded,
    ProviderUnavailable,
    ProviderProtocolError,
    ToolExecutionFailed,
    ModelRequestFailed,
    SessionCorrupt,
    UnknownOutcome,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WirePortableError {
    pub code: WireErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

impl WirePortableError {
    pub fn validate(&self) -> Result<(), CommonWireValidationError> {
        if self.message.is_empty() {
            return Err(CommonWireValidationError::EmptyField("error.message"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireTokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, Value>,
}

impl WireTokenUsage {
    pub fn validate(&self) -> Result<(), CommonWireValidationError> {
        if self.input_tokens > MAX_JSON_SAFE_INTEGER {
            return Err(CommonWireValidationError::UnsafeJsonInteger(
                "usage.inputTokens",
            ));
        }
        if self.output_tokens > MAX_JSON_SAFE_INTEGER {
            return Err(CommonWireValidationError::UnsafeJsonInteger(
                "usage.outputTokens",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireCancelCauseKind {
    User,
    Parent,
    Timeout,
    Policy,
    Shutdown,
    Disposed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WireCancelCause {
    pub kind: WireCancelCauseKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CommonWireValidationError {
    #[error("wire field {0} must not be empty")]
    EmptyField(&'static str),

    #[error("blob sha256 must contain exactly 64 lowercase hexadecimal characters")]
    InvalidSha256,

    #[error("wire integer {0} exceeds the maximum safe JSON integer")]
    UnsafeJsonInteger(&'static str),

    #[error("argumentsJson is invalid JSON: {0}")]
    InvalidArgumentsJson(String),
}

fn validate_content(content: &[WireContentBlock]) -> Result<(), CommonWireValidationError> {
    for block in content {
        block.validate()?;
    }
    Ok(())
}

pub(crate) fn is_utc_rfc3339(value: &str) -> bool {
    use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

    OffsetDateTime::parse(value, &Rfc3339).is_ok_and(|parsed| parsed.offset() == UtcOffset::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_arguments_remain_a_validated_json_string() {
        let block = WireContentBlock::ToolCall {
            id: "call_1".to_owned(),
            name: "read_file".to_owned(),
            arguments_json: r#"{"path":"README.md"}"#.to_owned(),
        };
        assert!(block.validate().is_ok());
    }

    #[test]
    fn non_lowercase_blob_digest_is_rejected() {
        let blob = WireBlobRef {
            id: "blob_1".to_owned(),
            sha256: "A".repeat(64),
            size: 1,
            media_type: None,
        };
        assert!(matches!(
            blob.validate(),
            Err(CommonWireValidationError::InvalidSha256)
        ));
    }
}
