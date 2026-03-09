//! In-memory knowledge graph backed by `petgraph`, loaded from SQLite edge table.

mod memory_graph;

pub use memory_graph::{EdgeData, MemoryGraph};
