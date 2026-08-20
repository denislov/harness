use harness_agent::AgentEventSource;
use harness_runtime::RuntimeIdSource;
use harness_types::{AgentInstanceId, EventId, MessageId, SessionId, Timestamp};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default)]
pub struct UuidIdentitySource;

impl UuidIdentitySource {
    pub fn next_message_id(&self) -> MessageId {
        MessageId::new(prefixed("msg")).expect("UUID-backed MessageId is non-empty")
    }
}

impl AgentEventSource for UuidIdentitySource {
    fn next_event_id(&self) -> EventId {
        EventId::new(prefixed("evt")).expect("UUID-backed EventId is non-empty")
    }

    fn now(&self) -> Timestamp {
        Timestamp::now_utc()
    }
}

impl RuntimeIdSource for UuidIdentitySource {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(prefixed("ses")).expect("UUID-backed SessionId is non-empty")
    }

    fn next_agent_instance_id(&self) -> AgentInstanceId {
        AgentInstanceId::new(prefixed("agt")).expect("UUID-backed AgentInstanceId is non-empty")
    }
}

fn prefixed(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}
