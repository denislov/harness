//! Storage abstractions shared by Harness Core subsystems.
//!
//! `SessionStore` remains owned by `harness-session` in v0.1 because it is part
//! of the Session event-log contract. This crate starts with the independent,
//! content-oriented `BlobStore` seam used by immutable request snapshots and
//! future large/binary payloads.

mod blob;

pub use blob::{BlobStore, BlobStoreError};
