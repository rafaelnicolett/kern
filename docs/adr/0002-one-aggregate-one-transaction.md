# ADR-0002 — Tactical DDD: One Aggregate, One Transaction

> Status: **accepted**

## Context

Domain modeling identified 5 aggregates across 2 bounded contexts (ontology,
agent surface). Implementation needs a clear consistency rule for where one
transaction's boundary ends.

## Decision

### Core rule

**One database transaction modifies at most ONE aggregate.** Cross-aggregate
consistency is eventual, guaranteed by invariants that never depend on a
just-deleted reference (kern never removes types in v0 — only adds or
promotes them, see Aggregate 1 below).

### Aggregates and their roots

| Aggregate root | Bounded context | Core invariant |
|---|---|---|
| `TypeRegistry` | Ontology | Unique names; promotion to canonical only after 3 independent hits; types are never removed in v0 |
| `InstanceGraph` (root: `Entity`) | Ontology | `first_seen_file` is immutable; every `Relation` carries a mandatory `evidence_chunk_id` |
| `FrontmatterProfile` | Ontology | Keyed by (folder scope, fingerprint) — scope is the immediate directory, not recursive |
| `KernProcess` | Agent surface | Strictly sequential transitions `Starting → CatchUpScan → Ready → Draining → Stopped`, no going back |
| `Project` | Agent surface | Its own `TypeRegistry` + `InstanceGraph`, never shared across projects |

### Cross-aggregate consistency

`InstanceGraph` references a `type_id` from `TypeRegistry` without checking
existence on every read — safe because `TypeRegistry` never removes types in
v0 (the invariant this simplification relies on; if a future version
introduces type removal, this ADR needs revisiting).

## Consequences

### Positive
- One repository per aggregate root (`TypeRepository`, `InstanceRepository`) —
  no repository for a subordinate entity (`Relation` never has its own
  repository, always accessed via `Entity`/`InstanceGraph`).
- Characterization tests can isolate one aggregate at a time.

### Negative
- Promoting a type to canonical and creating an instance of that type can
  happen "almost simultaneously" with no cross-aggregate lock — accepted
  because the worst case is an instance referencing a still-`candidate` type
  (not an error, it just means that instance doesn't count toward that
  specific file's promotion threshold).

### Neutral
- SQLite (one transaction per aggregate) and LanceDB (chunks, out of scope
  for this rule — not a DDD aggregate, it's ingestion/indexing storage) are
  physically separate stores — there is no distributed transaction between them.
