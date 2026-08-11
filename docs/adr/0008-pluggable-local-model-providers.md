# ADR-0008 — Pluggable Local Model Providers

> Status: **accepted**

## Context

Before this decision, kern hardcoded exactly two model backends in a fixed
order, entirely inside `kern-cli`'s composition root (`cmd_serve`): probe
Ollama on `:11434`, and if it responds use `all-minilm` for embeddings and
`llama3.2` for extraction/judging (both hardcoded model-name strings);
otherwise fall back to the bundled `llama-server` subprocess, which only
ever implemented embedding. There was no way for a user to choose a
different model, a different local engine, or configure anything about
this at all.

This surfaced as a real problem, not a hypothetical one: while verifying
the bundled embedding tarball end to end, a real oversized Markdown
section blew past `llama-server`'s context window and returned `"input
(1202 tokens) is too large to process... current batch size: 512"`.
Investigating it further surfaced that `EmbeddingProvider`/`ExtractionProvider`
had no self-description at all (no context window, no dimension, no
identity) — every assumption about what "the" model could handle was
implicit and untested. Fixing the chunking bug properly meant those
assumptions had to become real, queryable facts instead.

**Local-only remains a hard constraint** — kern's README principle #1 is
"Actually local-first — no data ever leaves the machine." This decision is
about selecting among kern's own built-in local engines (Ollama, bundled
llama.cpp, and future local-only engines), not a remote/cloud provider
surface.

## Decision

- `EmbeddingProvider` gains `capabilities() -> EmbeddingCapabilities { model_id,
  embedding_dim, max_input_tokens }` — always a real round-trip against the
  actual running backend, never a static claim about "the" model kern
  assumes is active. Empirically verified while implementing this: Ollama's
  `/api/show` reports a model's *architectural* max context separately from
  the *runtime* limit it actually enforces (`all-minilm` reports 512 in
  `model_info["bert.context_length"]`, but Ollama actually serves it with
  `num_ctx` 256) — `capabilities()` reads the runtime figure first,
  falling back to the architectural one only when the runtime figure is
  absent.
- `ExtractionProvider` gains a sync `model_id()` for display purposes
  (`kern status`, `kern config show`).
- The `TokenCounter` port (`kern-ingest`) makes the token-*counting*
  mechanism itself pluggable, not just the budget number — a
  `HeuristicTokenCounter` (chars/4) ships as the only implementation
  today, with the trait boundary left open for a real per-model tokenizer
  later, without the chunker's control flow needing to change when that
  lands.
- The previously-unused `ModelBackend` enum (a closed, 2-variant dispatch
  mechanism nothing actually used — `cmd_serve` already built
  `Arc<dyn EmbeddingProvider>` directly, bypassing it) is removed in favor
  of `EmbeddingProviderSelection`/`ExtractionProviderKind` + factory
  functions (`build_embedding_provider`, `build_extraction_provider`) in
  `kern-model`. These are open-ended (new local engines are a new enum
  variant + factory arm, not a `cmd_serve` control-flow change) and
  config-driven: `kern-cli`'s composition root resolves a provider from
  `.kern/config.toml` (see ADR-0009) instead of a hardcoded
  probe-then-fallback.
- `cmd_serve`'s runtime auto-fallback is replaced entirely: provider
  selection now happens once, at `kern project create` (a guided wizard in
  a real terminal, or explicit `--embedding-provider`/`--embedding-model`
  flags otherwise) or via `kern config set-embedding`/`set-extraction`
  later — never re-derived silently on every `serve` invocation. Once
  pinned, an unreachable configured provider is a hard
  `AGENT_SURFACE.PROVIDER_UNAVAILABLE` error, not an automatic swap to a
  different (and possibly differently-dimensioned) backend.
- The chunker (`kern-ingest`) gains a `BudgetAwareMarkdownChunker`
  decorator around the existing `StructuralMarkdownChunker`, sub-splitting
  any chunk that exceeds the active provider's real `max_input_tokens`
  (paragraph boundaries, then sentences, then a hard UTF-8-safe cut as the
  last resort), while never splitting a code block or table even under
  budget pressure. `kern-ingest` takes the budget as a plain `usize` from
  the composition root — it gains no dependency on `kern-model`.

## Consequences

### Positive
- New local backends are a matter of adding one enum variant + factory
  arm, not touching `cmd_serve`'s control flow.
- The chunking bug this decision traces back to is closed for real,
  independent of which provider is configured: the budget is always
  sourced from the active provider's own reported capability, not a
  guessed constant.
- `kern status`/`kern config show` can report what's actually configured
  and active, not leave it implicit.

### Negative
- Once pinned, an unreachable provider is a hard error rather than an
  automatic fallback — a deliberate trade against silently swapping to a
  differently-dimensioned backend (see ADR-0009's dimension-pinning
  guarantee, which this trade directly protects).
- Provider setup is no longer free/implicit — `kern project create` now
  always resolves (interactively or via flags) a real, working embedding
  provider before a project is usable, rather than deferring that
  entirely to `serve`.

### Neutral
- "Pluggable" means selecting among kern's own built-in local engines via
  config — not a dynamic `.so`/plugin-loading system. Adding a genuinely
  new backend still means a kern code change and release.
