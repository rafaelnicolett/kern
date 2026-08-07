# ADR-0001 — Reference Architecture: Hexagonal (Ports & Adapters)

> Status: **accepted**

## Context

kern needs to swap inference backends (embedded llama.cpp ↔ Ollama) and
persistence drivers without rewriting domain logic (the ontology engine,
ingestion). The design is traits-first: `EmbeddingProvider`/`ExtractionProvider`
are the reference interfaces for any pluggable component.

## Decision

Hexagonal (Ports & Adapters), applied per crate:

| Layer | Where it lives | Example |
|---|---|---|
| Domain | `kern-ontology` (aggregates), shared types in each crate | `TypeRegistry`, `InstanceGraph`, the `kern-cli` process state machine |
| Ports (traits) | Defined in the crate that consumes them | `EmbeddingProvider`, `ExtractionProvider` (kern-model), `TypeRepository`/`InstanceRepository` (kern-ontology) |
| Adapters/Out | Concrete implementations | `LlamaCppRuntime`/`OllamaClient` (kern-model), `SqliteTypeRepository` (kern-ontology) |
| Adapters/In | Entry points | MCP tools (kern-mcp), CLI commands (kern-cli), file watcher (kern-ingest) |

**Cross-crate invariant**: no domain crate (`kern-ontology`, `kern-ingest`)
depends directly on a concrete model or database implementation — always
through a trait. This is what lets `llama-server` be swapped for Ollama, or
SQLite for another driver, without touching business logic.

## Consequences

### Positive
- Testable without real I/O (mock `EmbeddingProvider`/`Repository` implementations in tests).
- Swapping backends (opportunistic Ollama) requires no change to the ontology engine.
- A trait as the default for any pluggable component is a project-wide convention, not just this ADR.

### Negative
- Overhead of defining a trait + implementation for components that today only
  have one real backend (accepted — it's the price of keeping the door open
  for Ollama and future backends without rework).

### Neutral
- Each crate in the workspace maps cleanly to a hexagonal boundary — no
  hexagonal layer crosses multiple crates.
