use futures_util::stream;
use harness_llm::{
    BlockType, FinishEvent, FinishReason, LlmCancelFuture, LlmEventStream, LlmProvider,
    ModelRequest, SequencedStreamEvent, StreamEvent,
};
use harness_provider_protocol::{
    CapabilityDescriptor, ProviderToolOutcome, WireBlockType, WireCancelCause, WireCancelCauseKind,
    WireFinishReason, WireLlmStreamEvent, WireModelRequest, WireSideEffectClass,
};
use harness_tools::{
    IdempotencySupport, ToolCancelFuture, ToolDefinition, ToolExecutionFuture, ToolExecutor,
    ToolInvocation,
};
use harness_types::{
    CancelCause, ContentBlock, ErrorCode, InvocationId, PortableError, ProviderId, RequestId,
    SideEffectClass, StreamSeq, TokenUsage, ToolCallId, ToolOutcome,
};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::{LlmStreamItem, ProviderHost, ProviderHostError, ProviderStreamError};

/// Adapts one ready [`ProviderHost`] into the provider-neutral LLM domain seam.
///
/// The adapter owns no retry policy and no durable state. It only translates
/// domain values to Provider Protocol v1 wire values and maps the resulting
/// stream back into `harness-llm` events.
#[derive(Clone)]
pub struct ProviderHostLlmAdapter {
    host: ProviderHost,
    provider_id: ProviderId,
}

impl ProviderHostLlmAdapter {
    pub async fn new(host: ProviderHost) -> Result<Self, ProviderAdapterError> {
        let manifest = host
            .manifest()
            .await
            .ok_or(ProviderAdapterError::ManifestUnavailable)?;
        let provider_id = ProviderId::new(manifest.provider_id.as_str()).map_err(|error| {
            ProviderAdapterError::InvalidProviderId {
                value: manifest.provider_id.clone(),
                message: error.to_string(),
            }
        })?;
        Ok(Self { host, provider_id })
    }

    pub fn host(&self) -> &ProviderHost {
        &self.host
    }
}

impl LlmProvider for ProviderHostLlmAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn stream(&self, request: ModelRequest) -> LlmEventStream {
        let host = self.host.clone();
        let operation_id = request.request_id.as_str().to_owned();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        std::mem::drop(tokio::spawn(async move {
            let wire_request = match transcode::<_, WireModelRequest>(request) {
                Ok(request) => request,
                Err(error) => {
                    let _ = tx.send(Err(protocol_mapping_error(
                        "failed to encode ModelRequest for Provider Protocol",
                        error,
                    )));
                    return;
                }
            };

            let mut handle = match host.start_llm(operation_id, wire_request, None).await {
                Ok(handle) => handle,
                Err(error) => {
                    let _ = tx.send(Err(map_llm_host_error(error)));
                    return;
                }
            };

            while let Some(item) = handle.recv().await {
                let mapped = match item {
                    Ok(item) => map_llm_stream_item(item),
                    Err(error) => Err(map_stream_error(error)),
                };
                let terminal = mapped.is_err()
                    || mapped
                        .as_ref()
                        .is_ok_and(|event| matches!(&event.event, StreamEvent::Finish(_)));
                if tx.send(mapped).is_err() || terminal {
                    break;
                }
            }
        }));

        Box::pin(stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        }))
    }

    fn cancel(&self, request_id: RequestId, cause: CancelCause) -> LlmCancelFuture {
        let host = self.host.clone();
        Box::pin(async move {
            host.cancel(request_id.into_string(), wire_cancel_cause(cause))
                .await
                .map_err(map_llm_host_error)
        })
    }
}

/// Adapts one named Tool capability from a ready [`ProviderHost`] into the
/// provider-neutral Tool executor seam.
#[derive(Clone)]
pub struct ProviderHostToolAdapter {
    host: ProviderHost,
    provider_id: ProviderId,
    tool_name: String,
    version: String,
    parallel_safe: bool,
    side_effect: SideEffectClass,
    idempotency_support: IdempotencySupport,
}

impl ProviderHostToolAdapter {
    pub async fn new(
        host: ProviderHost,
        tool_name: impl Into<String>,
    ) -> Result<Self, ProviderAdapterError> {
        let tool_name = tool_name.into();
        if tool_name.is_empty() {
            return Err(ProviderAdapterError::EmptyToolName);
        }
        let manifest = host
            .manifest()
            .await
            .ok_or(ProviderAdapterError::ManifestUnavailable)?;
        let provider_id = ProviderId::new(manifest.provider_id.as_str()).map_err(|error| {
            ProviderAdapterError::InvalidProviderId {
                value: manifest.provider_id.clone(),
                message: error.to_string(),
            }
        })?;

        let descriptor = manifest
            .capabilities
            .iter()
            .find_map(|capability| match capability {
                CapabilityDescriptor::Tool {
                    name,
                    version,
                    parallel_safe,
                    side_effect,
                    supports_idempotency_key,
                } if name == &tool_name => Some((
                    version.clone(),
                    *parallel_safe,
                    *side_effect,
                    *supports_idempotency_key,
                )),
                _ => None,
            });
        let Some((version, parallel_safe, wire_side_effect, supports_idempotency_key)) = descriptor
        else {
            return Err(ProviderAdapterError::ToolNotDeclared(tool_name));
        };

        let side_effect = domain_side_effect(wire_side_effect);
        let idempotency_support = if supports_idempotency_key {
            IdempotencySupport::Keyed
        } else {
            IdempotencySupport::None
        };

        Ok(Self {
            host,
            provider_id,
            tool_name,
            version,
            parallel_safe,
            side_effect,
            idempotency_support,
        })
    }

    pub async fn from_definition(
        host: ProviderHost,
        definition: &ToolDefinition,
    ) -> Result<Self, ProviderAdapterError> {
        let adapter = Self::new(host, definition.name.clone()).await?;
        adapter.validate_definition(definition)?;
        Ok(adapter)
    }

    pub fn host(&self) -> &ProviderHost {
        &self.host
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn manifest_side_effect(&self) -> SideEffectClass {
        self.side_effect
    }

    /// Verifies that the Core-authoritative ToolDefinition is bound to the same
    /// execution semantics declared by the provider manifest.
    pub fn validate_definition(
        &self,
        definition: &ToolDefinition,
    ) -> Result<(), ProviderAdapterError> {
        if definition.name != self.tool_name {
            return Err(ProviderAdapterError::DefinitionMismatch {
                tool: self.tool_name.clone(),
                field: "name",
                core: definition.name.clone(),
                provider: self.tool_name.clone(),
            });
        }
        if definition.version != self.version {
            return Err(ProviderAdapterError::DefinitionMismatch {
                tool: self.tool_name.clone(),
                field: "version",
                core: definition.version.clone(),
                provider: self.version.clone(),
            });
        }
        if definition.parallel_safe != self.parallel_safe {
            return Err(ProviderAdapterError::DefinitionMismatch {
                tool: self.tool_name.clone(),
                field: "parallelSafe",
                core: definition.parallel_safe.to_string(),
                provider: self.parallel_safe.to_string(),
            });
        }
        if definition.side_effect != self.side_effect {
            return Err(ProviderAdapterError::DefinitionMismatch {
                tool: self.tool_name.clone(),
                field: "sideEffect",
                core: format!("{:?}", definition.side_effect),
                provider: format!("{:?}", self.side_effect),
            });
        }
        Ok(())
    }
}

impl ToolExecutor for ProviderHostToolAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn idempotency_support(&self) -> IdempotencySupport {
        self.idempotency_support
    }

    fn invoke(&self, invocation: ToolInvocation) -> ToolExecutionFuture {
        let host = self.host.clone();
        let expected_tool = self.tool_name.clone();
        Box::pin(async move {
            if invocation.tool_name != expected_tool {
                return Err(PortableError::new(
                    ErrorCode::InvalidArgument,
                    format!(
                        "ProviderHostToolAdapter is bound to {expected_tool}, received {}",
                        invocation.tool_name
                    ),
                ));
            }

            let params = harness_provider_protocol::ToolInvokeParams {
                operation_id: invocation.invocation_id.as_str().to_owned(),
                invocation_id: invocation.invocation_id.as_str().to_owned(),
                call_id: invocation.call_id.as_str().to_owned(),
                session_id: invocation.session_id.as_str().to_owned(),
                tool: invocation.tool_name,
                arguments_json: invocation.arguments_json.into_string(),
                attempt: invocation.attempt,
                idempotency_key: invocation.idempotency_key.into_string(),
                deadline: None,
            };

            // Keep the Host RPC task alive even if Agent timeout/cancellation
            // drops this ToolExecutionFuture. That guarantees ProviderHost can
            // retire/correlate the eventual RPC response instead of leaking a
            // pending request entry.
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            std::mem::drop(tokio::spawn(async move {
                let result = host.invoke_tool(params).await;
                let _ = reply_tx.send(result);
            }));

            let result = reply_rx
                .await
                .map_err(|_| {
                    PortableError::new(
                        ErrorCode::ProviderUnavailable,
                        "ProviderHost Tool RPC task ended without a result",
                    )
                })?
                .map_err(map_tool_host_error)?;
            map_tool_outcome(result.outcome)
        })
    }

    fn cancel(&self, invocation_id: InvocationId, cause: CancelCause) -> ToolCancelFuture {
        let host = self.host.clone();
        Box::pin(async move {
            host.cancel(invocation_id.into_string(), wire_cancel_cause(cause))
                .await
                .map_err(map_tool_host_error)
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ProviderAdapterError {
    #[error("provider manifest is not available")]
    ManifestUnavailable,

    #[error("provider manifest providerId {value:?} cannot be represented by Harness: {message}")]
    InvalidProviderId { value: String, message: String },

    #[error("Tool adapter name must not be empty")]
    EmptyToolName,

    #[error("provider manifest does not declare Tool capability {0}")]
    ToolNotDeclared(String),

    #[error(
        "Tool definition mismatch for {tool}: field {field} is {core:?} in Core but {provider:?} in provider manifest"
    )]
    DefinitionMismatch {
        tool: String,
        field: &'static str,
        core: String,
        provider: String,
    },
}

fn map_llm_stream_item(item: LlmStreamItem) -> Result<SequencedStreamEvent, PortableError> {
    let seq = StreamSeq::new(item.seq).map_err(|error| {
        PortableError::new(
            ErrorCode::ProviderProtocolError,
            format!("provider LLM stream sequence cannot be represented by Harness: {error}"),
        )
    })?;
    let event = match item.event {
        WireLlmStreamEvent::BlockStart { index, block_type } => StreamEvent::BlockStart {
            index,
            block_type: match block_type {
                WireBlockType::Text => BlockType::Text,
                WireBlockType::Reasoning => BlockType::Reasoning,
                WireBlockType::ToolCall => BlockType::ToolCall,
            },
        },
        WireLlmStreamEvent::TextDelta { index, text } => StreamEvent::TextDelta { index, text },
        WireLlmStreamEvent::ReasoningDelta { index, text } => {
            StreamEvent::ReasoningDelta { index, text }
        }
        WireLlmStreamEvent::ToolCallDelta {
            index,
            call_id,
            name,
            arguments_delta,
        } => StreamEvent::ToolCallDelta {
            index,
            call_id: ToolCallId::new(call_id).map_err(|error| {
                PortableError::new(
                    ErrorCode::ProviderProtocolError,
                    format!("provider emitted invalid ToolCallId: {error}"),
                )
            })?,
            name,
            arguments_delta,
        },
        WireLlmStreamEvent::BlockEnd { index, block } => StreamEvent::BlockEnd {
            index,
            block: transcode::<_, ContentBlock>(block).map_err(|error| {
                protocol_mapping_error("failed to decode provider ContentBlock", error)
            })?,
        },
        WireLlmStreamEvent::Usage { usage } => {
            StreamEvent::Usage(transcode::<_, TokenUsage>(usage).map_err(|error| {
                protocol_mapping_error("failed to decode provider token usage", error)
            })?)
        }
        WireLlmStreamEvent::Finish { reason, failure } => {
            let failure = failure
                .map(transcode::<_, PortableError>)
                .transpose()
                .map_err(|error| {
                    protocol_mapping_error("failed to decode provider failure", error)
                })?;
            StreamEvent::Finish(FinishEvent {
                reason: match reason {
                    WireFinishReason::Completed => FinishReason::Completed,
                    WireFinishReason::MaxTokens => FinishReason::MaxTokens,
                    WireFinishReason::Error => FinishReason::Error,
                    WireFinishReason::Cancelled => FinishReason::Cancelled,
                },
                failure,
            })
        }
    };
    Ok(SequencedStreamEvent::new(seq, event))
}

fn map_tool_outcome(outcome: ProviderToolOutcome) -> Result<ToolOutcome, PortableError> {
    match outcome {
        ProviderToolOutcome::Success { content } => Ok(ToolOutcome::Success {
            content: transcode_content(content)?,
        }),
        ProviderToolOutcome::Error {
            code,
            message,
            content,
        } => Ok(ToolOutcome::Error {
            code,
            message,
            content: transcode_content(content)?,
        }),
        ProviderToolOutcome::Cancelled { cause } => Ok(ToolOutcome::Cancelled {
            cause: domain_cancel_cause(cause),
        }),
    }
}

fn transcode_content(
    content: Vec<harness_provider_protocol::WireContentBlock>,
) -> Result<Vec<ContentBlock>, PortableError> {
    content
        .into_iter()
        .map(|block| {
            transcode(block).map_err(|error| {
                protocol_mapping_error("failed to decode provider Tool content", error)
            })
        })
        .collect()
}

fn transcode<S, D>(value: S) -> Result<D, serde_json::Error>
where
    S: Serialize,
    D: DeserializeOwned,
{
    serde_json::from_value(serde_json::to_value(value)?)
}

fn protocol_mapping_error(context: &str, error: serde_json::Error) -> PortableError {
    PortableError::new(
        ErrorCode::ProviderProtocolError,
        format!("{context}: {error}"),
    )
}

fn map_stream_error(error: ProviderStreamError) -> PortableError {
    match error {
        ProviderStreamError::Protocol(message) => {
            PortableError::new(ErrorCode::ProviderProtocolError, message)
        }
        ProviderStreamError::ProviderUnavailable(message) => {
            PortableError::new(ErrorCode::ProviderUnavailable, message)
        }
    }
}

fn map_llm_host_error(error: ProviderHostError) -> PortableError {
    map_host_error(error, ErrorCode::ModelRequestFailed)
}

fn map_tool_host_error(error: ProviderHostError) -> PortableError {
    map_host_error(error, ErrorCode::ToolExecutionFailed)
}

fn map_host_error(error: ProviderHostError, provider_error_code: ErrorCode) -> PortableError {
    let code = match &error {
        ProviderHostError::Protocol(_)
        | ProviderHostError::DeserializeResponse(_)
        | ProviderHostError::UnexpectedResponse(_) => ErrorCode::ProviderProtocolError,
        ProviderHostError::ProviderUnavailable(_)
        | ProviderHostError::Io(_)
        | ProviderHostError::Spawn(_)
        | ProviderHostError::MissingPipe(_)
        | ProviderHostError::InvalidState { .. } => ErrorCode::ProviderUnavailable,
        ProviderHostError::RequestTimeout { .. } => ErrorCode::DeadlineExceeded,
        ProviderHostError::InvalidRequest(_)
        | ProviderHostError::InvalidConfig(_)
        | ProviderHostError::InvalidManifest(_) => ErrorCode::InvalidArgument,
        ProviderHostError::Rpc { code, .. } if *code == -32602 => ErrorCode::InvalidArgument,
        ProviderHostError::Rpc { code, .. } if *code == -32601 => ErrorCode::NotFound,
        ProviderHostError::StreamRejected(_) | ProviderHostError::Rpc { .. } => provider_error_code,
        ProviderHostError::Serialize(_) => ErrorCode::Internal,
    };
    PortableError::new(code, error.to_string())
}

fn wire_cancel_cause(cause: CancelCause) -> WireCancelCause {
    WireCancelCause {
        kind: match cause {
            CancelCause::User => WireCancelCauseKind::User,
            CancelCause::Parent => WireCancelCauseKind::Parent,
            CancelCause::Timeout => WireCancelCauseKind::Timeout,
            CancelCause::Policy => WireCancelCauseKind::Policy,
            CancelCause::Shutdown => WireCancelCauseKind::Shutdown,
            CancelCause::Disposed => WireCancelCauseKind::Disposed,
        },
    }
}

fn domain_cancel_cause(cause: WireCancelCauseKind) -> CancelCause {
    match cause {
        WireCancelCauseKind::User => CancelCause::User,
        WireCancelCauseKind::Parent => CancelCause::Parent,
        WireCancelCauseKind::Timeout => CancelCause::Timeout,
        WireCancelCauseKind::Policy => CancelCause::Policy,
        WireCancelCauseKind::Shutdown => CancelCause::Shutdown,
        WireCancelCauseKind::Disposed => CancelCause::Disposed,
    }
}

fn domain_side_effect(side_effect: WireSideEffectClass) -> SideEffectClass {
    match side_effect {
        WireSideEffectClass::ReadOnly => SideEffectClass::ReadOnly,
        WireSideEffectClass::IdempotentWrite => SideEffectClass::IdempotentWrite,
        WireSideEffectClass::NonIdempotentWrite => SideEffectClass::NonIdempotentWrite,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_provider_protocol::WireContentBlock;

    #[test]
    fn content_transcode_preserves_arguments_json_string() {
        let block = WireContentBlock::ToolCall {
            id: "call_1".to_owned(),
            name: "echo".to_owned(),
            arguments_json: r#"{"text":"hello"}"#.to_owned(),
        };
        let mapped: ContentBlock = transcode(block).unwrap();
        assert!(matches!(
            mapped,
            ContentBlock::ToolCall {
                id,
                name,
                arguments_json,
            } if id.as_str() == "call_1"
                && name == "echo"
                && arguments_json.as_str() == r#"{"text":"hello"}"#
        ));
    }
}
