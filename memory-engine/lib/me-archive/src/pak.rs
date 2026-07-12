use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use me_types::types::archive::ARCHIVE_SCHEMA_VERSION;

use crate::types::{ArchivePak, CURRENT_PAK_VERSION};
use me_types::error::{ArchiveError, Result};

/// Maximum decompressed `.pak` size (4 GiB) — prevents decompression bombs.
const MAX_PAK_DECOMPRESSED_SIZE: u64 = 4 * 1024 * 1024 * 1024;

/// Write a `.pak` file and return its blake3 hash.
/// Hash is computed during write (no TOCTOU). Atomic write via tmp+rename.
///
/// # Errors
///
/// Returns [`ArchiveError::Io`] if the temp file cannot be created, the write/rename
/// fails, or the parent directory does not exist. Returns [`ArchiveError::Codec`] if
/// the zstd encoder cannot be created or finalized. Any I/O error propagates through
/// `serde_json::to_writer` as [`MemoryError::Serialization`](me_types::error::MemoryError::Serialization).
pub fn write_pak_and_hash(pak: &ArchivePak, path: &Path) -> Result<String> {
    let tmp_path = path.with_extension("pak.tmp");

    // O_EXCL: atomic creation — fails if the tmp file already exists, preventing
    // symlink/TOCTOU attacks on the predictable temp path.
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .map_err(|e| {
            ArchiveError::Io(format!(
                "failed to create temp pak file {}: {e}",
                tmp_path.display()
            ))
        })?;

    // The temp file now exists. Any failure while serializing/compressing or
    // renaming must remove it, or a failed archive leaves an orphan `.pak.tmp`
    // on disk (CWE-459) — the temp-file analogue of the committed-`.pak` orphan
    // the caller guards against. Run the fallible work in a closure and clean up
    // on `Err`.
    let write_result: Result<String> = (|| {
        let mut hasher = blake3::Hasher::new();
        let hashing_writer = HashingWriter {
            inner: file,
            hasher: &mut hasher,
        };
        let mut encoder = zstd::Encoder::new(hashing_writer, 3)
            .map_err(|e| ArchiveError::Codec(format!("failed to create zstd encoder: {e}")))?;
        serde_json::to_writer(&mut encoder, pak)?;
        encoder
            .finish()
            .map_err(|e| ArchiveError::Codec(format!("failed to finalize zstd stream: {e}")))?;

        let hash = hasher.finalize().to_hex().to_string();

        fs::rename(&tmp_path, path).map_err(|e| {
            ArchiveError::Io(format!(
                "failed to rename {} -> {}: {e}",
                tmp_path.display(),
                path.display()
            ))
        })?;
        Ok(hash)
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_result
}

/// Read and decompress a `.pak` file. Caps the decompressed stream at
/// `MAX_PAK_DECOMPRESSED_SIZE` (4 GiB) to defend against decompression bombs
/// (CWE-409).
///
/// # Errors
///
/// Returns a [`MemoryError`](me_types::error::MemoryError) in these cases:
///
/// - [`MemoryError::Archive`](me_types::error::MemoryError::Archive) wrapping
///   [`ArchiveError::Io`] — the file cannot be opened (e.g. it does not exist or
///   is unreadable).
/// - [`MemoryError::Archive`](me_types::error::MemoryError::Archive) wrapping
///   [`ArchiveError::Codec`] — a zstd framing error detected *eagerly* at
///   `Decoder::new` (e.g. the bytes are not a zstd stream at all, so the frame
///   header is rejected up front). Note that zstd validates the *body*
///   incrementally: a corrupt or truncated stream whose header parses is usually
///   not caught here — it surfaces mid-read as `Serialization` (see below).
/// - [`MemoryError::Archive`](me_types::error::MemoryError::Archive) wrapping
///   [`ArchiveError::PakTooLarge`] — the decompressed stream exceeds the 4 GiB
///   cap (a possible decompression bomb, CWE-409). Reported as a *distinct*
///   variant so callers can tell a "too large" trip apart from an ordinary
///   parse/read failure programmatically (#333). It does **not** distinguish a
///   too-large pak from corruption in general: a corrupt stream that happens to
///   decompress past the cap is reported here, while one that fails earlier
///   surfaces as `Serialization`.
/// - [`MemoryError::Serialization`](me_types::error::MemoryError::Serialization) —
///   either the decompressed bytes are not valid JSON for an [`ArchivePak`]
///   (including a stale v1 layout missing the renamed `base_importance` field),
///   **or** the zstd stream is corrupt/truncated in a way zstd only detects
///   incrementally during the read. Because the decode is lazy, mid-stream zstd
///   corruption is wrapped by `serde_json` and bubbles up as `Serialization`,
///   not as [`Codec`](ArchiveError::Codec) — so this variant is *not* a reliable
///   "bad JSON vs. corrupt stream" discriminator.
/// - [`MemoryError::Archive`](me_types::error::MemoryError::Archive) wrapping
///   [`ArchiveError::PakVersionUnsupported`] — the archive's `pak_version` is
///   newer than this build supports (forward-incompatible).
/// - [`MemoryError::Archive`](me_types::error::MemoryError::Archive) wrapping
///   [`ArchiveError::SchemaVersionUnsupported`] — the archive's
///   `engine_schema_version` is newer than [`ARCHIVE_SCHEMA_VERSION`].
///
/// Older `pak_version` / `engine_schema_version` values are accepted
/// (backward-compatible read); only newer ones are rejected.
///
/// # Which version is checked
///
/// The gate compares against [`ARCHIVE_SCHEMA_VERSION`] — the **backend-independent**
/// logical content-schema version of the `.pak` format (L0, `me-types`). The *same*
/// constant is stamped on write (this crate's `manage::build_pak`),
/// so the write and read sides are symmetric **by construction** and cannot drift apart.
///
/// It is deliberately **not** a backend's schema version. A `.pak` is a portable blob of
/// `me-types` DTOs, so "can I read this pak?" is a question about DTO shape, not about
/// any backend's migration history — and the backends' counters are not comparable
/// (`SQLite` is at 14, Postgres independently at 1). Sourcing the check from a concrete
/// backend would also put an L3→backend edge on the archive primitive — impossible here
/// by construction, since this crate (`me-archive`) has no dependency on any
/// `me-backend-*` crate (Wave 2 #816 / S4, sub-PR 3b).
pub fn read_pak(path: &Path) -> Result<ArchivePak> {
    read_pak_capped(path, MAX_PAK_DECOMPRESSED_SIZE)
}

/// Cap-parameterized core of [`read_pak`].
///
/// Splitting the byte cap out as a parameter lets the cap-firing path be tested
/// with a tiny limit instead of a real 4 GiB payload (#299). [`read_pak`] is the
/// only non-test caller and always passes [`MAX_PAK_DECOMPRESSED_SIZE`].
fn read_pak_capped(path: &Path, cap: u64) -> Result<ArchivePak> {
    let file = fs::File::open(path).map_err(|e| {
        ArchiveError::Io(format!("failed to open pak file {}: {e}", path.display()))
    })?;
    let decoder = zstd::Decoder::new(file)
        .map_err(|e| ArchiveError::Codec(format!("failed to create zstd decoder: {e}")))?;
    // Read up to `cap + 1` bytes. The documented contract is *inclusive* — a pak
    // whose decompressed size is exactly `cap` bytes is valid and must read. With
    // a bare `take(decoder, cap)` such a pak consumes all `cap` bytes, leaving
    // `limit() == 0`, and the post-parse check below would falsely reject it
    // (off-by-one). The one-byte slack lets a legitimately exactly-`cap` payload
    // through (it leaves `limit() == 1`) while still bounding memory, so the
    // `limit() == 0` check below now trips *only* when MORE than `cap` bytes were
    // decompressed — a genuine overflow.
    let mut limited = std::io::Read::take(decoder, cap.saturating_add(1));
    let parsed: serde_json::Result<ArchivePak> = serde_json::from_reader(&mut limited);

    // Check the cap *before* propagating any serde error. When a decompression
    // bomb exceeds the cap, `Take` returns EOF and `serde_json` fails with a
    // truncated-input error indistinguishable from genuine corruption — exactly
    // the deficiency this guards (#333, CWE-409). By inspecting the cap first we
    // surface the bomb as a *distinct* [`ArchiveError::PakTooLarge`] error
    // regardless of whether serde happened to parse a complete prefix or choked
    // on the truncation. Because we read `cap + 1` bytes, `limit() == 0` means
    // strictly MORE than `cap` bytes were consumed (an inclusive cap was
    // exceeded), so a valid exactly-`cap` pak never reaches this branch.
    if limited.limit() == 0 {
        return Err(ArchiveError::PakTooLarge { cap }.into());
    }
    let pak = parsed?;

    // Validate versions after deserialize, mirroring `validate_schema_version`
    // (store/schema.rs): reject archives written by a *newer* library, but
    // accept older ones (forward-only incompatibility, backward-compatible read).
    if pak.pak_version > CURRENT_PAK_VERSION {
        return Err(ArchiveError::PakVersionUnsupported {
            found: pak.pak_version,
            supported: CURRENT_PAK_VERSION,
        }
        .into());
    }
    if pak.engine_schema_version > ARCHIVE_SCHEMA_VERSION {
        return Err(ArchiveError::SchemaVersionUnsupported {
            found: pak.engine_schema_version,
            supported: ARCHIVE_SCHEMA_VERSION,
        }
        .into());
    }

    Ok(pak)
}

/// Compute blake3 hash of a file (streaming, not `fs::read`).
///
/// # Errors
///
/// Returns [`ArchiveError::Io`] if the file cannot be opened or read.
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .map_err(|e| ArchiveError::Io(format!("failed to read pak file for hashing: {e}")))?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| ArchiveError::Io(format!("failed to hash pak file: {e}")))?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Verify a `.pak` file's blake3 hash matches expected.
///
/// # Errors
///
/// Returns [`ArchiveError::Io`] if the file cannot be opened or read (propagated from
/// [`hash_file`]).
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
    use crate::types::CURRENT_PAK_VERSION;
    use chrono::Utc;
    use me_types::error::MemoryError;

    /// Write an `ArchivePak` as zstd-compressed JSON (test convenience wrapper).
    ///
    /// Thin wrapper over [`write_pak_and_hash`] for callers that don't need the hash.
    fn write_pak(pak: &ArchivePak, path: &std::path::Path) -> me_types::error::Result<()> {
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
        let dir = tempfile::tempdir().unwrap();
        let pak_path = dir.path().join("future_pak.pak");
        let mut pak = empty_pak();
        pak.pak_version = CURRENT_PAK_VERSION + 1;
        pak.engine_schema_version = ARCHIVE_SCHEMA_VERSION;
        write_pak(&pak, &pak_path).unwrap();

        let err = read_pak(&pak_path).unwrap_err();
        let display = err.to_string();
        match err {
            MemoryError::Archive(ArchiveError::PakVersionUnsupported { found, supported }) => {
                assert_eq!(found, CURRENT_PAK_VERSION + 1);
                assert_eq!(supported, CURRENT_PAK_VERSION);
            }
            other => panic!("expected Archive(PakVersionUnsupported), got {other:?}"),
        }
        // Display byte-preservation: message still names the field and the cause.
        assert!(
            display.contains("newer than supported"),
            "expected 'newer than supported' in {display:?}"
        );
        assert!(
            display.contains("pak_version"),
            "expected 'pak_version' in {display:?}"
        );
    }

    #[test]
    fn read_pak_rejects_future_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let pak_path = dir.path().join("future_schema.pak");
        let mut pak = empty_pak();
        pak.engine_schema_version = ARCHIVE_SCHEMA_VERSION + 1;
        write_pak(&pak, &pak_path).unwrap();

        let err = read_pak(&pak_path).unwrap_err();
        let display = err.to_string();
        match err {
            MemoryError::Archive(ArchiveError::SchemaVersionUnsupported { found, supported }) => {
                assert_eq!(found, ARCHIVE_SCHEMA_VERSION + 1);
                assert_eq!(supported, ARCHIVE_SCHEMA_VERSION);
            }
            other => panic!("expected Archive(SchemaVersionUnsupported), got {other:?}"),
        }
        // Display byte-preservation: message still names the field and the cause.
        assert!(
            display.contains("newer than supported"),
            "expected 'newer than supported' in {display:?}"
        );
        assert!(
            display.contains("engine_schema_version"),
            "expected 'engine_schema_version' in {display:?}"
        );
    }

    #[test]
    fn read_pak_accepts_current_and_older_versions() {
        let dir = tempfile::tempdir().unwrap();

        // Current versions read OK.
        let current_path = dir.path().join("current.pak");
        let mut current = empty_pak();
        current.engine_schema_version = ARCHIVE_SCHEMA_VERSION;
        write_pak(&current, &current_path).unwrap();
        assert!(
            read_pak(&current_path).is_ok(),
            "current versions must read"
        );

        // Older schema version reads OK (backward-compat). empty_pak() already
        // stamps engine_schema_version = 7 (< ARCHIVE_SCHEMA_VERSION = 14).
        let older_path = dir.path().join("older.pak");
        let older = empty_pak();
        assert!(
            older.engine_schema_version < ARCHIVE_SCHEMA_VERSION,
            "fixture must use an older schema version to exercise backward-compat"
        );
        write_pak(&older, &older_path).unwrap();
        assert!(
            read_pak(&older_path).is_ok(),
            "older versions must still read (backward-compat)"
        );
    }

    // --- read_pak error paths (#299) ---

    #[test]
    fn read_pak_nonexistent_path_errors_io() {
        let dir = tempfile::tempdir().unwrap();
        let ghost = dir.path().join("ghost.pak");
        let err = read_pak(&ghost).unwrap_err();
        let display = err.to_string();
        match err {
            MemoryError::Archive(ArchiveError::Io(msg)) => {
                assert!(
                    msg.contains("failed to open pak file"),
                    "expected open-failure message, got {msg:?}"
                );
            }
            other => panic!("expected Archive(Io), got {other:?}"),
        }
        assert!(
            display.contains("failed to open pak file"),
            "display must name the open failure, got {display:?}"
        );
    }

    #[test]
    fn read_pak_non_zstd_bytes_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("garbage.pak");
        std::fs::write(&p, b"this is plainly not a zstd stream").unwrap();
        let err = read_pak(&p).unwrap_err();
        // `zstd::Decoder::new` does NOT eagerly validate the frame header — the
        // bad magic surfaces *lazily* on the first read, inside
        // `serde_json::from_reader`, which wraps the decoder I/O error as a serde
        // error → MemoryError::Serialization. Either way `read_pak` reliably
        // errors on non-zstd input (the property #299 case 2 asks for); we assert
        // the actual variant so a future eager-validation change is caught.
        assert!(
            matches!(err, MemoryError::Serialization(_)),
            "non-zstd bytes must error (surfaced as Serialization via the lazy \
             decoder), got {err:?}"
        );
    }

    #[test]
    fn read_pak_valid_zstd_invalid_json_errors_serialization() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("badjson.pak");
        // A well-formed zstd stream whose decompressed payload is not valid JSON
        // for an ArchivePak — must surface as Serialization, NOT Codec/Io.
        let file = std::fs::File::create(&p).unwrap();
        let mut enc = zstd::Encoder::new(file, 3).unwrap();
        enc.write_all(b"this is not json").unwrap();
        enc.finish().unwrap();

        let err = read_pak(&p).unwrap_err();
        assert!(
            matches!(err, MemoryError::Serialization(_)),
            "expected Serialization for invalid JSON, got {err:?}"
        );
    }

    #[test]
    fn read_pak_cap_fires_distinct_pak_too_large_error() {
        // The decompression-bomb cap (#333): a valid pak whose decompressed JSON
        // exceeds a (tiny, test-injected) cap must surface as a *distinct*
        // PakTooLarge error — proving the limit-exceeded case is no longer
        // indistinguishable from an ordinary truncated-JSON Serialization error,
        // nor conflated with a Codec (corrupt-zstd) error.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bomb.pak");
        write_pak(&empty_pak(), &p).unwrap();

        // empty_pak()'s JSON is well over 1 byte, so a 1-byte cap trips reliably.
        let err = read_pak_capped(&p, 1).unwrap_err();
        match err {
            MemoryError::Archive(ArchiveError::PakTooLarge { cap }) => {
                assert_eq!(cap, 1, "PakTooLarge must carry the cap that was exceeded");
            }
            other => panic!("expected Archive(PakTooLarge) for the cap trip, got {other:?}"),
        }

        // Sanity: the SAME file reads fine under the real (4 GiB) cap, so the error
        // above is purely the cap firing — not a corrupt fixture.
        assert!(
            read_pak(&p).is_ok(),
            "fixture must read fine under the production cap"
        );
    }

    /// FIX 1 boundary (#258, HIGH off-by-one): the cap is documented as
    /// *inclusive* ("caps at N bytes"), so a pak whose decompressed size is
    /// EXACTLY the cap must read successfully. Before the `cap + 1` slack, such a
    /// pak consumed all `cap` bytes (leaving `limit() == 0`) and was falsely
    /// rejected as a decompression bomb.
    #[test]
    fn read_pak_exactly_cap_bytes_reads_ok() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("exact.pak");
        write_pak(&empty_pak(), &p).unwrap();

        // Measure the exact decompressed length of this pak by decompressing it
        // through the same zstd decoder the reader uses.
        let decompressed_len = {
            let file = std::fs::File::open(&p).unwrap();
            let mut decoder = zstd::Decoder::new(file).unwrap();
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut buf).unwrap();
            u64::try_from(buf.len()).unwrap()
        };
        assert!(decompressed_len > 0, "fixture must decompress to >0 bytes");

        // A cap set to EXACTLY the decompressed length must read successfully —
        // the inclusive contract. This is the assertion that FAILS under the
        // pre-fix `take(decoder, cap)` (it would trip the bomb guard at limit 0).
        let restored = read_pak_capped(&p, decompressed_len)
            .expect("a pak whose size equals the cap must read (inclusive cap)");
        assert_eq!(restored.pak_version, CURRENT_PAK_VERSION);
    }

    /// FIX 1 boundary companion: a pak one byte OVER the cap must still be
    /// rejected as [`PakTooLarge`](ArchiveError::PakTooLarge) — proving the
    /// `cap + 1` slack widened the acceptance window by exactly one byte (the
    /// inclusive boundary) and not more.
    #[test]
    fn read_pak_one_byte_over_cap_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("over.pak");
        write_pak(&empty_pak(), &p).unwrap();

        let decompressed_len = {
            let file = std::fs::File::open(&p).unwrap();
            let mut decoder = zstd::Decoder::new(file).unwrap();
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut buf).unwrap();
            u64::try_from(buf.len()).unwrap()
        };
        assert!(
            decompressed_len >= 2,
            "fixture must decompress to >=2 bytes so cap = len - 1 is a real (positive) cap"
        );

        // cap = len - 1 means the payload is exactly one byte over the cap.
        let cap = decompressed_len - 1;
        let err = read_pak_capped(&p, cap).unwrap_err();
        match err {
            MemoryError::Archive(ArchiveError::PakTooLarge { cap: reported }) => {
                assert_eq!(reported, cap, "PakTooLarge must carry the exceeded cap");
            }
            other => panic!("expected Archive(PakTooLarge) one byte over the cap, got {other:?}"),
        }
    }

    #[test]
    fn write_pak_nonexistent_parent_dir_errors_io() {
        let dir = tempfile::tempdir().unwrap();
        // Parent directory `missing/` was never created.
        let p = dir.path().join("missing").join("out.pak");
        let err = write_pak_and_hash(&empty_pak(), &p).unwrap_err();
        match err {
            MemoryError::Archive(ArchiveError::Io(msg)) => {
                assert!(
                    msg.contains("failed to create temp pak file"),
                    "expected temp-file create failure, got {msg:?}"
                );
            }
            other => panic!("expected Archive(Io), got {other:?}"),
        }
    }
}
