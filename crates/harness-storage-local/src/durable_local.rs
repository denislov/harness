use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use harness_session::SessionStoreError;
use harness_storage::BlobStoreError;
use thiserror::Error;

use crate::{FilesystemBlobStore, SqliteSessionStore};

/// Conventional durable local storage layout owned by one Harness deployment.
///
/// ```text
/// <root>/
/// ├── sessions.sqlite3
/// └── blobs/
///     └── sha256/...
/// ```
#[derive(Clone, Debug)]
pub struct DurableLocalStorage {
    root: PathBuf,
    session_store: Arc<SqliteSessionStore>,
    blob_store: Arc<FilesystemBlobStore>,
}

impl DurableLocalStorage {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, DurableLocalStorageError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|source| DurableLocalStorageError::CreateRoot {
            root: root.clone(),
            source,
        })?;
        let session_store = Arc::new(
            SqliteSessionStore::open(root.join("sessions.sqlite3"))
                .map_err(DurableLocalStorageError::SessionStore)?,
        );
        let blob_store = Arc::new(
            FilesystemBlobStore::open(root.join("blobs"))
                .map_err(DurableLocalStorageError::BlobStore)?,
        );
        Ok(Self {
            root,
            session_store,
            blob_store,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn session_store(&self) -> Arc<SqliteSessionStore> {
        self.session_store.clone()
    }

    pub fn blob_store(&self) -> Arc<FilesystemBlobStore> {
        self.blob_store.clone()
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DurableLocalStorageError {
    #[error("cannot create durable local storage root {root}: {source}")]
    CreateRoot {
        root: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot open durable SessionStore: {0}")]
    SessionStore(#[source] SessionStoreError),

    #[error("cannot open durable BlobStore: {0}")]
    BlobStore(#[source] BlobStoreError),
}
