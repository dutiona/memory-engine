use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use crate::archive::types::{ArchivePak, CURRENT_PAK_VERSION};
use crate::error::{MemoryError, Result};
use crate::store::schema::CURRENT_SCHEMA_VERSION;

/// Maximum decompressed `.pak` size (4 GiB) — prevents decompression bombs.
const MAX_PAK_DECOMPRESSED_SIZE: u64 = 4 * 1024 * 1024 * 1024;

/// Write a `.pak` file and return its blake3 hash.
/// Hash is computed during write (no TOCTOU). Atomic write via tmp+rename.
pub fn write_pak_and_hash(pak: &ArchivePak, path: &Path) -> Result<String> {
    let tmp_path = path.with_extension("pak.tmp");

    // O_EXCL: atomic creation — fails if the tmp file already exists, preventing
    // symlink/TOCTOU attacks on the predictable temp path.
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .map_err(|e| {
            MemoryError::Archive(format!(
                "failed to create temp pak file {}: {e}",
                tmp_path.display()
            ))
        })?;

    let mut hasher = blake3::Hasher::new();
    let hashing_writer = HashingWriter {
        inner: file,
        hasher: &mut hasher,
    };
    let mut encoder = zstd::Encoder::new(hashing_writer, 3)
        .map_err(|e| MemoryError::Archive(format!("failed to create zstd encoder: {e}")))?;
    serde_json::to_writer(&mut encoder, pak)?;
    encoder
        .finish()
        .map_err(|e| MemoryError::Archive(format!("failed to finalize zstd stream: {e}")))?;

    let hash = hasher.finalize().to_hex().to_string();

    fs::rename(&tmp_path, path).map_err(|e| {
        MemoryError::Archive(format!(
            "failed to rename {} -> {}: {e}",
            tmp_path.display(),
            path.display()
        ))
    })?;
    Ok(hash)
}

/// Read and decompress a `.pak` file. Caps at 4 GiB decompressed.
pub fn read_pak(path: &Path) -> Result<ArchivePak> {
    let file = fs::File::open(path).map_err(|e| {
        MemoryError::Archive(format!("failed to open pak file {}: {e}", path.display()))
    })?;
    let decoder = zstd::Decoder::new(file)
        .map_err(|e| MemoryError::Archive(format!("failed to create zstd decoder: {e}")))?;
    let limited = std::io::Read::take(decoder, MAX_PAK_DECOMPRESSED_SIZE);
    let pak: ArchivePak = serde_json::from_reader(limited)?;

    // Validate versions after deserialize, mirroring `validate_schema_version`
    // (store/schema.rs): reject archives written by a *newer* library, but
    // accept older ones (forward-only incompatibility, backward-compatible read).
    if pak.pak_version > CURRENT_PAK_VERSION {
        return Err(MemoryError::Archive(format!(
            "pak_version {} is newer than supported {CURRENT_PAK_VERSION}; \
             consider upgrading the memory-engine crate",
            pak.pak_version
        )));
    }
    if pak.engine_schema_version > CURRENT_SCHEMA_VERSION {
        return Err(MemoryError::Archive(format!(
            "engine_schema_version {} is newer than supported {CURRENT_SCHEMA_VERSION}; \
             consider upgrading the memory-engine crate",
            pak.engine_schema_version
        )));
    }

    Ok(pak)
}

/// Compute blake3 hash of a file (streaming, not `fs::read`).
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .map_err(|e| MemoryError::Archive(format!("failed to read pak file for hashing: {e}")))?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| MemoryError::Archive(format!("failed to hash pak file: {e}")))?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Verify a `.pak` file's blake3 hash matches expected.
pub fn verify_pak(path: &Path, expected_hash: &str) -> Result<bool> {
    let actual = hash_file(path)?;
    Ok(actual == expected_hash)
}

/// Hashes bytes as they pass through to the inner writer.
struct HashingWriter<'a, W: Write> {
    inner: W,
    hasher: &'a mut blake3::Hasher,
}

impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::types::CURRENT_PAK_VERSION;
    use chrono::Utc;

    /// Write an `ArchivePak` as zstd-compressed JSON (test convenience wrapper).
    ///
    /// Thin wrapper over [`write_pak_and_hash`] for callers that don't need the hash.
    fn write_pak(pak: &ArchivePak, path: &std::path::Path) -> crate::error::Result<()> {
        write_pak_and_hash(pak, path)?;
        Ok(())
    }

    fn empty_pak() -> ArchivePak {
        ArchivePak {
            pak_version: CURRENT_PAK_VERSION,
            engine_schema_version: 7,
            embed_dim: 3,
            created_at: Utc::now(),
            facts: vec![],
            edges: vec![],
        }
    }

    #[test]
    fn write_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let pak_path = dir.path().join("test.pak");
        write_pak(&empty_pak(), &pak_path).unwrap();
        assert!(pak_path.exists());
        let restored = read_pak(&pak_path).unwrap();
        assert_eq!(restored.pak_version, CURRENT_PAK_VERSION);
        assert_eq!(restored.embed_dim, 3);
        assert!(restored.facts.is_empty());
    }

    #[test]
    fn hash_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let pak_path = dir.path().join("test.pak");
        write_pak(&empty_pak(), &pak_path).unwrap();
        let hash = hash_file(&pak_path).unwrap();
        assert!(verify_pak(&pak_path, &hash).unwrap());
        assert!(!verify_pak(&pak_path, "wrong_hash").unwrap());
    }

    #[test]
    fn write_pak_and_hash_returns_consistent_hash() {
        let dir = tempfile::tempdir().unwrap();
        let pak_path = dir.path().join("test.pak");
        let hash = write_pak_and_hash(&empty_pak(), &pak_path).unwrap();
        // Verify hash matches independent computation
        let independent_hash = hash_file(&pak_path).unwrap();
        assert_eq!(hash, independent_hash);
    }

    #[test]
    fn atomic_write_no_partial_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let pak_path = dir.path().join("atomic.pak");
        let tmp_path = dir.path().join("atomic.pak.tmp");
        write_pak(&empty_pak(), &pak_path).unwrap();
        assert!(!tmp_path.exists());
        assert!(pak_path.exists());
    }

    #[test]
    fn read_pak_rejects_future_pak_version() {
        use crate::store::schema::CURRENT_SCHEMA_VERSION;
        let dir = tempfile::tempdir().unwrap();
        let pak_path = dir.path().join("future_pak.pak");
        let mut pak = empty_pak();
        pak.pak_version = CURRENT_PAK_VERSION + 1;
        pak.engine_schema_version = CURRENT_SCHEMA_VERSION;
        write_pak(&pak, &pak_path).unwrap();

        let err = read_pak(&pak_path).unwrap_err();
        match err {
            MemoryError::Archive(msg) => {
                assert!(
                    msg.contains("newer than supported"),
                    "expected 'newer than supported' in {msg:?}"
                );
                assert!(
                    msg.contains("pak_version"),
                    "expected 'pak_version' in {msg:?}"
                );
            }
            other => panic!("expected MemoryError::Archive, got {other:?}"),
        }
    }

    #[test]
    fn read_pak_rejects_future_schema_version() {
        use crate::store::schema::CURRENT_SCHEMA_VERSION;
        let dir = tempfile::tempdir().unwrap();
        let pak_path = dir.path().join("future_schema.pak");
        let mut pak = empty_pak();
        pak.engine_schema_version = CURRENT_SCHEMA_VERSION + 1;
        write_pak(&pak, &pak_path).unwrap();

        let err = read_pak(&pak_path).unwrap_err();
        match err {
            MemoryError::Archive(msg) => {
                assert!(
                    msg.contains("newer than supported"),
                    "expected 'newer than supported' in {msg:?}"
                );
                assert!(
                    msg.contains("engine_schema_version"),
                    "expected 'engine_schema_version' in {msg:?}"
                );
            }
            other => panic!("expected MemoryError::Archive, got {other:?}"),
        }
    }

    #[test]
    fn read_pak_accepts_current_and_older_versions() {
        use crate::store::schema::CURRENT_SCHEMA_VERSION;
        let dir = tempfile::tempdir().unwrap();

        // Current versions read OK.
        let current_path = dir.path().join("current.pak");
        let mut current = empty_pak();
        current.engine_schema_version = CURRENT_SCHEMA_VERSION;
        write_pak(&current, &current_path).unwrap();
        assert!(
            read_pak(&current_path).is_ok(),
            "current versions must read"
        );

        // Older schema version reads OK (backward-compat). empty_pak() already
        // stamps engine_schema_version = 7 (< CURRENT_SCHEMA_VERSION = 9).
        let older_path = dir.path().join("older.pak");
        let older = empty_pak();
        assert!(
            older.engine_schema_version < CURRENT_SCHEMA_VERSION,
            "fixture must use an older schema version to exercise backward-compat"
        );
        write_pak(&older, &older_path).unwrap();
        assert!(
            read_pak(&older_path).is_ok(),
            "older versions must still read (backward-compat)"
        );
    }
}
