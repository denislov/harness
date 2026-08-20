use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use harness_storage::{BlobStore, BlobStoreError};
use harness_types::{BlobId, BlobRef, Sha256Digest};
use sha2::{Digest, Sha256};

const BLOB_ID_PREFIX: &str = "blob_sha256_";
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// Durable content-addressed BlobStore rooted at one local directory.
///
/// Bytes are stored by SHA-256 under `sha256/<first-two-hex>/<digest>`. Writes
/// use a temporary file in the destination directory followed by an atomic
/// hard-link publication, so a process crash cannot expose a partially-written committed blob.
#[derive(Clone, Debug)]
pub struct FilesystemBlobStore {
    root: Arc<PathBuf>,
}

impl FilesystemBlobStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, BlobStoreError> {
        let root = root.into();
        fs::create_dir_all(root.join("sha256"))
            .map_err(|error| backend_error("create root", error))?;
        Ok(Self {
            root: Arc::new(root),
        })
    }

    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    fn path_for_blob_id(&self, blob_id: &BlobId) -> Result<PathBuf, BlobStoreError> {
        let digest = blob_id
            .as_str()
            .strip_prefix(BLOB_ID_PREFIX)
            .filter(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or_else(|| BlobStoreError::NotFound {
                blob_id: blob_id.clone(),
            })?;
        Ok(self.root.join("sha256").join(&digest[..2]).join(digest))
    }
}

#[async_trait]
impl BlobStore for FilesystemBlobStore {
    async fn put(
        &self,
        bytes: Vec<u8>,
        media_type: Option<String>,
    ) -> Result<BlobRef, BlobStoreError> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || put_sync(root.as_path(), bytes, media_type))
            .await
            .map_err(join_error)?
    }

    async fn get(&self, blob_id: &BlobId) -> Result<Vec<u8>, BlobStoreError> {
        let path = self.path_for_blob_id(blob_id)?;
        let blob_id = blob_id.clone();
        tokio::task::spawn_blocking(move || {
            fs::read(&path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    BlobStoreError::NotFound { blob_id }
                } else {
                    backend_error("read blob", error)
                }
            })
        })
        .await
        .map_err(join_error)?
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

fn put_sync(
    root: &Path,
    bytes: Vec<u8>,
    media_type: Option<String>,
) -> Result<BlobRef, BlobStoreError> {
    let digest = digest(&bytes);
    let blob_id = BlobId::new(format!("{BLOB_ID_PREFIX}{digest}"))
        .expect("content-addressed BlobId is non-empty");
    let size = u64::try_from(bytes.len()).map_err(|_| BlobStoreError::Backend {
        message: "blob length does not fit in u64".to_owned(),
    })?;
    let digest_text = digest.as_str();
    let directory = root.join("sha256").join(&digest_text[..2]);
    fs::create_dir_all(&directory).map_err(|error| backend_error("create blob shard", error))?;
    let destination = directory.join(digest_text);

    if destination.exists() {
        verify_existing(&destination, &blob_id, &bytes)?;
    } else {
        let (temp, mut file) = create_temporary_blob(&directory, digest_text)?;
        let write_result = (|| -> Result<(), BlobStoreError> {
            file.write_all(&bytes)
                .map_err(|error| backend_error("write temporary blob", error))?;
            file.sync_all()
                .map_err(|error| backend_error("sync temporary blob", error))?;
            drop(file);
            // Publish the fully-written temporary inode without replacing an
            // already-committed blob. A hard link is atomic and returns
            // AlreadyExists if another writer won the same digest race.
            match fs::hard_link(&temp, &destination) {
                Ok(()) => {
                    fs::remove_file(&temp)
                        .map_err(|error| backend_error("remove committed temporary blob", error))?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    verify_existing(&destination, &blob_id, &bytes)?;
                    fs::remove_file(&temp).map_err(|remove_error| {
                        backend_error("remove raced temporary blob", remove_error)
                    })?;
                }
                Err(error) => return Err(backend_error("publish blob hard link", error)),
            }
            sync_directory(&directory)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        write_result?;
    }

    Ok(BlobRef {
        id: blob_id,
        sha256: digest,
        size,
        media_type,
    })
}

fn create_temporary_blob(
    directory: &Path,
    digest_text: &str,
) -> Result<(PathBuf, File), BlobStoreError> {
    loop {
        let temp_id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let temp = directory.join(format!(
            ".{digest_text}.tmp-{}-{temp_id}",
            std::process::id()
        ));
        match OpenOptions::new().create_new(true).write(true).open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(backend_error("create temporary blob", error)),
        }
    }
}

fn verify_existing(path: &Path, blob_id: &BlobId, expected: &[u8]) -> Result<(), BlobStoreError> {
    let mut file = File::open(path).map_err(|error| backend_error("open existing blob", error))?;
    let mut actual = Vec::new();
    file.read_to_end(&mut actual)
        .map_err(|error| backend_error("read existing blob", error))?;
    if actual != expected {
        return Err(BlobStoreError::Integrity {
            blob_id: blob_id.clone(),
            message: "content-addressed path contains different bytes".to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), BlobStoreError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| backend_error("sync blob directory", error))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), BlobStoreError> {
    Ok(())
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

fn backend_error(context: &str, error: std::io::Error) -> BlobStoreError {
    BlobStoreError::Backend {
        message: format!("FilesystemBlobStore {context}: {error}"),
    }
}

fn join_error(error: tokio::task::JoinError) -> BlobStoreError {
    BlobStoreError::Backend {
        message: format!("FilesystemBlobStore blocking task failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use harness_storage::BlobStore;

    use super::*;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    fn test_dir(label: &str) -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "harness-fs-blob-{label}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[tokio::test]
    async fn blob_survives_store_reopen() {
        let root = test_dir("reopen");
        let first = FilesystemBlobStore::open(&root).unwrap();
        let blob = first
            .put(b"durable hello".to_vec(), Some("text/plain".to_owned()))
            .await
            .unwrap();
        drop(first);

        let reopened = FilesystemBlobStore::open(&root).unwrap();
        assert_eq!(
            reopened.get(&blob.id).await.unwrap(),
            b"durable hello".to_vec()
        );
        reopened.verify(&blob).await.unwrap();

        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn put_refuses_to_overwrite_corrupt_committed_path() {
        let root = test_dir("corrupt-existing");
        let store = FilesystemBlobStore::open(&root).unwrap();
        let blob = store.put(b"expected".to_vec(), None).await.unwrap();
        let path = store.path_for_blob_id(&blob.id).unwrap();
        fs::write(&path, b"corrupt").unwrap();

        assert!(matches!(
            store.put(b"expected".to_vec(), None).await,
            Err(BlobStoreError::Integrity { .. })
        ));

        fs::remove_dir_all(root).unwrap();
    }
}
