use std::collections::BTreeMap;

use harness_agent::{AgentHandle, AgentTask, SpawnedAgent};
use harness_types::SessionId;
use tokio::sync::Mutex;

use crate::HarnessRuntimeError;

struct LiveAgent {
    handle: AgentHandle,
    task: AgentTask,
    profile_name: String,
}

enum AgentSlot {
    Opening,
    Live(LiveAgent),
    Closing,
}

/// Process-local ownership registry enforcing at most one live Agent driver for
/// each Session inside a HarnessRuntime.
pub struct AgentRegistry {
    slots: Mutex<BTreeMap<SessionId, AgentSlot>>,
}

impl AgentRegistry {
    pub(crate) fn new() -> Self {
        Self {
            slots: Mutex::new(BTreeMap::new()),
        }
    }

    pub async fn len(&self) -> usize {
        self.slots.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.slots.lock().await.is_empty()
    }

    pub async fn contains(&self, session_id: &SessionId) -> bool {
        self.slots.lock().await.contains_key(session_id)
    }

    pub async fn handle(&self, session_id: &SessionId) -> Option<AgentHandle> {
        match self.slots.lock().await.get(session_id) {
            Some(AgentSlot::Live(agent)) => Some(agent.handle.clone()),
            _ => None,
        }
    }

    pub async fn profile_name(&self, session_id: &SessionId) -> Option<String> {
        match self.slots.lock().await.get(session_id) {
            Some(AgentSlot::Live(agent)) => Some(agent.profile_name.clone()),
            _ => None,
        }
    }

    pub(crate) async fn reserve_open(
        &self,
        session_id: &SessionId,
    ) -> Result<(), HarnessRuntimeError> {
        let mut slots = self.slots.lock().await;
        if slots.contains_key(session_id) {
            return Err(HarnessRuntimeError::AgentAlreadyActive(session_id.clone()));
        }
        let _ = slots.insert(session_id.clone(), AgentSlot::Opening);
        Ok(())
    }

    pub(crate) async fn rollback_open(&self, session_id: &SessionId) {
        let mut slots = self.slots.lock().await;
        if matches!(slots.get(session_id), Some(AgentSlot::Opening)) {
            let _ = slots.remove(session_id);
        }
    }

    pub(crate) async fn commit_open(
        &self,
        session_id: &SessionId,
        profile_name: String,
        spawned: SpawnedAgent,
    ) -> AgentHandle {
        let mut slots = self.slots.lock().await;
        debug_assert!(
            matches!(slots.get(session_id), Some(AgentSlot::Opening)),
            "Agent opening reservation must survive until commit"
        );
        let handle = spawned.handle.clone();
        let _ = slots.insert(
            session_id.clone(),
            AgentSlot::Live(LiveAgent {
                handle: spawned.handle,
                task: spawned.task,
                profile_name,
            }),
        );
        handle
    }

    pub(crate) async fn take_for_close(
        &self,
        session_id: &SessionId,
    ) -> Result<(AgentHandle, AgentTask), HarnessRuntimeError> {
        let mut slots = self.slots.lock().await;
        let Some(slot) = slots.remove(session_id) else {
            return Err(HarnessRuntimeError::AgentNotActive(session_id.clone()));
        };
        match slot {
            AgentSlot::Live(agent) => {
                let _ = slots.insert(session_id.clone(), AgentSlot::Closing);
                Ok((agent.handle, agent.task))
            }
            AgentSlot::Opening => {
                let _ = slots.insert(session_id.clone(), AgentSlot::Opening);
                Err(HarnessRuntimeError::AgentTransitioning(session_id.clone()))
            }
            AgentSlot::Closing => {
                let _ = slots.insert(session_id.clone(), AgentSlot::Closing);
                Err(HarnessRuntimeError::AgentTransitioning(session_id.clone()))
            }
        }
    }

    pub(crate) async fn finish_close(&self, session_id: &SessionId) {
        let mut slots = self.slots.lock().await;
        if matches!(slots.get(session_id), Some(AgentSlot::Closing)) {
            let _ = slots.remove(session_id);
        }
    }

    pub(crate) async fn drain_live(&self) -> Vec<(SessionId, AgentHandle, AgentTask)> {
        let mut slots = self.slots.lock().await;
        let taken = std::mem::take(&mut *slots);
        taken
            .into_iter()
            .filter_map(|(session_id, slot)| match slot {
                AgentSlot::Live(agent) => Some((session_id, agent.handle, agent.task)),
                AgentSlot::Opening | AgentSlot::Closing => None,
            })
            .collect()
    }
}
