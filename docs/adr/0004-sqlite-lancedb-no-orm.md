# ADR-0004 — Persistence: Embedded SQLite + LanceDB, No ORM

> Status: **accepted**

## Context

SQLite (type registry + instance graph) and LanceDB (chunks) are the two
stores, deliberately not a dedicated graph database — at the scale of a
Spec-Driven Development corpus (hundreds to low thousands of files), plain
relational SQL is enough.

## Decision

- **SQLite** (`kern-ontology`): a direct driver (`rusqlite`) behind a
  `Repository` trait per aggregate (`TypeRepository`, `InstanceRepository`)
  — no ORM (Diesel/SeaORM). A small, stable schema (5 tables) doesn't
  justify the overhead of automatic migrations and object-relational mapping
  from a full ORM.
- **LanceDB** (`kern-vector`): a native embedded Rust crate, no external
  server, no FFI — accessed via LanceDB's own API, not SQL.
- **Per-project isolation**: every kern project has its own SQLite file and
  LanceDB directory — never a shared store with logical partitioning (no
  RLS/multi-tenancy inside a single store).

## Consequences

### Positive
- Full control over the query — relevant because the ontology engine does
  embedding-distance comparisons, simple joins, and count aggregation,
  nothing that justifies an ORM abstraction.
- No auto-generated migrations drifting from the real schema — the schema is
  versioned by hand, which fits the small number of tables.
- Isolation by file/directory is simpler to reason about (and safer) than RLS inside a shared store.

### Negative
- Writing SQL by hand has more friction than an ORM — mitigated by keeping
  the repository pattern consistent and idiomatic across both `Sqlite*Repository` implementations.

### Neutral
- `relations.evidence_chunk_id` references `chunks` in LanceDB — a
  cross-store reference, validated in application code, not by a database
  foreign key (physically separate stores, see ADR-0002).
