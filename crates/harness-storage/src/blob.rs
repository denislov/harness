use async_trait::async_trait;
use harness_types::{BlobId, BlobRef};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum BlobStoreError {
    #[error("blob {blob_id} was not found")]
    NotFound { blob_id: BlobId },

    #[error("blob integrity verification failed for {blob_id}: {message}")]
    Integrity { blob_id: BlobId, message: String },

    #[error("blob storage backend failure: {message}")]
    Backend { message: String },
}

/// Immutable byte storage addressed through [`BlobRef`].
///
/// A BlobRef that has been committed into Session state must continue to refer
/// to the same bytes. Implementations may deduplicate equal content and may use
/// content-addressed identifiers, but neither behavior is required by the trait.
#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(
        &self,
        bytes: Vec<u8>,
        media_type: Option<String>,
    ) -> Result<BlobRef, BlobStoreError>;

    async fn get(&self, blob_id: &BlobId) -> Result<Vec<u8>, BlobStoreError>;

    async fn verify(&self, blob: &BlobRef) -> Result<(), BlobStoreError>;
}
