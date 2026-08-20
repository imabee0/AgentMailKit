//! Content-addressed blob storage: raw MIME and attachment bodies.
//!
//! `docs/PLAN.md`:47-48 specifies "content-addressed filesystem blobs behind a trait", and
//! `reference/fixtures/06-download-url-expiry.txt` covers how they are handed out. This is the
//! storage half; `amk-core::download` is the handing-out half.
//!
//! # Why content-addressed
//!
//! The id is the SHA-256 of the bytes, which buys three things that matter here and are awkward to
//! retrofit:
//!
//! 1. **Writes are idempotent.** Re-ingesting the same message -- a duplicate delivery, a retried
//!    job, a re-run migration -- writes the same path with the same content. No dedup table, no
//!    "does this already exist" round trip.
//! 2. **Deduplication is free.** One attachment sent to forty inboxes is one object. A mail server
//!    is close to the best case for this.
//! 3. **Backups are cheap and incremental.** Objects are immutable, so `rsync` never re-sends one
//!    it has -- which is exactly what `docs/PLAN.md`:200's blob-tree snapshot depends on.
//!
//! The cost, stated: deletion needs refcounting or a mark-and-sweep, because a blob may be
//! referenced by any number of rows. Nothing here deletes; `docs/PLAN.md`'s retention story is P6
//! and a sweep belongs with it.
//!
//! # Why a trait
//!
//! So the tests do not touch a filesystem and so an object-store backend can arrive later without
//! reaching into call sites. The trait is deliberately tiny -- put, get, len -- because every
//! method added is one an alternative backend must implement.

use std::future::Future;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

/// A blob's identity: lowercase hex SHA-256 of its contents.
///
/// A newtype rather than `String` so a path, a message id and a blob id cannot be swapped at a
/// call site -- `MessageId` and `InboxId` earn their newtypes for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobId(String);

impl BlobId {
    /// Compute the id of some bytes. The only way to make one from content.
    pub fn of(bytes: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(bytes);
        Self(hex(&h.finalize()))
    }

    /// Parse an id received from outside -- a database column, a URL path segment.
    ///
    /// Strict: exactly 64 lowercase hex characters. This is the ONLY validation between a
    /// caller-supplied string and a filesystem path, so it is what stops `../../etc/passwd` and
    /// every other traversal. `relative_path` cannot be reached with anything else.
    pub fn parse(s: &str) -> Option<Self> {
        if s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Some(Self(s.to_owned()))
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `ab/cd/abcd...` -- two levels of fan-out.
    ///
    /// One flat directory with millions of entries is slow to list and, on some filesystems, slow
    /// to open. Two levels of 256 gives 65,536 buckets, which keeps directories small well past
    /// any volume this deployment will see.
    fn relative_path(&self) -> PathBuf {
        PathBuf::from(&self.0[0..2])
            .join(&self.0[2..4])
            .join(&self.0)
    }
}

impl std::fmt::Display for BlobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("blob io: {0}")]
    Io(String),
    #[error("blob not found")]
    NotFound,
    /// Stored bytes do not hash to the id they are filed under.
    #[error("blob {0} is corrupt: content does not match its id")]
    Corrupt(String),
}

/// Storage for immutable content-addressed objects.
pub trait BlobStore: Send + Sync {
    /// Store `bytes`, returning their id. Idempotent by construction.
    fn put(&self, bytes: &[u8]) -> impl Future<Output = Result<BlobId, BlobError>> + Send;
    /// Fetch by id.
    fn get(&self, id: &BlobId) -> impl Future<Output = Result<Vec<u8>, BlobError>> + Send;
    /// Size in bytes, without reading the object.
    fn len(&self, id: &BlobId) -> impl Future<Output = Result<u64, BlobError>> + Send;
}

/// A blob tree on local disk.
#[derive(Debug, Clone)]
pub struct FsBlobStore {
    root: PathBuf,
    /// Re-hash on read. Off by default: it doubles read cost, and the filesystem plus the backup
    /// story are the primary integrity mechanisms. On, it turns silent corruption into a loud
    /// error -- which is what `docs/PLAN.md`'s restore drill wants to be able to assert.
    verify_on_read: bool,
}

impl FsBlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), verify_on_read: false }
    }

    pub fn verifying(mut self) -> Self {
        self.verify_on_read = true;
        self
    }

    fn path(&self, id: &BlobId) -> PathBuf {
        self.root.join(id.relative_path())
    }
}

impl BlobStore for FsBlobStore {
    async fn put(&self, bytes: &[u8]) -> Result<BlobId, BlobError> {
        let id = BlobId::of(bytes);
        let path = self.path(&id);

        // Already present: identical content by definition, so nothing to do. This is what makes a
        // re-delivery or a replayed job free rather than a rewrite.
        if tokio::fs::metadata(&path).await.is_ok() {
            return Ok(id);
        }

        let dir = path.parent().expect("blob paths always have a parent");
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| BlobError::Io(format!("create {}: {e}", dir.display())))?;

        // Write to a unique temporary name in the SAME directory, then rename. A rename within one
        // filesystem is atomic, so a reader never observes a partial object and a crash mid-write
        // leaves a stray temp file rather than a truncated blob that hashes to nothing.
        let tmp = dir.join(format!(".tmp-{}-{}", std::process::id(), id.as_str()));
        tokio::fs::write(&tmp, bytes)
            .await
            .map_err(|e| BlobError::Io(format!("write {}: {e}", tmp.display())))?;
        match tokio::fs::rename(&tmp, &path).await {
            Ok(()) => Ok(id),
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                // A concurrent writer winning the race is success: the content is identical.
                if tokio::fs::metadata(&path).await.is_ok() {
                    Ok(id)
                } else {
                    Err(BlobError::Io(format!("rename into {}: {e}", path.display())))
                }
            }
        }
    }

    async fn get(&self, id: &BlobId) -> Result<Vec<u8>, BlobError> {
        let path = self.path(id);
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(BlobError::NotFound),
            Err(e) => return Err(BlobError::Io(format!("read {}: {e}", path.display()))),
        };
        if self.verify_on_read && BlobId::of(&bytes) != *id {
            return Err(BlobError::Corrupt(id.to_string()));
        }
        Ok(bytes)
    }

    async fn len(&self, id: &BlobId) -> Result<u64, BlobError> {
        match tokio::fs::metadata(self.path(id)).await {
            Ok(m) => Ok(m.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(BlobError::NotFound),
            Err(e) => Err(BlobError::Io(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn store() -> (FsBlobStore, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "amk-blobs-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        (FsBlobStore::new(&root), root)
    }

    #[tokio::test]
    async fn a_blob_round_trips_and_its_id_is_the_hash_of_its_content() {
        let (s, root) = store();
        let id = s.put(b"hello world").await.unwrap();
        assert_eq!(id, BlobId::of(b"hello world"));
        assert_eq!(s.get(&id).await.unwrap(), b"hello world");
        assert_eq!(s.len(&id).await.unwrap(), 11);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn storing_the_same_bytes_twice_is_one_object() {
        // What makes a re-delivery or a replayed job free, and dedup automatic.
        let (s, root) = store();
        let a = s.put(b"same").await.unwrap();
        let b = s.put(b"same").await.unwrap();
        assert_eq!(a, b);
        let count = walkdir_count(&root);
        assert_eq!(count, 1, "the same content produced {count} files");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn different_content_is_a_different_object() {
        let (s, root) = store();
        let a = s.put(b"one").await.unwrap();
        let b = s.put(b"two").await.unwrap();
        assert_ne!(a, b);
        assert_eq!(s.get(&a).await.unwrap(), b"one");
        assert_eq!(s.get(&b).await.unwrap(), b"two");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_missing_blob_is_not_found_rather_than_an_io_error() {
        // The HTTP layer maps NotFound to 404 and Io to 500; conflating them turns a normal
        // outcome into a page.
        let (s, root) = store();
        let id = BlobId::of(b"never stored");
        assert!(matches!(s.get(&id).await, Err(BlobError::NotFound)));
        assert!(matches!(s.len(&id).await, Err(BlobError::NotFound)));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn an_empty_blob_is_storable() {
        // A zero-byte attachment is legal MIME, and `metadata().is_ok()` must not be confused
        // with "has content".
        let (s, root) = store();
        let id = s.put(b"").await.unwrap();
        assert_eq!(s.get(&id).await.unwrap(), Vec::<u8>::new());
        assert_eq!(s.len(&id).await.unwrap(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn corruption_is_detected_when_verification_is_on() {
        let (s, root) = store();
        let id = s.put(b"original").await.unwrap();
        let path = s.path(&id);
        std::fs::write(&path, b"tampered").unwrap();
        // Off by default: the bytes come back as they are.
        assert_eq!(s.get(&id).await.unwrap(), b"tampered");
        // On: loud.
        let v = FsBlobStore::new(&root).verifying();
        assert!(matches!(v.get(&id).await, Err(BlobError::Corrupt(_))));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn only_a_64_character_lowercase_hex_string_parses_as_an_id() {
        // The ONLY validation between caller input and a filesystem path.
        assert!(BlobId::parse(&"a".repeat(64)).is_some());
        for bad in [
            "../../etc/passwd",
            "",
            &"a".repeat(63),
            &"a".repeat(65),
            &"A".repeat(64), // uppercase
            &"g".repeat(64), // not hex
            "a/b",
            &format!("{}..", "a".repeat(62)),
        ] {
            assert!(BlobId::parse(bad).is_none(), "{bad:?} parsed as a blob id");
        }
    }

    #[test]
    fn a_blob_path_stays_inside_the_root() {
        // Traversal is impossible by construction rather than by sanitising: the id is hex, so
        // the path is hex, so there is nothing to escape with.
        let s = FsBlobStore::new("/var/lib/amk/blobs");
        let id = BlobId::of(b"x");
        let p = s.path(&id);
        assert!(p.starts_with("/var/lib/amk/blobs"));
        assert!(!p.to_string_lossy().contains(".."));
        // Two levels of fan-out, asserted as the shape rather than as a component count -- the
        // count depends on how many components the ROOT has, which is not what this is about.
        let h = id.as_str();
        assert!(
            p.ends_with(format!("{}/{}/{}", &h[0..2], &h[2..4], h)),
            "unexpected layout: {}",
            p.display()
        );
    }

    fn walkdir_count(root: &Path) -> usize {
        fn rec(p: &Path, n: &mut usize) {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    let path = e.path();
                    if path.is_dir() {
                        rec(&path, n);
                    } else {
                        *n += 1;
                    }
                }
            }
        }
        let mut n = 0;
        rec(root, &mut n);
        n
    }
}
