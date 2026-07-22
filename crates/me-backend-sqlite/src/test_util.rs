//! Shared store-level test setup (Wave 2 #816).
//!
//! Relocated from the monolith's `test_utils.rs` — [`setup_memory_db`] needs
//! `crate::store::schema`, so it lives here rather than in `me-types`.
//!
//! # FK-pragma guarantee
//!
//! [`setup_memory_db`] opens the connection through [`crate::store::schema::open_memory`],
//! which applies all pragmas including `PRAGMA foreign_keys = ON`. Raw
//! `Connection::open_in_memory()` skips this pragma and silently masks FK
//! violations; every store-level test setup must use this helper instead (#485).
use rusqlite::Connection;

use crate::store::schema::{init_schema, open_memory};

/// Open an in-memory `SQLite` connection with all pragmas (including
/// `foreign_keys = ON`) and the latest schema applied.
///
/// Use this in every store-level `fn setup() -> Connection` instead of
/// `Connection::open_in_memory()`, which skips the FK pragma (#485).
pub fn setup_memory_db() -> Connection {
    let conn = open_memory().expect("open in-memory db");
    init_schema(&conn).expect("init schema");
    conn
}
