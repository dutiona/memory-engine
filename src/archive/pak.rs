use std::fs;
use std::io::Write;
use std::path::Path;

use crate::archive::types::ArchivePak;
use crate::error::{MemoryError, Result};

/// Maximum decompressed `.pak` size (4 GiB) — prevents decompression bombs.
const MAX_PAK_DECOMPRESSED_SIZE: u64 = 4 * 1024 * 1024 * 1024;

/// Write an `ArchivePak` as zstd-compressed JSON (convenience wrapper).
///
/// Thin wrapper over [`write_pak_and_hash`] for callers that don't need the hash.
// Used in unit tests below; restore tooling will also use this.
#[allow(dead_code)]
pub fn write_pak(pak: &ArchivePak, path: &Path) -> Result<()> {
    write_pak_and_hash(pak, path)?;
    Ok(())
}

/// Write a `.pak` file and return its blake3 hash.
/// Hash is computed during write (no TOCTOU). Atomic write via tmp+rename.
pub fn write_pak_and_hash(pak: &ArchivePak, path: &Path) -> Result<String> {
    let tmp_path = path.with_extension("pak.tmp");

    let file = fs::File::create(&tmp_path).map_err(|e| {
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
    use chrono::Utc;

    fn empty_pak() -> ArchivePak {
        ArchivePak {
            pak_version: 1,
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
        assert_eq!(restored.pak_version, 1);
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
}
