use harness_agent::{AgentBootstrap, AgentBootstrapper, AgentEventSource};
use harness_llm::ModelOptions;
use harness_session::{
    CompositionActivated, NewSessionEvent, SessionEvent, SessionEventPayload, SessionProjector,
    SessionStore, V1SessionProjector,
};
use harness_storage::BlobStore;
use harness_tools::ToolDefinition;
use harness_types::{BlobRef, ProviderId, SessionId};
use serde::{Deserialize, Serialize};

use crate::HarnessRuntimeError;

pub const EXECUTION_COMPOSITION_SCHEMA_VERSION: u16 = 1;
pub const EXECUTION_COMPOSITION_MEDIA_TYPE: &str =
    "application/vnd.harness.execution-composition+json;version=1";

/// Immutable, provider-neutral description of the execution semantics compiled
/// for one Agent profile. The serialized bytes are persisted in BlobStore and a
/// `composition/activated` SessionEvent selects the snapshot that owns future
/// execution until a later quiescent activation supersedes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionCompositionSnapshot {
    pub schema_version: u16,
    pub profile: String,
    pub model: ExecutionModelComposition,
    pub tools: Vec<ExecutionToolComposition>,
    pub policy_identity: String,
    pub max_automatic_tool_attempts: u32,
}

impl ExecutionCompositionSnapshot {
    pub fn snapshot_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionModelComposition {
    pub provider: ProviderId,
    pub provider_version: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub options: ModelOptions,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionToolComposition {
    pub definition: ToolDefinition,
    pub provider: ProviderId,
    pub provider_version: String,
    pub supports_idempotency_key: bool,
    pub validator_identity: String,
}

pub(crate) async fn reconcile_session_composition(
    store: &dyn SessionStore,
    event_source: &dyn AgentEventSource,
    blob_store: &dyn BlobStore,
    session_id: &SessionId,
    profile_name: &str,
    requested: &BlobRef,
    bootstrap_page_size: usize,
) -> Result<(), HarnessRuntimeError> {
    let bootstrap = AgentBootstrapper::new(V1SessionProjector, bootstrap_page_size)
        .load(store, session_id)
        .await
        .map_err(|source| HarnessRuntimeError::CompositionBootstrap {
            session_id: session_id.clone(),
            source: Box::new(source),
        })?;

    if let Some(active) = bootstrap.projection.active_composition.as_ref() {
        blob_store
            .verify(&active.data.snapshot)
            .await
            .map_err(|source| HarnessRuntimeError::CompositionSnapshotVerify {
                session_id: session_id.clone(),
                source: Box::new(source),
            })?;
    }

    match bootstrap.projection.active_composition.as_ref() {
        Some(active)
            if active.data.profile == profile_name
                && same_snapshot(&active.data.snapshot, requested) =>
        {
            Ok(())
        }
        Some(active) if !bootstrap.resume.is_clean() => {
            Err(HarnessRuntimeError::CompositionDrift {
                session_id: session_id.clone(),
                active_profile: active.data.profile.clone(),
                active_sha256: active.data.snapshot.sha256.to_string(),
                requested_profile: profile_name.to_owned(),
                requested_sha256: requested.sha256.to_string(),
            })
        }
        None if !bootstrap.resume.is_clean() => {
            Err(HarnessRuntimeError::LegacyCompositionUnbound {
                session_id: session_id.clone(),
                requested_profile: profile_name.to_owned(),
            })
        }
        _ => {
            append_activation(
                store,
                event_source,
                session_id,
                profile_name,
                requested,
                bootstrap,
            )
            .await
        }
    }
}

fn same_snapshot(left: &BlobRef, right: &BlobRef) -> bool {
    left.sha256 == right.sha256 && left.size == right.size
}

async fn append_activation(
    store: &dyn SessionStore,
    event_source: &dyn AgentEventSource,
    session_id: &SessionId,
    profile_name: &str,
    requested: &BlobRef,
    bootstrap: AgentBootstrap,
) -> Result<(), HarnessRuntimeError> {
    let expected_head = bootstrap.head.seq;
    let next_seq = expected_head.checked_next().map_err(|error| {
        HarnessRuntimeError::CompositionInvariant {
            session_id: session_id.clone(),
            message: format!("cannot allocate composition activation sequence: {error}"),
        }
    })?;
    let draft = NewSessionEvent::new(
        event_source.next_event_id(),
        event_source.now(),
        SessionEventPayload::CompositionActivated(CompositionActivated {
            profile: profile_name.to_owned(),
            snapshot: requested.clone(),
        }),
    );
    let expected_event = SessionEvent::committed(session_id.clone(), next_seq, draft.clone())
        .map_err(|error| HarnessRuntimeError::CompositionInvariant {
            session_id: session_id.clone(),
            message: format!("composition activation event is invalid: {error}"),
        })?;

    let mut proposed = bootstrap.events;
    proposed.push(expected_event.clone());
    V1SessionProjector.project(&proposed).map_err(|error| {
        HarnessRuntimeError::CompositionInvariant {
            session_id: session_id.clone(),
            message: format!("composition activation projection failed: {error}"),
        }
    })?;

    let appended = store.append(session_id, expected_head, vec![draft]).await?;
    if appended.new_head != next_seq
        || appended.committed.len() != 1
        || appended.committed[0] != expected_event
    {
        return Err(HarnessRuntimeError::CompositionInvariant {
            session_id: session_id.clone(),
            message: "SessionStore returned a composition activation batch different from the prevalidated batch"
                .to_owned(),
        });
    }
    Ok(())
}
