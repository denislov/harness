use harness_types::{AgentInstanceId, SessionId};

/// Generates process-level identities that are not owned by the Agent state
/// machine.
///
/// Production composition MUST provide collision-resistant identifiers across
/// process restarts. Batch 14 deliberately keeps the choice of UUID/ULID/etc.
/// outside the Core crates and does not introduce a new randomness dependency.
pub trait RuntimeIdSource: Send + Sync {
    fn next_session_id(&self) -> SessionId;
    fn next_agent_instance_id(&self) -> AgentInstanceId;
}
