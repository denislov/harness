use std::sync::Arc;

use harness_agent::{
    AgentEventSource, AgentExitReason, AgentHandle, AgentTask, spawn_agent_with_capabilities,
};
use harness_session::{CreateSession, SessionCreated, SessionStore};
use harness_storage::BlobStore;
use harness_types::SessionId;
use tokio::sync::{Mutex, RwLock};

use crate::{
    AgentRegistry, HarnessRuntimeBuilder, HarnessRuntimeError, HarnessRuntimeInfo, LlmRegistry,
    ProfileRegistry, ProviderRegistry, RuntimeEventBus, RuntimeEventKind, RuntimeIdSource,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarnessRuntimeState {
    Running,
    ShuttingDown,
    Stopped,
}

/// Process-level composition root for Harness Core.
///
/// `HarnessRuntime` owns provider and Agent lifecycles plus capability binding.
/// It does not own Turn/Step state-machine decisions; those remain exclusively
/// inside `harness-agent` and the durable Session log.
pub struct HarnessRuntime {
    info: HarnessRuntimeInfo,
    state: RwLock<HarnessRuntimeState>,
    shutdown_lock: Mutex<()>,
    providers: ProviderRegistry,
    llms: LlmRegistry,
    profiles: ProfileRegistry,
    agents: AgentRegistry,
    session_store: Arc<dyn SessionStore>,
    blob_store: Arc<dyn BlobStore>,
    event_source: Arc<dyn AgentEventSource>,
    id_source: Arc<dyn RuntimeIdSource>,
    events: RuntimeEventBus,
}

pub(crate) struct HarnessRuntimeParts {
    pub info: HarnessRuntimeInfo,
    pub providers: ProviderRegistry,
    pub llms: LlmRegistry,
    pub profiles: ProfileRegistry,
    pub session_store: Arc<dyn SessionStore>,
    pub blob_store: Arc<dyn BlobStore>,
    pub event_source: Arc<dyn AgentEventSource>,
    pub id_source: Arc<dyn RuntimeIdSource>,
    pub events: RuntimeEventBus,
}

impl HarnessRuntime {
    pub fn builder() -> HarnessRuntimeBuilder {
        HarnessRuntimeBuilder::new()
    }

    pub(crate) fn from_parts(parts: HarnessRuntimeParts) -> Self {
        Self {
            info: parts.info,
            state: RwLock::new(HarnessRuntimeState::Running),
            shutdown_lock: Mutex::new(()),
            providers: parts.providers,
            llms: parts.llms,
            profiles: parts.profiles,
            agents: AgentRegistry::new(),
            session_store: parts.session_store,
            blob_store: parts.blob_store,
            event_source: parts.event_source,
            id_source: parts.id_source,
            events: parts.events,
        }
    }

    pub fn info(&self) -> &HarnessRuntimeInfo {
        &self.info
    }

    pub async fn state(&self) -> HarnessRuntimeState {
        *self.state.read().await
    }

    pub fn providers(&self) -> &ProviderRegistry {
        &self.providers
    }

    pub fn llms(&self) -> &LlmRegistry {
        &self.llms
    }

    pub fn profiles(&self) -> &ProfileRegistry {
        &self.profiles
    }

    pub fn agents(&self) -> &AgentRegistry {
        &self.agents
    }

    pub fn session_store(&self) -> &Arc<dyn SessionStore> {
        &self.session_store
    }

    pub fn blob_store(&self) -> &Arc<dyn BlobStore> {
        &self.blob_store
    }

    pub fn events(&self) -> &RuntimeEventBus {
        &self.events
    }

    pub async fn create_session(&self) -> Result<SessionId, HarnessRuntimeError> {
        self.create_session_with_data(SessionCreated::default())
            .await
    }

    pub async fn create_session_with_data(
        &self,
        data: SessionCreated,
    ) -> Result<SessionId, HarnessRuntimeError> {
        let session_id = self.id_source.next_session_id();
        self.create_session_with_id(session_id.clone(), data)
            .await?;
        Ok(session_id)
    }

    pub async fn create_session_with_id(
        &self,
        session_id: SessionId,
        data: SessionCreated,
    ) -> Result<(), HarnessRuntimeError> {
        let state = self.state.read().await;
        ensure_running(*state)?;
        self.session_store
            .create(CreateSession {
                session_id,
                event_id: self.event_source.next_event_id(),
                timestamp: self.event_source.now(),
                data,
            })
            .await?;
        Ok(())
    }

    pub async fn open_agent(
        &self,
        session_id: SessionId,
        profile_name: &str,
    ) -> Result<AgentHandle, HarnessRuntimeError> {
        // Holding the read guard through spawn means shutdown's write guard cannot
        // pass us and terminate Providers while an Agent is still opening.
        let state = self.state.read().await;
        ensure_running(*state)?;

        let profile = self
            .profiles
            .resolve(profile_name)
            .cloned()
            .ok_or_else(|| HarnessRuntimeError::ProfileNotFound(profile_name.to_owned()))?;
        self.agents.reserve_open(&session_id).await?;

        let instance_id = self.id_source.next_agent_instance_id();
        self.events.publish(RuntimeEventKind::AgentOpening {
            session_id: session_id.clone(),
            profile: profile_name.to_owned(),
            instance_id: instance_id.clone(),
        });
        let spawned = match spawn_agent_with_capabilities(
            instance_id.clone(),
            session_id.clone(),
            self.session_store.clone(),
            self.event_source.clone(),
            profile.llm_runtime,
            profile.tool_runtime,
            profile.actor_config,
        )
        .await
        {
            Ok(spawned) => spawned,
            Err(source) => {
                self.agents.rollback_open(&session_id).await;
                self.events.publish(RuntimeEventKind::AgentOpenFailed {
                    session_id: session_id.clone(),
                    profile: profile_name.to_owned(),
                    instance_id,
                });
                return Err(HarnessRuntimeError::AgentSpawn { session_id, source });
            }
        };

        let handle = self
            .agents
            .commit_open(&session_id, profile_name.to_owned(), spawned)
            .await;
        self.events.publish(RuntimeEventKind::AgentOpened {
            session_id,
            profile: profile_name.to_owned(),
            instance_id,
        });
        Ok(handle)
    }

    pub async fn agent_handle(&self, session_id: &SessionId) -> Option<AgentHandle> {
        self.agents.handle(session_id).await
    }

    pub async fn close_agent(&self, session_id: &SessionId) -> Result<(), HarnessRuntimeError> {
        // As with open_agent, lifecycle shutdown waits for this close to finish.
        let state = self.state.read().await;
        ensure_running(*state)?;

        let (handle, task) = self.agents.take_for_close(session_id).await?;
        self.events.publish(RuntimeEventKind::AgentClosing {
            session_id: session_id.clone(),
        });
        let failures = stop_agent(handle, task).await;
        self.agents.finish_close(session_id).await;
        self.events.publish(RuntimeEventKind::AgentClosed {
            session_id: session_id.clone(),
            failed: !failures.is_empty(),
        });
        if failures.is_empty() {
            Ok(())
        } else {
            Err(HarnessRuntimeError::AgentCloseFailed {
                session_id: session_id.clone(),
                failures,
            })
        }
    }

    /// Stops accepting new lifecycle operations, terminates all Agents, then
    /// shuts Providers down in reverse startup order.
    pub async fn shutdown(&self) -> Result<(), HarnessRuntimeError> {
        let _shutdown = self.shutdown_lock.lock().await;
        {
            let mut state = self.state.write().await;
            if *state == HarnessRuntimeState::Stopped {
                return Ok(());
            }
            *state = HarnessRuntimeState::ShuttingDown;
        }
        self.events.publish(RuntimeEventKind::RuntimeStopping);

        let mut failures = Vec::new();
        for (session_id, handle, task) in self.agents.drain_live().await {
            self.events.publish(RuntimeEventKind::AgentClosing {
                session_id: session_id.clone(),
            });
            let agent_failures = stop_agent(handle, task).await;
            self.events.publish(RuntimeEventKind::AgentClosed {
                session_id: session_id.clone(),
                failed: !agent_failures.is_empty(),
            });
            for failure in agent_failures {
                failures.push(format!("session {session_id}: {failure}"));
            }
        }
        failures.extend(self.providers.shutdown_all().await);

        *self.state.write().await = HarnessRuntimeState::Stopped;
        self.events.publish(RuntimeEventKind::RuntimeStopped {
            failure_count: failures.len(),
        });
        if failures.is_empty() {
            Ok(())
        } else {
            Err(HarnessRuntimeError::ShutdownFailed { failures })
        }
    }
}

fn ensure_running(state: HarnessRuntimeState) -> Result<(), HarnessRuntimeError> {
    if state == HarnessRuntimeState::Running {
        Ok(())
    } else {
        Err(HarnessRuntimeError::NotRunning { actual: state })
    }
}

async fn stop_agent(handle: AgentHandle, task: AgentTask) -> Vec<String> {
    let mut failures = Vec::new();
    if !task.is_finished()
        && let Err(error) = handle.shutdown().await
    {
        failures.push(format!("shutdown command failed: {error}"));
    }

    match task.join().await {
        Ok(exit) => {
            if let AgentExitReason::Fatal(error) = exit.reason {
                failures.push(format!("Agent exited fatally: {error}"));
            }
        }
        Err(error) => failures.push(format!("Agent task join failed: {error}")),
    }
    failures
}
