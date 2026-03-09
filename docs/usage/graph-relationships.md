# Graph Relationships

memory-engine maintains a knowledge graph where facts are nodes and edges represent typed relationships between them. The graph is used for importance scoring (during forgetting), connected component analysis, and traversal queries.

## Architecture

The graph is backed by two layers:

- **SQLite** (`edges` table) -- persistent storage, source of truth for edge data.
- **petgraph** (`DiGraph<i64, EdgeData>`) -- in-memory directed graph loaded from SQLite on engine open. All graph queries run against the in-memory representation for speed.

On `MemoryEngine::open`, all active (non-expired) edges are loaded from SQLite into the petgraph instance. The two layers are kept in sync: every mutation (insert, expire) updates both SQLite and the in-memory graph within the same write lock.

## How edges are created

Edges are not created directly through a public API. They are produced by two internal processes:

**Conflict resolution** -- When `resolve_conflict` determines that a new fact supersedes an old one (via `CrudDecision::Update`), the engine inserts a directed edge from the new fact to the old fact with relation type `"supersedes"`. The old fact is soft-deleted (`t_expired` set).

**Consolidation** -- During three-pass consolidation, when duplicate facts are identified and merged, edges may be created to track the dedup lineage.

## The `Edge` and `NewEdge` structs

The persisted edge:

```rust
pub struct Edge {
    pub id: i64,
    pub source_fact_id: i64,
    pub target_fact_id: i64,
    pub relation_type: String,
    pub weight: f64,
    pub t_created: DateTime<Utc>,
    pub t_expired: Option<DateTime<Utc>>,
    pub scope_id: i64,
}
```

For insertion:

```rust
pub struct NewEdge {
    pub source_fact_id: i64,
    pub target_fact_id: i64,
    pub relation_type: String,
    pub weight: f64,
    pub t_created: DateTime<Utc>,
    pub t_expired: Option<DateTime<Utc>>,
    pub scope_id: i64,
}
```

The `relation_type` is a free-form string. Built-in processes use `"supersedes"` (conflict resolution) and `"duplicates"` (consolidation), but the schema does not constrain it.

## In-memory edge data

The petgraph edge weight stores a subset of the full edge:

```rust
pub struct EdgeData {
    pub edge_id: i64,
    pub relation_type: String,
    pub weight: f64,
}
```

## Graph query methods

All graph methods take `&self` and are thread-safe (the graph is behind an `RwLock`).

### `graph_degree(fact_id) -> usize`

Returns the total degree (in-edges + out-edges) for a fact. Returns 0 if the fact is not in the graph. This is used internally by the forgetting system -- highly connected facts get a higher importance score and are less likely to be pruned.

```rust
let degree = engine.graph_degree(fact_id);
println!("Fact {fact_id} has {degree} connections");
```

### `graph_neighbors(fact_id) -> Vec<i64>`

Returns the outgoing neighbors of a fact (fact IDs reachable by following directed edges from this node). Returns an empty vec if the fact is not in the graph.

```rust
let neighbors = engine.graph_neighbors(fact_id);
for neighbor_id in &neighbors {
    let fact = engine.get_fact(*neighbor_id)?;
    println!("  -> {}: {}", neighbor_id, fact.content);
}
```

### `graph_component(fact_id) -> Vec<i64>`

Returns all fact IDs in the connected component containing `fact_id`. Treats the directed graph as undirected for connectivity -- edges are traversed in both directions. Returns an empty vec if the fact is not in the graph.

```rust
let component = engine.graph_component(fact_id);
println!("Component has {} facts", component.len());
```

### `graph_stats() -> (usize, usize)`

Returns `(node_count, edge_count)` for the entire graph.

```rust
let (nodes, edges) = engine.graph_stats();
println!("Graph: {nodes} nodes, {edges} edges");
```

### `graph_has_node(fact_id) -> bool`

Check whether a fact has any edges in the graph. A fact only appears as a node if it participates in at least one edge.

## Edge lifecycle

Edges follow the same soft-delete pattern as facts:

- **Active**: `t_expired IS NULL` -- the edge is in the in-memory graph and participates in queries.
- **Expired**: `t_expired IS NOT NULL` -- the edge is removed from the in-memory graph but remains in SQLite for audit.

When a fact is expired (by forgetting or conflict resolution), all edges involving that fact are also expired. The in-memory graph is updated to reflect this within the same write lock.

On engine reopen, `MemoryGraph::load_from_db` rebuilds the in-memory graph from only the active edges. Expired edges are not loaded.

## Graph-aware forgetting

The forgetting system uses graph connectivity as one of four signals for computing a fact's importance:

```
importance = recency * decay
           + frequency * log(access_count + 1)
           + graph_degree * log(edges + 1)
           + base * fact.importance
```

High-degree facts (many connections) are more resistant to pruning. This prevents the forgetting system from removing facts that serve as important connectors in the knowledge graph.
