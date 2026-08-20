//! Local storage backends for the Harness reference implementation.
//!
//! The in-memory backends remain deterministic process-local references. Batch
//! 15 adds durable local implementations: a transactional SQLite SessionStore
//! and a content-addressed filesystem BlobStore.

mod durable_local;
mod filesystem_blob;
mod memory_blob;
mod memory_session;
mod sqlite_session;

pub use durable_local::{DurableLocalStorage, DurableLocalStorageError};
pub use filesystem_blob::FilesystemBlobStore;
pub use memory_blob::MemoryBlobStore;
pub use memory_session::MemorySessionStore;
pub use sqlite_session::SqliteSessionStore;
