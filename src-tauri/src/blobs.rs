//! Content-addressed blob store (spec §18): bytes live at
//! `app_data/blobs/<first-two-hex>/<blake3-hash>`, so identical content is
//! stored exactly once, writes are atomic (temp + rename), and nothing in the
//! store is ever mutated. Attachment metadata lives on notes; blobs are only
//! bytes. Note windows read them via the `pinq-blob://` URI protocol.

use std::io;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

pub fn blobs_dir(app: &AppHandle) -> io::Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(io::Error::other)?
        .join("blobs");
    Ok(dir)
}

/// Store bytes, returning their hash. No-op if the blob already exists.
pub fn store_blob(app: &AppHandle, bytes: &[u8]) -> io::Result<String> {
    store_blob_in(&blobs_dir(app)?, bytes)
}

/// Resolve a hash to its blob path — None if malformed or missing.
pub fn blob_path(app: &AppHandle, hash: &str) -> Option<PathBuf> {
    blob_path_in(&blobs_dir(app).ok()?, hash)
}

/// Dir-parameterized core (testable without an AppHandle).
pub fn store_blob_in(blobs: &std::path::Path, bytes: &[u8]) -> io::Result<String> {
    let hash = blake3::hash(bytes).to_hex().to_string();
    let dir = blobs.join(&hash[..2]);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(&hash);
    if !path.exists() {
        let tmp = dir.join(format!("{hash}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(hash)
}

pub fn blob_path_in(blobs: &std::path::Path, hash: &str) -> Option<PathBuf> {
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let path = blobs.join(&hash[..2]).join(hash);
    path.exists().then_some(path)
}

static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn tmp_path(blobs: &std::path::Path) -> PathBuf {
    let n = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    blobs.join(format!("incoming.{}.{n}.tmp", std::process::id()))
}

/// Size of a stored blob, without reading it.
pub fn blob_size(app: &AppHandle, hash: &str) -> Option<u64> {
    std::fs::metadata(blob_path(app, hash)?).ok().map(|m| m.len())
}

/// Stream a file into the store without holding it in memory: hash while
/// copying to a temp file, then rename into place. Returns (hash, size).
/// This is the path for arbitrarily large drag-dropped files.
pub fn store_blob_from_file(app: &AppHandle, src: &std::path::Path) -> io::Result<(String, u64)> {
    store_blob_from_file_in(&blobs_dir(app)?, src)
}

pub fn store_blob_from_file_in(
    blobs: &std::path::Path,
    src: &std::path::Path,
) -> io::Result<(String, u64)> {
    let mut reader = std::fs::File::open(src)?;
    let mut incoming = IncomingBlob::new(blobs)?;
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = std::io::Read::read(&mut reader, &mut buf)?;
        if n == 0 {
            break;
        }
        incoming.write(&buf[..n])?;
    }
    let size = incoming.size;
    Ok((incoming.finish()?, size))
}

/// A blob arriving in chunks (LAN sync). Bytes stream straight to a temp file
/// while the hash accumulates, so multi-GB transfers never sit in memory.
pub struct IncomingBlob {
    blobs: PathBuf,
    tmp: PathBuf,
    file: std::io::BufWriter<std::fs::File>,
    hasher: blake3::Hasher,
    pub size: u64,
}

impl IncomingBlob {
    pub fn new(blobs: &std::path::Path) -> io::Result<Self> {
        std::fs::create_dir_all(blobs)?;
        let tmp = tmp_path(blobs);
        Ok(Self {
            blobs: blobs.to_path_buf(),
            file: std::io::BufWriter::new(std::fs::File::create(&tmp)?),
            tmp,
            hasher: blake3::Hasher::new(),
            size: 0,
        })
    }

    pub fn write(&mut self, chunk: &[u8]) -> io::Result<()> {
        self.hasher.update(chunk);
        std::io::Write::write_all(&mut self.file, chunk)?;
        self.size += chunk.len() as u64;
        Ok(())
    }

    /// Seal the blob under its content hash and return it.
    pub fn finish(mut self) -> io::Result<String> {
        std::io::Write::flush(&mut self.file)?;
        drop(self.file);
        let hash = self.hasher.finalize().to_hex().to_string();
        let dir = self.blobs.join(&hash[..2]);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(&hash);
        if path.exists() {
            let _ = std::fs::remove_file(&self.tmp);
        } else {
            std::fs::rename(&self.tmp, &path)?;
        }
        Ok(hash)
    }

    /// Drop the partial bytes (failed or mismatched transfer).
    pub fn discard(self) {
        drop(self.file);
        let _ = std::fs::remove_file(&self.tmp);
    }
}

/// Garbage-collect blobs no note references anymore (purged notes, emptied
/// trash). `min_age` protects blobs that were just written for ops still in
/// flight to peers. Returns (files removed, bytes reclaimed). Blobs are
/// content-addressed and never mutated, so deleting an unreferenced one can
/// only ever cost a re-transfer if a peer re-introduces it.
pub fn gc_blobs_in(
    blobs: &std::path::Path,
    referenced: &std::collections::HashSet<String>,
    min_age: std::time::Duration,
) -> (usize, u64) {
    let mut removed = 0usize;
    let mut bytes = 0u64;
    let Ok(shards) = std::fs::read_dir(blobs) else { return (0, 0) };
    for shard in shards.filter_map(|e| e.ok()) {
        // Root-level incoming.*.tmp files from interrupted streaming writes.
        if shard.path().is_file() {
            let name = shard.file_name().to_string_lossy().to_string();
            let old_enough = shard
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .map(|age| age >= min_age)
                .unwrap_or(false);
            if name.contains(".tmp") && old_enough {
                let bytes_len = shard.metadata().map(|m| m.len()).unwrap_or(0);
                if std::fs::remove_file(shard.path()).is_ok() {
                    removed += 1;
                    bytes += bytes_len;
                }
            }
            continue;
        }
        let Ok(files) = std::fs::read_dir(shard.path()) else { continue };
        for entry in files.filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            // Stale tmp files from interrupted writes are always fair game.
            let is_tmp = name.contains(".tmp");
            if !is_tmp && referenced.contains(&name) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let old_enough = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|age| age >= min_age)
                .unwrap_or(false);
            if old_enough && std::fs::remove_file(&path).is_ok() {
                removed += 1;
                bytes += meta.len();
            }
        }
    }
    (removed, bytes)
}

/// App-level GC pass: everything any note (trash included) still references
/// stays; the rest goes once it's a day old.
pub fn gc(app: &AppHandle, store: &crate::store::Store) -> (usize, u64) {
    let Ok(dir) = blobs_dir(app) else { return (0, 0) };
    let referenced: std::collections::HashSet<String> = store
        .all_notes()
        .iter()
        .flat_map(|n| n.attachments.iter().map(|a| a.hash.clone()))
        .collect();
    gc_blobs_in(&dir, &referenced, std::time::Duration::from_secs(24 * 60 * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_store_matches_in_memory_hash() {
        let dir = std::env::temp_dir().join(format!("pinq-blob-stream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Larger than one read buffer, and not a tidy multiple of anything.
        let payload: Vec<u8> = (0..3 * 1024 * 1024 + 17).map(|i| (i % 251) as u8).collect();
        let src = dir.join("source.bin");
        std::fs::write(&src, &payload).unwrap();

        let expected = blake3::hash(&payload).to_hex().to_string();
        let (hash, size) = store_blob_from_file_in(&dir, &src).unwrap();
        assert_eq!(hash, expected);
        assert_eq!(size, payload.len() as u64);
        assert!(blob_path_in(&dir, &hash).is_some());

        // Chunked arrival (IncomingBlob) lands identically, and a re-store of
        // existing content is a clean no-op.
        let mut incoming = IncomingBlob::new(&dir).unwrap();
        for chunk in payload.chunks(128 * 1024) {
            incoming.write(chunk).unwrap();
        }
        assert_eq!(incoming.finish().unwrap(), expected);
        let (again, _) = store_blob_from_file_in(&dir, &src).unwrap();
        assert_eq!(again, expected);

        // No stray tmp files left behind.
        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(strays.is_empty(), "leftover tmp files: {strays:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn gc_keeps_referenced_and_young_blobs() {
        let dir = std::env::temp_dir().join(format!("pinq-blob-gc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let keep = store_blob_in(&dir, b"still referenced").unwrap();
        let drop_ = store_blob_in(&dir, b"orphaned bytes").unwrap();
        let referenced: std::collections::HashSet<String> = [keep.clone()].into();

        // With a min-age window, nothing is young enough to delete yet.
        let (n, _) = gc_blobs_in(&dir, &referenced, std::time::Duration::from_secs(3600));
        assert_eq!(n, 0, "fresh blobs are protected");

        // With the window at zero, only the orphan goes.
        let (n, bytes) = gc_blobs_in(&dir, &referenced, std::time::Duration::ZERO);
        assert_eq!(n, 1);
        assert_eq!(bytes, b"orphaned bytes".len() as u64);
        assert!(blob_path_in(&dir, &keep).is_some());
        assert!(blob_path_in(&dir, &drop_).is_none());
        std::fs::remove_dir_all(dir).ok();
    }
}
