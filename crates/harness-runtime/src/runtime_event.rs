use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use harness_types::{AgentInstanceId, MAX_JS_SAFE_INTEGER, ProviderId, SessionId, Timestamp};
use serde::{Serialize, Serializer};
use thiserror::Error;
use tokio::sync::broadcast;

pub const RUNTIME_EVENT_SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_RUNTIME_EVENT_CAPACITY: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeEvent {
    pub schema_version: u16,
    pub seq: u64,
    pub timestamp: Timestamp,
    pub kind: RuntimeEventKind,
}

impl Serialize for RuntimeEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let (event_type, data) = self.kind.wire_parts();
        RuntimeEventWire {
            schema_version: self.schema_version,
            seq: self.seq,
            timestamp: self.timestamp,
            event_type,
            data,
        }
        .serialize(serializer)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RuntimeBuildStage {
    Preflight,
    Provider,
    Llm,
    Profile,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeEventKind {
    RuntimeBuildStarted {
        name: String,
        version: String,
    },
    RuntimeBuildFailed {
        stage: RuntimeBuildStage,
    },
    RuntimeStarted {
        name: String,
        version: String,
    },
    RuntimeStopping,
    RuntimeStopped {
        failure_count: usize,
    },
    ProviderStarting {
        provider: ProviderId,
    },
    ProviderReady {
        provider: ProviderId,
        provider_version: String,
    },
    ProviderStartFailed {
        provider: ProviderId,
    },
    ProviderStopping {
        provider: ProviderId,
    },
    ProviderStopped {
        provider: ProviderId,
        failed: bool,
    },
    CredentialResolutionFailed {
        provider: ProviderId,
        environment: String,
        credential: String,
    },
    AgentOpening {
        session_id: SessionId,
        profile: String,
        instance_id: AgentInstanceId,
    },
    AgentOpened {
        session_id: SessionId,
        profile: String,
        instance_id: AgentInstanceId,
    },
    AgentOpenFailed {
        session_id: SessionId,
        profile: String,
        instance_id: AgentInstanceId,
    },
    AgentClosing {
        session_id: SessionId,
    },
    AgentClosed {
        session_id: SessionId,
        failed: bool,
    },
}

impl RuntimeEventKind {
    fn wire_parts(&self) -> (&'static str, Option<RuntimeEventDataRef<'_>>) {
        match self {
            Self::RuntimeBuildStarted { name, version } => (
                "runtime/build-started",
                Some(RuntimeEventDataRef::RuntimeIdentity { name, version }),
            ),
            Self::RuntimeBuildFailed { stage } => (
                "runtime/build-failed",
                Some(RuntimeEventDataRef::BuildFailed { stage: *stage }),
            ),
            Self::RuntimeStarted { name, version } => (
                "runtime/started",
                Some(RuntimeEventDataRef::RuntimeIdentity { name, version }),
            ),
            Self::RuntimeStopping => ("runtime/stopping", None),
            Self::RuntimeStopped { failure_count } => (
                "runtime/stopped",
                Some(RuntimeEventDataRef::RuntimeStopped {
                    failure_count: *failure_count,
                }),
            ),
            Self::ProviderStarting { provider } => (
                "provider/starting",
                Some(RuntimeEventDataRef::Provider { provider }),
            ),
            Self::ProviderReady {
                provider,
                provider_version,
            } => (
                "provider/ready",
                Some(RuntimeEventDataRef::ProviderReady {
                    provider,
                    provider_version,
                }),
            ),
            Self::ProviderStartFailed { provider } => (
                "provider/start-failed",
                Some(RuntimeEventDataRef::Provider { provider }),
            ),
            Self::ProviderStopping { provider } => (
                "provider/stopping",
                Some(RuntimeEventDataRef::Provider { provider }),
            ),
            Self::ProviderStopped { provider, failed } => (
                "provider/stopped",
                Some(RuntimeEventDataRef::ProviderStopped {
                    provider,
                    failed: *failed,
                }),
            ),
            Self::CredentialResolutionFailed {
                provider,
                environment,
                credential,
            } => (
                "credential/resolution-failed",
                Some(RuntimeEventDataRef::CredentialResolutionFailed {
                    provider,
                    environment,
                    credential,
                }),
            ),
            Self::AgentOpening {
                session_id,
                profile,
                instance_id,
            } => (
                "agent/opening",
                Some(RuntimeEventDataRef::AgentIdentity {
                    session_id,
                    profile,
                    instance_id,
                }),
            ),
            Self::AgentOpened {
                session_id,
                profile,
                instance_id,
            } => (
                "agent/opened",
                Some(RuntimeEventDataRef::AgentIdentity {
                    session_id,
                    profile,
                    instance_id,
                }),
            ),
            Self::AgentOpenFailed {
                session_id,
                profile,
                instance_id,
            } => (
                "agent/open-failed",
                Some(RuntimeEventDataRef::AgentIdentity {
                    session_id,
                    profile,
                    instance_id,
                }),
            ),
            Self::AgentClosing { session_id } => (
                "agent/closing",
                Some(RuntimeEventDataRef::AgentSession { session_id }),
            ),
            Self::AgentClosed { session_id, failed } => (
                "agent/closed",
                Some(RuntimeEventDataRef::AgentClosed {
                    session_id,
                    failed: *failed,
                }),
            ),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeEventWire<'a> {
    schema_version: u16,
    seq: u64,
    #[serde(rename = "time")]
    timestamp: Timestamp,
    #[serde(rename = "type")]
    event_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<RuntimeEventDataRef<'a>>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum RuntimeEventDataRef<'a> {
    RuntimeIdentity {
        name: &'a str,
        version: &'a str,
    },
    BuildFailed {
        stage: RuntimeBuildStage,
    },
    RuntimeStopped {
        #[serde(rename = "failureCount")]
        failure_count: usize,
    },
    Provider {
        provider: &'a ProviderId,
    },
    ProviderReady {
        provider: &'a ProviderId,
        #[serde(rename = "providerVersion")]
        provider_version: &'a str,
    },
    ProviderStopped {
        provider: &'a ProviderId,
        failed: bool,
    },
    CredentialResolutionFailed {
        provider: &'a ProviderId,
        environment: &'a str,
        credential: &'a str,
    },
    AgentIdentity {
        #[serde(rename = "sessionId")]
        session_id: &'a SessionId,
        profile: &'a str,
        #[serde(rename = "instanceId")]
        instance_id: &'a AgentInstanceId,
    },
    AgentSession {
        #[serde(rename = "sessionId")]
        session_id: &'a SessionId,
    },
    AgentClosed {
        #[serde(rename = "sessionId")]
        session_id: &'a SessionId,
        failed: bool,
    },
}

#[derive(Clone)]
pub struct RuntimeEventBus {
    inner: Arc<RuntimeEventBusInner>,
}

struct RuntimeEventBusInner {
    last_seq: AtomicU64,
    sender: broadcast::Sender<RuntimeEvent>,
}

impl Default for RuntimeEventBus {
    fn default() -> Self {
        Self::new(DEFAULT_RUNTIME_EVENT_CAPACITY)
            .expect("default runtime event capacity is greater than zero")
    }
}

impl RuntimeEventBus {
    pub fn new(capacity: usize) -> Result<Self, RuntimeEventBusError> {
        if capacity == 0 {
            return Err(RuntimeEventBusError::ZeroCapacity);
        }
        let (sender, _) = broadcast::channel(capacity);
        Ok(Self {
            inner: Arc::new(RuntimeEventBusInner {
                last_seq: AtomicU64::new(0),
                sender,
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.inner.sender.subscribe()
    }

    pub fn publish(&self, kind: RuntimeEventKind) -> RuntimeEvent {
        let previous = self
            .inner
            .last_seq
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                (current < MAX_JS_SAFE_INTEGER).then_some(current + 1)
            })
            .expect("RuntimeEvent sequence exhausted");
        let seq = previous + 1;
        let event = RuntimeEvent {
            schema_version: RUNTIME_EVENT_SCHEMA_VERSION,
            seq,
            timestamp: Timestamp::now_utc(),
            kind,
        };
        let _ = self.inner.sender.send(event.clone());
        event
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RuntimeEventBusError {
    #[error("RuntimeEventBus capacity must be greater than zero")]
    ZeroCapacity,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_assigns_process_local_sequence_and_serializable_shape() {
        let bus = RuntimeEventBus::new(8).unwrap();
        let mut receiver = bus.subscribe();
        bus.publish(RuntimeEventKind::RuntimeStopping);
        bus.publish(RuntimeEventKind::RuntimeStopped { failure_count: 0 });

        let first = receiver.recv().await.unwrap();
        let second = receiver.recv().await.unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);
        assert_eq!(first.schema_version, RUNTIME_EVENT_SCHEMA_VERSION);
        let json = serde_json::to_value(second).unwrap();
        assert_eq!(json["type"], "runtime/stopped");
        assert_eq!(json["data"]["failureCount"], 0);
    }
}
