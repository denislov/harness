use harness_types::{EventId, Timestamp};

/// Source of durable event identity and time for Agent-owned writes.
///
/// The trait is intentionally infallible. A production composition MUST provide
/// an implementation that generates globally collision-resistant EventIds across
/// process restarts and UTC timestamps. `harness-agent` does not choose a UUID/
/// ULID dependency; that policy belongs in `harness-runtime`.
pub trait AgentEventSource: Send + Sync {
    fn next_event_id(&self) -> EventId;
    fn now(&self) -> Timestamp;
}
