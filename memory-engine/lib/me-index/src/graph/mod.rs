//! In-memory graph backed by `petgraph`, mirroring the active edges in `SQLite`.
//!
//! Node weights are fact ids; edge weights are [`EdgeData`]. Only active
//! (non-expired) edges are represented, and the graph is edge-only — isolated
//! facts with no active edge are not materialized as nodes (matching
//! [`MemoryGraph::from_active_edges`] semantics).

mod memory_graph;

pub use memory_graph::{EdgeData, MemoryGraph};
