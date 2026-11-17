//! The store lock: one exclusive advisory lock beside the database (ADR-0020 §8).
//!
//! The server holds it for its whole life, and every command that opens the store writable
//! takes it or refuses, so maintenance never runs beside a live server. It is an OS file
//! lock (`flock` on Unix): the kernel releases it when its holder exits, even on `SIGKILL`,
//! so a killed server leaves no stale lock. It holds between processes of one host, the
//! containers sharing the store's volume included, never across hosts or on a network
//! filesystem. Read-only opens never take it: they never migrate and never write.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// A held store lock. The lock is released when this value drops.
#[derive(Debug)]
#[must_use = "the store lock is released as soon as it is dropped"]
pub struct StoreLock {
    _file: File,
    path: PathBuf,
}

/// Why [`StoreLock::acquire`] failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreLockError {
    /// Another process holds the lock: a running server or another maintenance command.
    #[error("the store is in use by another process ({} is locked)", lock.display())]
    Held {
        /// The lock file.
        lock: PathBuf,
    },
    /// The lock file could not be opened or locked.
    #[error("cannot take the store lock {}: {source}", lock.display())]
    Io {
        /// The lock file.
        lock: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

impl StoreLock {
    /// Take the exclusive lock for the database at `db`, without waiting.
    ///
    /// The lock file is `db` with `.store-lock` appended (not `.lock`, which SQLite's
    /// unix-dotfile VFS uses). It is created when missing and never deleted: unlinking a
    /// held lock file would hand the next process a fresh inode and a second "exclusive"
    /// lock.
    ///
    /// # Errors
    ///
    /// [`StoreLockError::Held`] when another process holds the lock, and
    /// [`StoreLockError::Io`] when the lock file cannot be opened or locked.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_engine::{StoreLock, StoreLockError};
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let db = dir.path().join("memory.db");
    /// let held = StoreLock::acquire(&db).unwrap();
    /// assert!(matches!(StoreLock::acquire(&db), Err(StoreLockError::Held { .. })));
    /// drop(held);
    /// assert!(StoreLock::acquire(&db).is_ok());
    /// ```
    pub fn acquire(db: &Path) -> Result<Self, StoreLockError> {
        let mut name = db.as_os_str().to_owned();
        name.push(".store-lock");
        let path = PathBuf::from(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            // A lock file another user created (a maintenance command run as root) still
            // locks: the lock needs only a read handle.
            .or_else(|e| match e.kind() {
                io::ErrorKind::PermissionDenied => File::open(&path),
                _ => Err(e),
            })
            .map_err(|source| StoreLockError::Io {
                lock: path.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(TryLockError::WouldBlock) => Err(StoreLockError::Held { lock: path }),
            Err(TryLockError::Error(source)) => Err(StoreLockError::Io { lock: path, source }),
        }
    }

    /// The lock file this lock holds.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_acquire_is_refused_until_the_first_drops() {
        // flock contends per open file description, so one process can test it.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let held = StoreLock::acquire(&db).unwrap();
        assert!(matches!(
            StoreLock::acquire(&db),
            Err(StoreLockError::Held { .. })
        ));
        drop(held);
        assert!(StoreLock::acquire(&db).is_ok());
    }

    #[test]
    fn the_lock_file_sits_beside_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let lock = StoreLock::acquire(&db).unwrap();
        assert_eq!(lock.path(), dir.path().join("memory.db.store-lock"));
        assert!(lock.path().is_file());
        assert!(!db.exists(), "taking the lock must not create the database");
    }

    #[test]
    fn an_unopenable_lock_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("missing-dir").join("memory.db");
        let err = StoreLock::acquire(&db).unwrap_err();
        assert!(matches!(err, StoreLockError::Io { .. }), "{err}");
    }

    #[test]
    fn the_held_message_names_the_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let _held = StoreLock::acquire(&db).unwrap();
        let err = StoreLock::acquire(&db).unwrap_err().to_string();
        assert!(err.contains("memory.db.store-lock"), "{err}");
        assert!(err.contains("in use by another process"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_lock_file_this_user_cannot_write_still_locks() {
        // e.g. left by a maintenance command run as root: flock needs only a read handle.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        drop(StoreLock::acquire(&db).unwrap());
        let lock = dir.path().join("memory.db.store-lock");
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o444)).unwrap();
        let held = StoreLock::acquire(&db).unwrap();
        assert!(matches!(
            StoreLock::acquire(&db),
            Err(StoreLockError::Held { .. })
        ));
        drop(held);
    }
}
