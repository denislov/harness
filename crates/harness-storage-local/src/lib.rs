//! Local storage backends for the Harness reference implementation.
//!
//! The in-memory backends are deterministic process-local reference
//! implementations suitable for unit/conformance tests and early runtime
//! development. They are not durable across process restarts.

mod memory_blob;
mod memory_session;

pub use memory_blob::MemoryBlobStore;
pub use memory_session::MemorySessionStore;
