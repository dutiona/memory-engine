# ADR-0007: Hierarchical Scope Tree for Multi-Tenant Memory

**Status:** Accepted
**Date:** 2026-03-10

## Context

The agent operates across multiple projects, users, and contexts. Facts about "project A" should not pollute queries about "project B," yet some facts (user preferences, global knowledge) should be accessible everywhere.

Two approaches were considered:

1. **Separate databases** -- One SQLite file per context. Simple isolation, but prevents cross-context queries and multiplies operational complexity (migrations, backups, connection management).
2. **Single database with scoping** -- One database, with a scope identifier on every entity. Requires query-level filtering but enables cross-context queries and single-file operations.

The single-database approach was chosen. The design question then became: flat scopes (simple string tags) or hierarchical scopes (tree structure enabling inheritance).

Flat scopes fail when context nests naturally: a user has projects, projects have sessions. A query in "user:michael/project:demo/session:3" should see facts from that session, the project, and the user level. Flat scopes require the consumer to enumerate all relevant scope IDs manually.

Design rationale was developed in GitHub Issue #20.

## Decision

Hierarchical scope tree with slash-separated consumer-facing paths. The engine maintains an in-memory `ScopeTree` (backed by `RwLock` for concurrent access) loaded from a `scopes` SQLite table.

**Data model:**

`ScopeNode` has: `id` (integer), `parent_id` (nullable, root has None), `label` (string), `depth` (integer).

Consumer-facing paths use slash separation: `"user:michael/project:demo/session:3"`. The engine resolves paths to integer `scope_id` values internally.

`scope_id` is present on all entities: `Fact`, `Edge`, `Event`, `Summary`.

**Query semantics via `ScopeQuery` enum:**

| Variant     | Behavior                                                         |
| ----------- | ---------------------------------------------------------------- |
| `Exact`     | Facts at exactly the specified scope path.                       |
| `Subtree`   | Facts at the specified scope and all descendant scopes.          |
| `Ancestors` | Facts at the specified scope and all ancestor scopes up to root. |
| `Inherited` | Union of Ancestors and Subtree -- full inherited context.        |

`Inherited` is the typical query mode: it gives the agent everything visible from its current scope, including inherited knowledge from parent scopes and specific knowledge from child scopes.

The `ScopeTree` is loaded from SQLite at engine initialization and cached in memory behind an `RwLock`. New scopes are created lazily when a consumer references a path that does not yet exist.

## Consequences

### Positive

- Single database for all contexts. One file to back up, one migration path, one connection pool.
- Flexible querying. `Inherited` mode gives natural scope resolution without consumer-side logic.
- Lazy scope creation means consumers do not need to pre-declare scope hierarchies. Paths are created on first use.
- Integer `scope_id` foreign keys are compact and indexed, adding minimal overhead to queries.

### Negative

- Every query gains a `scope_id` filter predicate. This adds complexity to every SQL query in the store layer.
- The `ScopeTree` is an in-memory cache that must stay synchronized with SQLite. Concurrent scope creation requires locking.
- Consumers must understand the scoping model to use it effectively. Incorrect scope paths lead to invisible facts (scoped too narrowly) or leaking facts (scoped too broadly).

### Mitigations

- Default scope (root, id=1) is created at database initialization. Consumers who do not need scoping can ignore it entirely -- all entities default to root scope.
- The `RwLock`-based `ScopeTree` allows concurrent reads (the common case) with exclusive writes only on scope creation.
- Phase 3 SQL-level filters apply `scope_id` filtering automatically based on the `ScopeQuery`, so consumers interact with paths, not raw IDs.
