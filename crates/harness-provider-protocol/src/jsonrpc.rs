use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::Value;
use thiserror::Error;

use crate::JSONRPC_VERSION;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RpcId(String);

impl RpcId {
    pub fn new(value: impl Into<String>) -> Result<Self, RpcIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RpcIdError);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RpcId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for RpcId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RpcId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("JSON-RPC id must be a non-empty string in Provider Protocol v1")]
pub struct RpcIdError;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcRequest<P> {
    pub jsonrpc: String,
    pub id: RpcId,
    pub method: String,
    pub params: P,
}

impl<P> RpcRequest<P> {
    pub fn new(id: RpcId, method: impl Into<String>, params: P) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcNotification<P> {
    pub jsonrpc: String,
    pub method: String,
    pub params: P,
}

impl<P> RpcNotification<P> {
    pub fn new(method: impl Into<String>, params: P) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: method.into(),
            params,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcSuccessResponse<R> {
    pub jsonrpc: String,
    pub id: RpcId,
    pub result: R,
}

impl<R> RpcSuccessResponse<R> {
    pub fn new(id: RpcId, result: R) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcErrorResponse {
    pub jsonrpc: String,
    pub id: RpcId,
    pub error: RpcErrorObject,
}

impl RpcErrorResponse {
    pub fn new(id: RpcId, error: RpcErrorObject) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            error,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcErrorObject {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;

    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum InboundMessage {
    Response(RpcResponseEnvelope),
    Notification(RpcNotificationEnvelope),
    Request(RpcRequestEnvelope),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RpcResponseEnvelope {
    pub id: RpcId,
    pub outcome: RpcResponseOutcome,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RpcResponseOutcome {
    Success(Value),
    Error(RpcErrorObject),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RpcNotificationEnvelope {
    pub method: String,
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RpcRequestEnvelope {
    pub id: RpcId,
    pub method: String,
    pub params: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_id_deserialization_rejects_empty_strings() {
        assert!(serde_json::from_str::<RpcId>(r#""""#).is_err());
    }
}
