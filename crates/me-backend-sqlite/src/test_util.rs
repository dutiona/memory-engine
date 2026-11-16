//! Shared store-level test setup (Wave 2 #816).
//!
//! Relocated from the monolith's `test_utils.rs` — [`setup_memory_db`] needs
//! `crate::store::schema`, so it lives here rather than in `me-types`.
//!
//! # FK-pragma guarantee
//!
//! [`setup_memory_db`] opens the connection through [`crate::store::schema::open_memory`],
//! which applies all pragmas including `PRAGMA foreign_keys = ON`, the setting the
//! engine runs with. The bundled SQLite build also defaults foreign keys on
//! (`SQLITE_DEFAULT_FOREIGN_KEYS=1`), but a raw `Connection::open_in_memory()` loses
//! that on any other build; every store-level test setup uses this helper (#485).
use rusqlite::Connection;

use crate::store::schema::{init_schema, open_memory};

/// Open an in-memory `SQLite` connection with all pragmas (including
/// `foreign_keys = ON`) and the latest schema applied.
///
/// Use this in every store-level `fn setup() -> Connection` instead of
/// `Connection::open_in_memory()`, which applies none of the engine's pragmas (#485).
pub fn setup_memory_db() -> Connection {
    let conn = open_memory().expect("open in-memory db");
    init_schema(&conn).expect("init schema");
    conn
}
