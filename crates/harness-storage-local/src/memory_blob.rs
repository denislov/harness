use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use harness_storage::{BlobStore, BlobStoreError};
use harness_types::{BlobId, BlobRef, Sha256Digest};
use sha2::{Digest, Sha256};

#[derive(Default)]
pub struct MemoryBlobStore {
    blobs: RwLock<HashMap<BlobId, Arc<[u8]>>>,
}

impl MemoryBlobStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> Result<usize, BlobStoreError> {
        Ok(self.blobs.read().map_err(lock_error)?.len())
    }

    pub fn is_empty(&self) -> Result<bool, BlobStoreError> {
        Ok(self.len()? == 0)
    }
}

#[async_trait]
impl BlobStore for MemoryBlobStore {
    async fn put(
        &self,
        bytes: Vec<u8>,
        media_type: Option<String>,
    ) -> Result<BlobRef, BlobStoreError> {
        let digest = digest(&bytes);
        let blob_id = BlobId::new(format!("blob_sha256_{digest}"))
            .expect("content-addressed BlobId is non-empty");
        let size = u64::try_from(bytes.len()).map_err(|_| BlobStoreError::Backend {
            message: "blob length does not fit in u64".to_owned(),
        })?;

        let mut blobs = self.blobs.write().map_err(lock_error)?;
        if let Some(existing) = blobs.get(&blob_id) {
            if existing.as_ref() != bytes.as_slice() {
                return Err(BlobStoreError::Integrity {
                    blob_id,
                    message: "content-addressed identifier collision".to_owned(),
                });
            }
        } else {
            blobs.insert(blob_id.clone(), Arc::<[u8]>::from(bytes));
        }

        Ok(BlobRef {
            id: blob_id,
            sha256: digest,
            size,
            media_type,
        })
    }

    async fn get(&self, blob_id: &BlobId) -> Result<Vec<u8>, BlobStoreError> {
        self.blobs
            .read()
            .map_err(lock_error)?
            .get(blob_id)
            .map(|bytes| bytes.as_ref().to_vec())
            .ok_or_else(|| BlobStoreError::NotFound {
                blob_id: blob_id.clone(),
            })
    }

    async fn verify(&self, blob: &BlobRef) -> Result<(), BlobStoreError> {
        let bytes = self.get(&blob.id).await?;
        let actual_digest = digest(&bytes);
        let actual_size = u64::try_from(bytes.len()).map_err(|_| BlobStoreError::Backend {
            message: "blob length does not fit in u64".to_owned(),
        })?;
        if actual_digest != blob.sha256 {
            return Err(BlobStoreError::Integrity {
                blob_id: blob.id.clone(),
                message: format!(
                    "SHA-256 mismatch: expected {}, got {}",
                    blob.sha256, actual_digest
                ),
            });
        }
        if actual_size != blob.size {
            return Err(BlobStoreError::Integrity {
                blob_id: blob.id.clone(),
                message: format!("size mismatch: expected {}, got {actual_size}", blob.size),
            });
        }
        Ok(())
    }
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    Sha256Digest::new(hex).expect("SHA-256 formatter emits 64 lowercase hex characters")
}

fn lock_error<T>(error: std::sync::PoisonError<T>) -> BlobStoreError {
    BlobStoreError::Backend {
        message: format!("MemoryBlobStore lock poisoned: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use harness_storage::BlobStore;
    use harness_types::Sha256Digest;

    use super::MemoryBlobStore;

    #[tokio::test]
    async fn put_is_content_addressed_and_deduplicated() {
        let store = MemoryBlobStore::new();
        let first = store
            .put(b"hello".to_vec(), Some("text/plain".to_owned()))
            .await
            .unwrap();
        let second = store
            .put(
                b"hello".to_vec(),
                Some("application/octet-stream".to_owned()),
            )
            .await
            .unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(store.len().unwrap(), 1);
        assert_eq!(store.get(&first.id).await.unwrap(), b"hello".to_vec());
        store.verify(&first).await.unwrap();
    }

    #[tokio::test]
    async fn verify_rejects_digest_mismatch() {
        let store = MemoryBlobStore::new();
        let mut blob = store.put(b"hello".to_vec(), None).await.unwrap();
        blob.sha256 = Sha256Digest::new("0".repeat(64)).unwrap();

        assert!(store.verify(&blob).await.is_err());
    }
}
