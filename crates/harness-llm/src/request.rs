use std::collections::BTreeSet;

use harness_types::{Message, ProviderId, RequestId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequest {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub provider: ProviderId,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ModelToolSpec>,
    #[serde(default)]
    pub options: ModelOptions,
}

impl ModelRequest {
    pub fn validate(&self) -> Result<(), ModelRequestError> {
        validate_request_fields(&self.model, &self.tools, &self.options)
    }

    /// Serializes the exact provider-neutral request object used by Core.
    ///
    /// These bytes are suitable for immutable BlobStore persistence before the
    /// corresponding `model/requested` SessionEvent is committed.
    pub fn snapshot_bytes(&self) -> Result<Vec<u8>, ModelSnapshotError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(ModelSnapshotError::from)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequestConfig {
    pub provider: ProviderId,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ModelToolSpec>,
    #[serde(default)]
    pub options: ModelOptions,
}

impl ModelRequestConfig {
    pub fn validate(&self) -> Result<(), ModelRequestError> {
        validate_request_fields(&self.model, &self.tools, &self.options)
    }

    pub fn build(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        messages: Vec<Message>,
    ) -> Result<ModelRequest, ModelRequestError> {
        self.validate()?;
        Ok(ModelRequest {
            request_id,
            session_id,
            provider: self.provider.clone(),
            model: self.model.clone(),
            system: self.system.clone(),
            messages,
            tools: self.tools.clone(),
            options: self.options.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ModelRequestError {
    #[error("model name must not be empty")]
    EmptyModel,

    #[error("model tool name must not be empty")]
    EmptyToolName,

    #[error("model tool {0} is declared more than once")]
    DuplicateToolName(String),

    #[error("maxOutputTokens must be greater than zero when specified")]
    ZeroMaxOutputTokens,
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ModelSnapshotError {
    #[error(transparent)]
    InvalidRequest(#[from] ModelRequestError),

    #[error("failed to serialize provider-neutral ModelRequest snapshot: {0}")]
    Serialize(#[from] serde_json::Error),
}

fn validate_request_fields(
    model: &str,
    tools: &[ModelToolSpec],
    options: &ModelOptions,
) -> Result<(), ModelRequestError> {
    if model.is_empty() {
        return Err(ModelRequestError::EmptyModel);
    }
    if options.max_output_tokens == Some(0) {
        return Err(ModelRequestError::ZeroMaxOutputTokens);
    }

    let mut names = BTreeSet::new();
    for tool in tools {
        if tool.name.is_empty() {
            return Err(ModelRequestError::EmptyToolName);
        }
        if !names.insert(tool.name.clone()) {
            return Err(ModelRequestError::DuplicateToolName(tool.name.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use harness_types::{ContentBlock, Message, MessageId, MessageSource, Role};

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    #[test]
    fn snapshot_round_trips_exact_provider_neutral_request() {
        let config = ModelRequestConfig {
            provider: id("prv_test"),
            model: "model-x".to_owned(),
            system: Some("system".to_owned()),
            tools: Vec::new(),
            options: ModelOptions {
                max_output_tokens: Some(128),
            },
        };
        let request = config
            .build(
                id("req_1"),
                id("ses_1"),
                vec![Message {
                    id: MessageId::new("msg_1").unwrap(),
                    role: Role::User,
                    source: MessageSource::user(),
                    content: vec![ContentBlock::text("hello")],
                }],
            )
            .unwrap();

        let bytes = request.snapshot_bytes().unwrap();
        let decoded: ModelRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn duplicate_tool_names_are_rejected() {
        let tool = ModelToolSpec {
            name: "read_file".to_owned(),
            description: "read".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let config = ModelRequestConfig {
            provider: id("prv_test"),
            model: "model-x".to_owned(),
            system: None,
            tools: vec![tool.clone(), tool],
            options: ModelOptions::default(),
        };

        assert!(matches!(
            config.validate(),
            Err(ModelRequestError::DuplicateToolName(name)) if name == "read_file"
        ));
    }
}
