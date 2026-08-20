use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::{
    InboundMessage, JSONRPC_VERSION, RpcErrorObject, RpcId, RpcNotificationEnvelope,
    RpcRequestEnvelope, RpcResponseEnvelope, RpcResponseOutcome,
};

pub fn encode_ndjson<T: Serialize>(message: &T) -> Result<Vec<u8>, ProtocolCodecError> {
    let mut encoded = serde_json::to_vec(message).map_err(ProtocolCodecError::Serialize)?;
    encoded.push(b'\n');
    Ok(encoded)
}

pub fn decode_inbound_line(line: &str) -> Result<InboundMessage, ProtocolCodecError> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.is_empty() {
        return Err(ProtocolCodecError::EmptyFrame);
    }
    if line.contains('\n') || line.contains('\r') {
        return Err(ProtocolCodecError::MultipleFrames);
    }

    let value: Value = serde_json::from_str(line).map_err(ProtocolCodecError::Deserialize)?;
    let object = value
        .as_object()
        .ok_or(ProtocolCodecError::EnvelopeMustBeObject)?;

    match object.get("jsonrpc").and_then(Value::as_str) {
        Some(version) if version == JSONRPC_VERSION => {}
        _ => return Err(ProtocolCodecError::InvalidJsonRpcVersion),
    }

    let method = object.get("method").and_then(Value::as_str);
    let id = object.get("id");

    if let Some(method) = method {
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        if let Some(id) = id {
            return Ok(InboundMessage::Request(RpcRequestEnvelope {
                id: parse_rpc_id(id)?,
                method: non_empty_method(method)?,
                params,
            }));
        }
        return Ok(InboundMessage::Notification(RpcNotificationEnvelope {
            method: non_empty_method(method)?,
            params,
        }));
    }

    let id = id.ok_or(ProtocolCodecError::MissingResponseId)?;
    let id = parse_rpc_id(id)?;
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");
    if has_result == has_error {
        return Err(ProtocolCodecError::InvalidResponseShape);
    }

    let outcome = if has_result {
        RpcResponseOutcome::Success(object.get("result").cloned().unwrap_or(Value::Null))
    } else {
        let error = serde_json::from_value::<RpcErrorObject>(
            object.get("error").cloned().unwrap_or(Value::Null),
        )
        .map_err(ProtocolCodecError::Deserialize)?;
        RpcResponseOutcome::Error(error)
    };

    Ok(InboundMessage::Response(RpcResponseEnvelope {
        id,
        outcome,
    }))
}

fn parse_rpc_id(value: &Value) -> Result<RpcId, ProtocolCodecError> {
    let value = value
        .as_str()
        .ok_or(ProtocolCodecError::RpcIdMustBeString)?;
    RpcId::new(value).map_err(|_| ProtocolCodecError::RpcIdMustBeString)
}

fn non_empty_method(method: &str) -> Result<String, ProtocolCodecError> {
    if method.is_empty() {
        return Err(ProtocolCodecError::EmptyMethod);
    }
    Ok(method.to_owned())
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProtocolCodecError {
    #[error("cannot serialize JSON-RPC message: {0}")]
    Serialize(serde_json::Error),

    #[error("cannot decode JSON-RPC message: {0}")]
    Deserialize(serde_json::Error),

    #[error("NDJSON frame is empty")]
    EmptyFrame,

    #[error("one NDJSON decode call may contain exactly one physical line")]
    MultipleFrames,

    #[error("JSON-RPC envelope must be a JSON object")]
    EnvelopeMustBeObject,

    #[error("JSON-RPC version must equal 2.0")]
    InvalidJsonRpcVersion,

    #[error("JSON-RPC response is missing id")]
    MissingResponseId,

    #[error("Provider Protocol v1 requires non-empty string JSON-RPC ids")]
    RpcIdMustBeString,

    #[error("JSON-RPC method must not be empty")]
    EmptyMethod,

    #[error("JSON-RPC response must contain exactly one of result or error")]
    InvalidResponseShape,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{RpcId, RpcRequest, RpcResponseOutcome};

    #[test]
    fn encodes_exactly_one_ndjson_frame() {
        let request = RpcRequest::new(RpcId::new("rpc_1").unwrap(), "provider.ping", json!({}));
        let bytes = encode_ndjson(&request).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
    }

    #[test]
    fn decodes_success_response_with_null_result() {
        let decoded =
            decode_inbound_line(r#"{"jsonrpc":"2.0","id":"rpc_1","result":null}"#).unwrap();
        let InboundMessage::Response(response) = decoded else {
            panic!("expected response");
        };
        assert_eq!(response.id.as_str(), "rpc_1");
        assert!(matches!(
            response.outcome,
            RpcResponseOutcome::Success(Value::Null)
        ));
    }

    #[test]
    fn rejects_numeric_rpc_ids() {
        assert!(decode_inbound_line(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).is_err());
    }
}
