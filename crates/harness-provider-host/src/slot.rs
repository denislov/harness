use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use futures_util::{StreamExt, stream};
use harness_llm::{LlmCancelFuture, LlmEventStream, LlmProvider, ModelRequest};
use harness_provider_protocol::{CapabilityDescriptor, ProviderManifest, WireSideEffectClass};
use harness_tools::{
    IdempotencySupport, ToolCancelFuture, ToolDefinition, ToolExecutionFuture, ToolExecutor,
    ToolInvocation,
};
use harness_types::{CancelCause, ErrorCode, InvocationId, PortableError, ProviderId, RequestId};
use thiserror::Error;
use tokio::sync::watch;

use crate::{
    ProviderAdapterError, ProviderHost, ProviderHostLlmAdapter, ProviderHostToolAdapter,
    ProviderState,
};

#[derive(Clone)]
pub struct ProviderGeneration {
    generation: u32,
    host: ProviderHost,
    manifest: ProviderManifest,
}

impl ProviderGeneration {
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    pub fn host(&self) -> &ProviderHost {
        &self.host
    }

    pub fn manifest(&self) -> &ProviderManifest {
        &self.manifest
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderSlotStatus {
    Ready { generation: u32 },
    Unavailable { generation: u32 },
    Quarantined { generation: u32 },
    Stopped { generation: u32 },
}

#[derive(Clone)]
enum ProviderSlotState {
    Ready(ProviderGeneration),
    Unavailable { generation: u32, host: ProviderHost },
    Quarantined { generation: u32 },
    Stopped { generation: u32 },
}

#[derive(Clone)]
pub struct ProviderSlot {
    provider_id: ProviderId,
    baseline_manifest: ProviderManifest,
    state: watch::Sender<ProviderSlotState>,
}

impl ProviderSlot {
    pub fn new(
        provider_id: ProviderId,
        host: ProviderHost,
        manifest: ProviderManifest,
    ) -> Result<Self, ProviderSlotError> {
        validate_slot_manifest(&provider_id, &manifest)?;
        let baseline_manifest = manifest.clone();
        let (state, _) = watch::channel(ProviderSlotState::Ready(ProviderGeneration {
            generation: 1,
            host,
            manifest,
        }));
        Ok(Self {
            provider_id,
            baseline_manifest,
            state,
        })
    }

    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn baseline_manifest(&self) -> &ProviderManifest {
        &self.baseline_manifest
    }

    pub fn manifest_compatible(&self, candidate: &ProviderManifest) -> bool {
        candidate.validate().is_ok()
            && manifests_semantically_equal(&self.baseline_manifest, candidate)
    }

    pub fn status(&self) -> ProviderSlotStatus {
        status_of(&self.state.borrow())
    }

    pub fn current(&self) -> Option<ProviderGeneration> {
        match &*self.state.borrow() {
            ProviderSlotState::Ready(generation) => Some(generation.clone()),
            ProviderSlotState::Unavailable { .. }
            | ProviderSlotState::Quarantined { .. }
            | ProviderSlotState::Stopped { .. } => None,
        }
    }

    pub fn host_for_shutdown(&self) -> Option<ProviderHost> {
        match &*self.state.borrow() {
            ProviderSlotState::Ready(generation) => Some(generation.host.clone()),
            ProviderSlotState::Unavailable { host, .. } => Some(host.clone()),
            ProviderSlotState::Quarantined { .. } | ProviderSlotState::Stopped { .. } => None,
        }
    }

    pub fn mark_unavailable(&self, generation: u32) -> bool {
        let current = self.state.borrow().clone();
        match current {
            ProviderSlotState::Ready(active) if active.generation == generation => {
                self.state.send_replace(ProviderSlotState::Unavailable {
                    generation,
                    host: active.host,
                });
                true
            }
            _ => false,
        }
    }

    pub fn replace(
        &self,
        generation: u32,
        host: ProviderHost,
        manifest: ProviderManifest,
    ) -> Result<(), ProviderSlotError> {
        validate_slot_manifest(&self.provider_id, &manifest)?;
        if !self.manifest_compatible(&manifest) {
            return Err(ProviderSlotError::ManifestDrift);
        }
        let current = self.status();
        let expected = current
            .generation()
            .checked_add(1)
            .ok_or(ProviderSlotError::GenerationExhausted)?;
        if generation != expected {
            return Err(ProviderSlotError::InvalidGeneration {
                expected,
                actual: generation,
            });
        }
        if !matches!(current, ProviderSlotStatus::Unavailable { .. }) {
            return Err(ProviderSlotError::InvalidTransition {
                from: current,
                to: "ready",
            });
        }
        self.state
            .send_replace(ProviderSlotState::Ready(ProviderGeneration {
                generation,
                host,
                manifest,
            }));
        Ok(())
    }

    pub fn quarantine(&self, generation: u32) -> bool {
        let current = self.status();
        if current.generation() != generation
            || matches!(current, ProviderSlotStatus::Stopped { .. })
        {
            return false;
        }
        self.state
            .send_replace(ProviderSlotState::Quarantined { generation });
        true
    }

    pub fn mark_stopped(&self) {
        let generation = self.status().generation();
        self.state
            .send_replace(ProviderSlotState::Stopped { generation });
    }

    pub async fn wait_ready(&self) -> Result<ProviderGeneration, ProviderSlotError> {
        let mut receiver = self.state.subscribe();
        loop {
            let state = receiver.borrow().clone();
            match state {
                ProviderSlotState::Ready(generation) => return Ok(generation),
                ProviderSlotState::Unavailable { .. } => {}
                ProviderSlotState::Quarantined { generation } => {
                    return Err(ProviderSlotError::Quarantined { generation });
                }
                ProviderSlotState::Stopped { generation } => {
                    return Err(ProviderSlotError::Stopped { generation });
                }
            }
            receiver
                .changed()
                .await
                .map_err(|_| ProviderSlotError::Closed)?;
        }
    }
}

impl ProviderSlotStatus {
    pub const fn generation(self) -> u32 {
        match self {
            Self::Ready { generation }
            | Self::Unavailable { generation }
            | Self::Quarantined { generation }
            | Self::Stopped { generation } => generation,
        }
    }
}

fn status_of(state: &ProviderSlotState) -> ProviderSlotStatus {
    match state {
        ProviderSlotState::Ready(generation) => ProviderSlotStatus::Ready {
            generation: generation.generation,
        },
        ProviderSlotState::Unavailable { generation, .. } => ProviderSlotStatus::Unavailable {
            generation: *generation,
        },
        ProviderSlotState::Quarantined { generation } => ProviderSlotStatus::Quarantined {
            generation: *generation,
        },
        ProviderSlotState::Stopped { generation } => ProviderSlotStatus::Stopped {
            generation: *generation,
        },
    }
}

fn validate_slot_manifest(
    expected: &ProviderId,
    manifest: &ProviderManifest,
) -> Result<(), ProviderSlotError> {
    manifest
        .validate()
        .map_err(|error| ProviderSlotError::InvalidManifest(error.to_string()))?;
    let actual = ProviderId::new(manifest.provider_id.clone()).map_err(|error| {
        ProviderSlotError::InvalidManifestProviderId {
            value: manifest.provider_id.clone(),
            message: error.to_string(),
        }
    })?;
    if &actual != expected {
        return Err(ProviderSlotError::IdentityMismatch {
            expected: expected.clone(),
            actual,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ProviderSlotError {
    #[error("provider slot manifest is invalid: {0}")]
    InvalidManifest(String),

    #[error("provider manifest providerId {value:?} cannot be represented by Harness: {message}")]
    InvalidManifestProviderId { value: String, message: String },

    #[error("provider slot identity mismatch: expected {expected}, manifest declared {actual}")]
    IdentityMismatch {
        expected: ProviderId,
        actual: ProviderId,
    },

    #[error("provider restart manifest is not semantically compatible with the slot baseline")]
    ManifestDrift,

    #[error("provider slot generation is exhausted")]
    GenerationExhausted,

    #[error("provider slot expected generation {expected}, received {actual}")]
    InvalidGeneration { expected: u32, actual: u32 },

    #[error("provider slot cannot transition from {from:?} to {to}")]
    InvalidTransition {
        from: ProviderSlotStatus,
        to: &'static str,
    },

    #[error("provider generation {generation} is quarantined")]
    Quarantined { generation: u32 },

    #[error("provider generation {generation} is stopped")]
    Stopped { generation: u32 },

    #[error("provider slot state channel is closed")]
    Closed,
}

#[derive(Clone)]
pub struct ProviderSlotLlmAdapter {
    slot: ProviderSlot,
    provider_id: ProviderId,
    active_requests: Arc<Mutex<BTreeMap<RequestId, ProviderHost>>>,
}

impl ProviderSlotLlmAdapter {
    pub fn new(slot: ProviderSlot) -> Self {
        let provider_id = slot.provider_id().clone();
        Self {
            slot,
            provider_id,
            active_requests: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn slot(&self) -> &ProviderSlot {
        &self.slot
    }
}

impl LlmProvider for ProviderSlotLlmAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn stream(&self, request: ModelRequest) -> LlmEventStream {
        let slot = self.slot.clone();
        let active_requests = self.active_requests.clone();
        let request_id = request.request_id.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        std::mem::drop(tokio::spawn(async move {
            let generation = tokio::select! {
                _ = tx.closed() => return,
                result = slot.wait_ready() => match result {
                    Ok(generation) => generation,
                    Err(error) => {
                        let _ = tx.send(Err(slot_error_to_portable(error)));
                        return;
                    }
                },
            };
            let generation_no = generation.generation();
            let host = generation.host().clone();
            let adapter = match ProviderHostLlmAdapter::new(host.clone()).await {
                Ok(adapter) => adapter,
                Err(error) => {
                    let _ = slot.mark_unavailable(generation_no);
                    let _ = tx.send(Err(adapter_error_to_portable(error)));
                    return;
                }
            };
            if tx.is_closed() {
                return;
            }
            active_requests
                .lock()
                .expect("ProviderSlotLlmAdapter active request mutex is not poisoned")
                .insert(request_id.clone(), host);
            let _active = ActiveHostGuard::new(active_requests, request_id);
            let mut upstream = adapter.stream(request);
            while let Some(item) = upstream.next().await {
                if item.is_err() && generation.host().state().await != ProviderState::Ready {
                    let _ = slot.mark_unavailable(generation_no);
                }
                if tx.send(item).is_err() {
                    return;
                }
            }
        }));
        Box::pin(stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        }))
    }

    fn cancel(&self, request_id: RequestId, cause: CancelCause) -> LlmCancelFuture {
        let host = self
            .active_requests
            .lock()
            .expect("ProviderSlotLlmAdapter active request mutex is not poisoned")
            .get(&request_id)
            .cloned();
        Box::pin(async move {
            let Some(host) = host else {
                return Ok(());
            };
            let adapter = ProviderHostLlmAdapter::new(host)
                .await
                .map_err(adapter_error_to_portable)?;
            adapter.cancel(request_id, cause).await
        })
    }
}

#[derive(Clone)]
pub struct ProviderSlotToolAdapter {
    slot: ProviderSlot,
    provider_id: ProviderId,
    definition: ToolDefinition,
    idempotency_support: IdempotencySupport,
    active_invocations: Arc<Mutex<BTreeMap<InvocationId, ProviderHost>>>,
}

impl ProviderSlotToolAdapter {
    pub fn from_definition(
        slot: ProviderSlot,
        definition: &ToolDefinition,
    ) -> Result<Self, ProviderAdapterError> {
        let idempotency_support = validate_tool_manifest(slot.baseline_manifest(), definition)?;
        Ok(Self {
            provider_id: slot.provider_id().clone(),
            slot,
            definition: definition.clone(),
            idempotency_support,
            active_invocations: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn slot(&self) -> &ProviderSlot {
        &self.slot
    }
}

impl ToolExecutor for ProviderSlotToolAdapter {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn idempotency_support(&self) -> IdempotencySupport {
        self.idempotency_support
    }

    fn invoke(&self, invocation: ToolInvocation) -> ToolExecutionFuture {
        let slot = self.slot.clone();
        let definition = self.definition.clone();
        let active_invocations = self.active_invocations.clone();
        let invocation_id = invocation.invocation_id.clone();
        Box::pin(async move {
            let generation = slot.wait_ready().await.map_err(slot_error_to_portable)?;
            let generation_no = generation.generation();
            let host = generation.host().clone();
            let adapter =
                match ProviderHostToolAdapter::from_definition(host.clone(), &definition).await {
                    Ok(adapter) => adapter,
                    Err(error) => {
                        let _ = slot.mark_unavailable(generation_no);
                        return Err(adapter_error_to_portable(error));
                    }
                };
            active_invocations
                .lock()
                .expect("ProviderSlotToolAdapter active invocation mutex is not poisoned")
                .insert(invocation_id.clone(), host);
            let _active = ActiveHostGuard::new(active_invocations, invocation_id);
            let outcome = adapter.invoke(invocation).await;
            if outcome.is_err() && generation.host().state().await != ProviderState::Ready {
                let _ = slot.mark_unavailable(generation_no);
            }
            outcome
        })
    }

    fn cancel(&self, invocation_id: InvocationId, cause: CancelCause) -> ToolCancelFuture {
        let definition = self.definition.clone();
        let host = self
            .active_invocations
            .lock()
            .expect("ProviderSlotToolAdapter active invocation mutex is not poisoned")
            .get(&invocation_id)
            .cloned();
        Box::pin(async move {
            let Some(host) = host else {
                return Ok(());
            };
            let adapter = ProviderHostToolAdapter::from_definition(host, &definition)
                .await
                .map_err(adapter_error_to_portable)?;
            adapter.cancel(invocation_id, cause).await
        })
    }
}

struct ActiveHostGuard<K>
where
    K: Ord,
{
    active: Arc<Mutex<BTreeMap<K, ProviderHost>>>,
    key: K,
}

impl<K> ActiveHostGuard<K>
where
    K: Ord,
{
    fn new(active: Arc<Mutex<BTreeMap<K, ProviderHost>>>, key: K) -> Self {
        Self { active, key }
    }
}

impl<K> Drop for ActiveHostGuard<K>
where
    K: Ord,
{
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            let _ = active.remove(&self.key);
        }
    }
}

fn validate_tool_manifest(
    manifest: &ProviderManifest,
    definition: &ToolDefinition,
) -> Result<IdempotencySupport, ProviderAdapterError> {
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
            } if name == &definition.name => Some((
                version,
                *parallel_safe,
                *side_effect,
                *supports_idempotency_key,
            )),
            _ => None,
        });
    let Some((version, parallel_safe, side_effect, supports_idempotency_key)) = descriptor else {
        return Err(ProviderAdapterError::ToolNotDeclared(
            definition.name.clone(),
        ));
    };
    let provider_side_effect = match side_effect {
        WireSideEffectClass::ReadOnly => harness_types::SideEffectClass::ReadOnly,
        WireSideEffectClass::IdempotentWrite => harness_types::SideEffectClass::IdempotentWrite,
        WireSideEffectClass::NonIdempotentWrite => {
            harness_types::SideEffectClass::NonIdempotentWrite
        }
    };
    if version != &definition.version {
        return Err(ProviderAdapterError::DefinitionMismatch {
            tool: definition.name.clone(),
            field: "version",
            core: definition.version.clone(),
            provider: version.clone(),
        });
    }
    if parallel_safe != definition.parallel_safe {
        return Err(ProviderAdapterError::DefinitionMismatch {
            tool: definition.name.clone(),
            field: "parallelSafe",
            core: definition.parallel_safe.to_string(),
            provider: parallel_safe.to_string(),
        });
    }
    if provider_side_effect != definition.side_effect {
        return Err(ProviderAdapterError::DefinitionMismatch {
            tool: definition.name.clone(),
            field: "sideEffect",
            core: format!("{:?}", definition.side_effect),
            provider: format!("{:?}", provider_side_effect),
        });
    }
    Ok(if supports_idempotency_key {
        IdempotencySupport::Keyed
    } else {
        IdempotencySupport::None
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolManifestShape {
    version: String,
    parallel_safe: bool,
    side_effect: WireSideEffectClass,
    supports_idempotency_key: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ManifestShape {
    provider_id: String,
    provider_version: String,
    protocol_version: String,
    tools: std::collections::BTreeMap<String, ToolManifestShape>,
    models: std::collections::BTreeSet<String>,
}

fn manifests_semantically_equal(left: &ProviderManifest, right: &ProviderManifest) -> bool {
    manifest_shape(left) == manifest_shape(right)
}

fn manifest_shape(manifest: &ProviderManifest) -> ManifestShape {
    let mut tools = std::collections::BTreeMap::new();
    let mut models = std::collections::BTreeSet::new();
    for capability in &manifest.capabilities {
        match capability {
            CapabilityDescriptor::Tool {
                name,
                version,
                parallel_safe,
                side_effect,
                supports_idempotency_key,
            } => {
                let _ = tools.insert(
                    name.clone(),
                    ToolManifestShape {
                        version: version.clone(),
                        parallel_safe: *parallel_safe,
                        side_effect: *side_effect,
                        supports_idempotency_key: *supports_idempotency_key,
                    },
                );
            }
            CapabilityDescriptor::Llm { models: declared } => {
                models.extend(declared.iter().cloned());
            }
        }
    }
    ManifestShape {
        provider_id: manifest.provider_id.clone(),
        provider_version: manifest.provider_version.clone(),
        protocol_version: manifest.protocol_version.clone(),
        tools,
        models,
    }
}

fn slot_error_to_portable(error: ProviderSlotError) -> PortableError {
    PortableError::new(ErrorCode::ProviderUnavailable, error.to_string())
}

fn adapter_error_to_portable(error: ProviderAdapterError) -> PortableError {
    PortableError::new(
        ErrorCode::ProviderUnavailable,
        format!("provider generation adapter is unavailable: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use harness_provider_protocol::PROTOCOL_VERSION;

    use super::*;

    #[test]
    fn manifest_compatibility_ignores_capability_and_model_order() {
        let tool = CapabilityDescriptor::Tool {
            name: "echo".to_owned(),
            version: "1".to_owned(),
            parallel_safe: true,
            side_effect: WireSideEffectClass::ReadOnly,
            supports_idempotency_key: false,
        };
        let left = ProviderManifest {
            provider_id: "provider".to_owned(),
            provider_version: "1.0.0".to_owned(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            capabilities: vec![
                tool.clone(),
                CapabilityDescriptor::Llm {
                    models: vec!["b".to_owned(), "a".to_owned()],
                },
            ],
        };
        let right = ProviderManifest {
            provider_id: "provider".to_owned(),
            provider_version: "1.0.0".to_owned(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            capabilities: vec![
                CapabilityDescriptor::Llm {
                    models: vec!["a".to_owned(), "b".to_owned()],
                },
                tool,
            ],
        };
        assert!(manifests_semantically_equal(&left, &right));
    }
}
