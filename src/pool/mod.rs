//! Bounded connection pool: N read connections + 1 exclusive write connection.
//!
//! Uses `parking_lot::Mutex` for the write lock and a bounded channel for readers.

mod connection_pool;

pub use connection_pool::ConnectionPool;
pub use connection_pool::ReadConn;
