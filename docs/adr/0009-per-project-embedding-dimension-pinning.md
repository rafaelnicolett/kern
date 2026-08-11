# ADR-0009 — Per-Project Embedding Dimension Pinning

> Status: **accepted**

## Context

`kern-vector`'s embedding dimension was a single global constant
(`EMBEDDING_DIM: i32 = 384`), feeding LanceDB's Arrow `FixedSizeList`
schema directly. Arrow fixes a `FixedSizeList`'s width at table-creation
time — it cannot change after data exists. `cmd_project_create` created
that table immediately on `project create`, before any model was ever
chosen, implicitly pinning every project to 384 dimensions regardless of
what provider would eventually be configured (see ADR-0008). Once
pluggable providers (ADR-0008) made different real dimensions possible —
Ollama's `all-minilm` is 384, but nothing stopped a future provider from
reporting something else — this implicit, premature pinning became a real
correctness gap, not just a hardcoded value.

## Decision

- `LanceVectorStore::open` now only connects to the project's vector
  directory — it does **not** create the table. A new `ensure_table(dim)`
  is called explicitly, once the real dimension is known from a resolved
  provider's `capabilities()` (ADR-0008), and is idempotent: a no-op if a
  table already exists with a matching dimension, a clear
  `AGENT_SURFACE.EMBEDDING_DIMENSION_MISMATCH` error if it exists with a
  *different* one, and creates it if absent. It never silently
  truncates or rebuilds an existing index.
- The dimension is a per-project, persisted fact in
  `<project>/.kern/config.toml` (`embedding.dimension`), written once from
  a real `capabilities()` call at `project create` or `kern config
  set-embedding` time — never hand-edited.
- **Migration safety for pre-existing projects**: the mismatch check reads
  the table's *actual* Arrow schema via a new `existing_dimension()`
  accessor, not just the persisted config. A project indexed before this
  decision existed has real 384-dim data and no config file at all —
  resolving to a provider that reports the same real 384 dimensions
  succeeds and backfills the config; resolving to a different dimension
  fails clearly, exactly as it would for a project that already has a
  config file. "No config" is never treated as "blank slate" when a real
  table already exists.

## Consequences

### Positive
- Supports embedding models of different dimensionality across projects,
  a direct prerequisite for ADR-0008's pluggable providers actually being
  usable in practice.
- Never silently corrupts an existing index — switching a project's
  embedding model always surfaces as an explicit, clear error, with the
  existing table left completely untouched.
- Correctly handles projects that predate this mechanism, without a
  separate migration step or tool.

### Negative
- Switching a project's embedding model requires an explicit re-index
  today (delete `.kern/vectors`, reconfigure, re-run `serve`) — a
  first-class `kern project reindex` convenience command was considered
  but deferred as a future improvement, not required for this decision to
  be correct.
- `project create` and "first successful embed" are conceptually two
  separable moments now (dimension isn't known until a provider is
  actually resolved), rather than one implicit step.

### Neutral
- `ensure_table`/`existing_dimension` are inherent methods on the concrete
  `LanceVectorStore`, not part of the `VectorStore` trait — schema
  creation is a composition-root concern; `kern-mcp` and `kern-ontology`
  only ever search/upsert through the trait, unaware of dimension pinning
  entirely.
