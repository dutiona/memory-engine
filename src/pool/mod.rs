//! Bounded connection pool: N read connections + 1 exclusive write connection.
//!
//! Uses `parking_lot::Mutex` + `Condvar` for both the write lock and the reader pool.

mod connection_pool;

pub use connection_pool::{ConnectionPool, WriteGuard};
