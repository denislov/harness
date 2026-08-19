//! Local storage backends for the Harness reference implementation.
//!
//! Batch 02 intentionally exposes only [`MemorySessionStore`]. It is a
//! deterministic, process-local reference implementation of the
//! [`harness_session::SessionStore`] contract. It is suitable for unit tests,
//! conformance tests, and early Agent runtime development; it is not durable
//! across process restarts.

mod memory_session;

pub use memory_session::MemorySessionStore;
