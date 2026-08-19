use serde::{Deserialize, Serialize};

use crate::{BlobRef, JsonText, MessageId, ProviderId, ToolCallId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum MessageSource {
    User,
    Model { provider: ProviderId, model: String },
    Plugin,
    System,
}

impl MessageSource {
    pub const fn user() -> Self {
        Self::User
    }

    pub fn model(provider: ProviderId, model: impl Into<String>) -> Self {
        Self::Model {
            provider,
            model: model.into(),
        }
    }

    pub const fn plugin() -> Self {
        Self::Plugin
    }

    pub const fn system() -> Self {
        Self::System
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: MessageId,
    pub role: Role,
    pub source: MessageSource,
    pub content: Vec<ContentBlock>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Image {
        blob: BlobRef,
    },
    ToolCall {
        id: ToolCallId,
        name: String,
        #[serde(rename = "argumentsJson")]
        arguments_json: JsonText,
    },
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: ToolCallId,
        content: Vec<ContentBlock>,
        #[serde(rename = "isError")]
        is_error: bool,
    },
    Blob {
        blob: BlobRef,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}
