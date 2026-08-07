# ADR-0003 — Stack: Rust, Multi-Crate Workspace, Explicit MSRV

> Status: **accepted**

## Context

kern needs: a distributable static binary with no runtime, zero mandatory
GPU, fine-grained memory control (never loading a whole file when streaming
works), and concurrency that doesn't block the async runtime.

## Decision

- **Language**: Rust, multi-crate workspace:
  ```
  kern-ingest/     kern-vector/     kern-ontology/
  kern-model/      kern-mcp/        kern-cli/
  ```
- **Versioning**: Rust has no "LTS" — an explicit MSRV (Minimum Supported
  Rust Version), pinned via `rust-toolchain.toml`, never the loose `stable`
  channel.
  **Real finding**: the initial MSRV (1.75.0, "deliberately conservative")
  didn't survive the first non-trivial real dependency. `lancedb`
  (kern-vector) pulls in `arrow`/`datafusion`, whose transitive crate tree
  (`icu_*`, `time`) already required a recent toolchain — 1.75 didn't
  resolve, 1.85 didn't resolve, only the then-current stable (1.97.1)
  resolved. **Conservative, in practice, doesn't mean "the lowest number
  that compiles today"** — it means a pinned, explicit number (never the
  `stable` channel drifting on its own), even if that number ends up high
  because a real project dependency demands it. See the full history in
  `rust-toolchain.toml`, and a related non-MSRV finding: an `arrow-arith`/
  `chrono` build failure that looked like an ecosystem bug but was actually
  a `lancedb` version pinned to a stale, outdated release by mistake — fixed
  by pinning the real current version instead.
- **Async runtime**: Tokio. No synchronous/CPU-bound work runs directly on
  the event loop — always via `spawn_blocking`.
- **Memory safety**: ownership/borrowing idioms before cloning; `Arc`/`Cow`
  preferred over `.clone()` when data is shared.
- **`unsafe`/FFI**: avoided entirely — this is why inference runs via the
  `llama-server` subprocess rather than FFI bindings embedded in-process.

## Consequences

### Positive
- Single binary, zero runtime required (`curl | install | run`).
- No GC/interpreter cost — fits the CPU-only target.
- A pinned MSRV reduces the risk of breakage in corporate environments running an older Rust.

### Negative
- Less mature ORM/library ecosystem than Python/Java — mitigated by avoiding an ORM on purpose (see ADR-0004).

### Neutral
- A multi-crate workspace means each domain bounded context maps roughly 1:1 to a crate — see ADR-0001.
