use harness_types::AgentInstanceId;

use crate::{AgentBootstrap, AgentState};

/// Process-local owner of one Session's mutable Agent state.
///
/// Batch 04 intentionally does not introduce a channel/executor dependency. The
/// next batch will wrap this owner in the concrete async actor task and will make
/// `AgentCommand` the only state-changing ingress path.
#[derive(Clone, Debug)]
pub struct AgentActor {
    state: AgentState,
}

impl AgentActor {
    pub fn from_bootstrap(instance_id: AgentInstanceId, bootstrap: AgentBootstrap) -> Self {
        Self {
            state: AgentState::from_bootstrap(instance_id, bootstrap),
        }
    }

    pub const fn state(&self) -> &AgentState {
        &self.state
    }

    pub fn into_state(self) -> AgentState {
        self.state
    }
}
