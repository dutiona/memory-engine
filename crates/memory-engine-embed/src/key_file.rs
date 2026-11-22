//! The embedder API key read from a file (#1101), so a deployment can mount it as a secret
//! instead of passing it in a flag or the environment.

use std::path::Path;

use memory_engine::error::{ConflictError, MemoryError};

/// Read the embedder API key held in the file at `path`.
///
/// The file holds the key alone, with one trailing `\n` allowed. The key is returned
/// as is; the caller holds it like a key given directly (never logged, no `Debug`).
///
/// # Errors
///
/// [`MemoryError::Conflict`] (`PolicyParameter`) when the file is missing or unreadable, is
/// not UTF-8, holds an empty key, or holds any control character besides that one trailing
/// `\n` (so a `\r\n` ending or a second line is refused). Every error names `path` and never
/// contains a byte of the file.
///
/// # Examples
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("embed-key");
/// std::fs::write(&path, "k3y\n").unwrap();
/// assert_eq!(memory_engine_embed::read_api_key_file(&path).unwrap(), "k3y");
/// ```
pub fn read_api_key_file(path: &Path) -> Result<String, MemoryError> {
    let refuse = |why: &str| {
        MemoryError::Conflict(ConflictError::PolicyParameter(format!(
            "the embedder key file {} {why}",
            path.display()
        )))
    };
    let bytes =
        std::fs::read(path).map_err(|e| refuse(&format!("cannot be read: {}", e.kind())))?;
    let text = String::from_utf8(bytes).map_err(|_| refuse("is not UTF-8"))?;
    let key = text.strip_suffix('\n').unwrap_or(&text);
    if key.is_empty() {
        return Err(refuse("is empty"));
    }
    if key.chars().any(char::is_control) {
        return Err(refuse(
            "holds a control character: it must hold the key alone, with one trailing newline at most",
        ));
    }
    Ok(key.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key built at run time, so no secret-shaped literal sits in the source.
    fn secret() -> String {
        format!("{}-{}", "embed", std::process::id())
    }

    fn file_with(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embed-key");
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn reads_the_key_with_or_without_one_trailing_newline() {
        let key = secret();
        for content in [key.clone(), format!("{key}\n")] {
            let (_dir, path) = file_with(content.as_bytes());
            assert_eq!(read_api_key_file(&path).unwrap(), key);
        }
    }

    #[test]
    fn refuses_every_bad_file_naming_the_path_and_no_byte_of_it() {
        let key = secret();
        let cases: Vec<Vec<u8>> = vec![
            b"".to_vec(),
            b"\n".to_vec(),
            format!("{key}\nmore").into_bytes(),
            format!("{key}\r\n").into_bytes(),
            [key.as_bytes(), &[0xff, 0xfe]].concat(),
        ];
        for content in cases {
            let (_dir, path) = file_with(&content);
            let err = read_api_key_file(&path).unwrap_err().to_string();
            assert!(err.contains(&path.display().to_string()), "{err}");
            assert!(!err.contains(&key) && !err.contains("more"), "{err}");
        }
        let dir = tempfile::tempdir().unwrap();
        for path in [dir.path().join("missing"), dir.path().to_path_buf()] {
            let err = read_api_key_file(&path).unwrap_err().to_string();
            assert!(err.contains(&path.display().to_string()), "{err}");
        }
    }
}
