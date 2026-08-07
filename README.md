<p align="center">
  <img src="docs/banner.png" alt="kern" width="640">
</p>

<p align="center">
<a href="https://github.com/rafaelnicolett/kern/actions/workflows/ci.yml"><img src="https://github.com/rafaelnicolett/kern/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
<a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License"></a>
<a href="rust-toolchain.toml"><img src="https://img.shields.io/badge/rust-1.97.1%2B-orange.svg" alt="Rust"></a>
</p>

**kern** is a local-first RAG engine built for **Spec-Driven Development** —
the Markdown that spec-first workflows and AI coding agents already
produce, full of frontmatter like `id`, `kind`, `depends_on`, `implements`.
That frontmatter already *is* a relationship graph — kern parses it
**deterministically** into real entities and typed relations, no LLM
involved for content that's already structured. A local model only gets
called for free-form prose that has no frontmatter to parse, or for the
rare candidate ambiguous enough to need a judgment call against what the
ontology already knows.

Point kern at that folder, and it keeps a local vector index (an embedded
LanceDB) and that lightweight ontology — entities, typed relations, cited
evidence — in sync as your specs, plans, and tasks change. No external
database, no GPU, no full-corpus rebuild on every edit.

> **Status: pre-v0, under active construction.** The CLI, the MCP contract
> and the ontology schema can still change without notice before v1.
> Issues and feedback are very welcome.

|  | kern | plain vector search | full GraphRAG (Neo4j/Qdrant) |
|---|---|---|---|
| External infra | none | vector database | graph database + vector database |
| GPU | not required | not required | usually recommended |
| Update cost | file diff | file diff | often a full rebuild |
| Relational queries | yes, routed by type | no | yes |

Plain vector search alone still loses relational understanding as a corpus
grows — "what depends on X?" isn't a question embeddings answer well on
their own, which is exactly the gap the frontmatter-driven ontology closes
without needing GraphRAG's external infrastructure.

## Principles

1. **Actually local-first** — no data ever leaves the machine.
2. **No GPU required** — CPU is the reference case, not the exception.
3. **Incremental by default** — reprocessing is triggered by a file diff,
   never a full corpus rebuild.
4. **Agents are the primary consumer** — the main interface is MCP over
   stdio; the CLI exists for operation and debugging, not as the product
   itself.
5. **Static binary, no runtime dependency** — download it, run it.

## How it works

```mermaid
flowchart LR
    MD(["📁 Markdown folder"]) -- watch + diff --> ING["kern-ingest<br/>chunking"]
    ING -- frontmatter --> FM["deterministic parse<br/>(no LLM)"]
    ING -- free prose --> LLM["kern-model<br/>extract / judge"]
    ING -- every chunk --> EMB["kern-model<br/>embed"]
    EMB -- embeddings --> VEC[("kern-vector<br/>LanceDB, embedded")]
    FM --> ONT["kern-ontology<br/>type registry + instance graph"]
    LLM --> ONT
    VEC --> MCP["kern-mcp<br/>MCP server"]
    ONT --> MCP
    MCP -- stdio --> AGENT(["Any MCP host<br/>Claude Code, Claude Desktop, ..."])
```

A single binary watches the folder, chunks and embeds new or changed
content, and keeps the vector index up to date. Frontmatter fields that map
to a known concept (`id`, `kind`, `depends_on`, `implements`, ...) become a
real entity and real relations immediately — no distance computation, no
model call beyond the one-time, cached interpretation of a new frontmatter
shape. Free-form prose (or the ambiguous case even the ontology's own
distance check can't resolve) is what asks a local model to decide: merge
into an existing type, promote to a new one, or discard. See
[`docs/adr`](docs/adr) for the architecture decision history.

### Model backend

kern needs a local model for embeddings (and, lazily, for ontology
extraction/judging). It resolves one automatically, and never falls back
silently — if nothing usable is found, `kern serve` fails with a clear
error instead of guessing:

- **Ollama, if available** — if a daemon is already responding on
  `:11434`, kern uses it opportunistically. This is the easiest path
  today: `ollama pull all-minilm` for embeddings, and optionally
  `ollama pull llama3.2` for ontology extraction/judging.
- **Bundled engine, otherwise** — release binaries embed a
  [llama.cpp](https://github.com/ggml-org/llama.cpp) `llama-server`,
  extracted to `~/.cache/kern/bin/` on first use, no separate install. The
  embedding *weights* still have to come from somewhere:
  - the **[`kern-<target>-with-embedding-model`](#zero-setup-embeddings-no-ollama)
    release tarball** ships a real embedding model
    ([`all-MiniLM-L6-v2`](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2),
    F16 GGUF, ~46 MB, Apache-2.0 — see
    [NOTICE-THIRD-PARTY](NOTICE-THIRD-PARTY)) as a file next to the binary,
    adopted into `~/.cache/kern/models/` the first time it's needed — no
    manual step.
  - the plain `kern-<target>` tarball doesn't include a model file — either
    run Ollama, or place a compatible embedding `.gguf` under
    `~/.cache/kern/models/` by hand.

This only closes the gap for **embeddings**. Ontology extraction/judging
has no bundled backend yet — that path still needs Ollama regardless of
which tarball you use. There is also still no *runtime* automatic download
from Hugging Face — the with-embedding-model tarball is a build-time
bundling choice, not download-on-demand.

## Installing

### From a release (recommended)

Pre-built binaries are published on the
[Releases page](https://github.com/rafaelnicolett/kern/releases) for
macOS (Apple Silicon and Intel) and Linux (x86_64):

```bash
# macOS, Apple Silicon
curl -L https://github.com/rafaelnicolett/kern/releases/latest/download/kern-aarch64-apple-darwin.tar.gz | tar xz

# macOS, Intel
curl -L https://github.com/rafaelnicolett/kern/releases/latest/download/kern-x86_64-apple-darwin.tar.gz | tar xz

# Linux, x86_64
curl -L https://github.com/rafaelnicolett/kern/releases/latest/download/kern-x86_64-unknown-linux-gnu.tar.gz | tar xz
```

Then move the extracted `kern` binary onto your `PATH`, e.g.:

```bash
mv kern/kern /usr/local/bin/
```

> Linux arm64 and Windows aren't built yet — see [Contributing](#contributing)
> if you'd like to help close that gap.

### Zero-setup embeddings (no Ollama)

`kern-<target>-with-embedding-model.tar.gz` bundles a real embedding model
alongside the binary — no Ollama, no manual `.gguf` placement:

```bash
curl -L https://github.com/rafaelnicolett/kern/releases/latest/download/kern-aarch64-apple-darwin-with-embedding-model.tar.gz | tar xz
```

(same pattern for `x86_64-apple-darwin` and `x86_64-unknown-linux-gnu`)

**Run it once from the extracted folder before moving the binary anywhere.**
kern looks for a `.gguf` sitting next to its own executable the first time
it needs the embedded backend, and adopts it into `~/.cache/kern/models/`:

```bash
cd kern-aarch64-apple-darwin-with-embedding-model
./kern project create acme --path ./docs/acme
./kern serve --project acme
```

After that first run, the model is cached and the binary can be moved onto
`PATH` like the plain tarball above (`mv kern /usr/local/bin/`) — a
symlink created *before* that first run does **not** work as a shortcut
around this (verified: `current_exe()` returns the invoked path, not the
symlink's target, so a pre-placed symlink can't see the sidecar file
either). Moving the binary away before the first run strands the model
file behind and `kern serve` fails with a clear
`AGENT_SURFACE.MODEL_MISSING_FROM_CACHE` error rather than a confusing one.

This still doesn't cover ontology extraction/judging — that needs Ollama
either way (see [Model backend](#model-backend)).

### From source

Requires the Rust toolchain pinned in
[`rust-toolchain.toml`](rust-toolchain.toml) — `rustup` installs it
automatically:

```bash
git clone https://github.com/rafaelnicolett/kern.git
cd kern
cargo build --release -p kern-cli
# binary at target/release/kern
```

## Quick start

```bash
# 1. Create a project — an isolated index + ontology over one folder
kern project create acme --path ./docs/acme

# 2. Serve it: catches up on any backlog, then exposes MCP over stdio
kern serve --project acme

# 3. From another terminal, check on it
kern status --project acme
```

`kern serve` blocks, speaking MCP JSON-RPC over stdio — it's meant to be
launched by an MCP host, not run interactively in a terminal you're typing
into. See below for wiring it up.

Before the first `serve`, resolve a model backend per
[Model backend](#model-backend): `ollama pull all-minilm` (and optionally
`llama3.2`), the
[`kern-<target>-with-embedding-model`](#zero-setup-embeddings-no-ollama)
release tarball (embeddings only, zero manual steps), or a manually cached
`.gguf` — `kern serve` has nothing to fall back on otherwise, and fails
with a clear error rather than hanging.

**v0 target**: install-to-useful-`query_ontological`-response should take
about 2 minutes. For the **embedding** path, that's met today by the
with-embedding-model tarball (see
[BENCHMARKS.md](BENCHMARKS.md#3-time-to-useful-response) for a real
measurement) — no manual `ollama pull` or `.gguf` placement needed.
**Ontology extraction/judging** still needs Ollama regardless of tarball;
that half of the v0 target isn't met yet.

### Connecting an MCP host

**Claude Code:**

```bash
claude mcp add kern -- kern serve --project acme
```

**Claude Desktop** (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "kern": {
      "command": "kern",
      "args": ["serve", "--project", "acme"]
    }
  }
}
```

Any other MCP-compatible host works the same way: point it at the `kern`
binary with `serve --project <name>` as arguments.

## MCP tools

| Tool | What it does |
|---|---|
| `search_hybrid` | Vector similarity search over indexed chunks, boosted by keyword match |
| `query_by_concept` | Look up an entity by name and its direct relations |
| `get_related_entities` | Walk the instance graph outward from an entity, up to a given depth |
| `get_ontology_schema` | List the current entity and relation type vocabulary |
| `explain_relation` | Return the evidence path behind a specific relation |
| `query_ontological` | The main entry point — routes semantically between the vector index and the ontology, whichever answers the question better |

See
[`docs/architecture/mcp-tool-contract.md`](docs/architecture/mcp-tool-contract.md)
for the full contract.

## Examples

[`examples/sample-specs/`](examples/sample-specs) is a small, real
Spec-Driven Development corpus: a spec, a plan, and two tasks with
frontmatter (`id`, `kind`, `status`, `depends_on`, `implements`), plus one
free-form file with no frontmatter at all, to exercise the prose fallback
path. Try it yourself:

```bash
kern project create demo --path examples/sample-specs
python3 examples/query_ontological.py target/release/kern demo \
  "what is the depends_on relation for TASK-002?"
```

The transcripts below are copied verbatim from that command against a real
MCP session (`tools/call` over stdio, not simulated output) —
[`examples/query_ontological.py`](examples/query_ontological.py) is the
exact driver script, real and runnable, not a doc-only snippet. Ollama was
running locally (`all-minilm` for embeddings, `llama3.2` for extraction and
judging).

**A question that routes by relation type** — `TASK-002`'s frontmatter
(`depends_on: [TASK-001]`) was parsed deterministically into a real
`depends_on` relation; `query_ontological` finds it via graph traversal,
not vector search:

```
> query_ontological({"question": "what is the depends_on relation for TASK-002?"})

{
  "mode": "graph_traversal",
  "answer": "TASK-002 has 1 relation(s) of type 'depends_on'",
  "evidence": [
    {
      "chunk_id": "af6d9596-0faa-4c42-a2be-6f5e8d4da481",
      "excerpt": "see evidence chunk via search_hybrid"
    }
  ]
}
```

**A question with no relation-type match** — falls back to `search_hybrid`
over the free-form `design-notes.md` file, which has no frontmatter and
went through prose extraction instead:

```
> query_ontological({"question": "how does the export handle large row counts?"})

{
  "mode": "vector_fallback",
  "answer": "# Design notes: CSV export\n\nThis file has no frontmatter on purpose — it's the free-form counterpart to\nthe specs/plan/tasks in `.specify/specs/`, meant to exercise kern's prose\nfallback path rather than the deterministic frontmatter path.\n\nRow streaming for the export endpoint is implemented with a cursor-based\ndatabase query rather than `OFFSET`/`LIMIT` paging, since offset pagination\ndegrades badly past a few hundred thousand rows — exactly the range\nSPEC-001 asks this to handle. The dashboard's CSV button reuses the same\nfilter-serialization helper the dashboard's own data-fetching code already\nuses, so the exported rows always match what's on screen.\n",
  "evidence": [
    { "chunk_id": "79897d41-...", "excerpt": "# Design notes: CSV export\n\n..." },
    { "chunk_id": "3130dd70-...", "excerpt": "## Requirements\n\n- A logged-in user can export..." },
    { "chunk_id": "64bd4542-...", "excerpt": "# Implementation plan: CSV export\n\n..." }
  ]
}
```

**Getting a `graph_traversal` result reliably depends on how the question
is phrased**, and this is worth being honest about: `query_ontological`'s
semantic router embeds the question and compares it against each relation
type's (fairly generic, seeded) description — a question that echoes the
relation's name closely (`depends_on`) scores well above the routing
threshold; a more naturally-phrased one ("what does TASK-002 *depend on*?")
scored well under it in testing. This is a real, current limitation of the
v0 router, not fabricated behavior — improving it (richer canonical
descriptions, a better routing signal than raw cosine similarity over a
short template string) is open work.

## v0 scope

1. Markdown-aware ingestion, chunking, and local vector indexing (embedded
   LanceDB, embedding via a local subprocess).
2. An incremental ontology engine with three outcomes per candidate
   (merge / new type / judge), with a fallback-rate metric exposed via
   structured logs and traces (`tracing` crate — not yet wired to an
   OpenTelemetry exporter, see [ADR-0005](docs/adr/0005-observability-from-v0.md)).
3. MCP over stdio, with the six tools above.
4. A minimal CLI: `project create`, `serve`, `status`.

**Deliberately out of scope for v0**: a GUI/desktop app, a local HTTP API,
multi-process/multi-client coordination, and built-in document conversion
(PDF, DOCX, etc. always go through an external, configurable subprocess —
never a library compiled into kern).

## Workspace layout

```
kern-ingest/     file watcher + markdown-aware chunking
kern-vector/     wrapper around the embedded LanceDB vector store
kern-ontology/   type registry, relation vocabulary, incremental diff engine
kern-model/      EmbeddingProvider/ExtractionProvider traits, bundled llama-server subprocess, opportunistic Ollama adapter
kern-mcp/        MCP server (rmcp) exposing the agent-facing tools
kern-cli/        the `kern` binary: project create, serve, status
```

Hexagonal architecture (ports & adapters), traits-first — see [`docs/adr/`](docs/adr)
for the decision history.

## Benchmarks

Real, reproducible, small-corpus numbers (fallback rate, memory/CPU) live
in [`BENCHMARKS.md`](BENCHMARKS.md), with the exact methodology to
reproduce each one — no comparison against other tools is included until
that can be done with a declared, verified configuration on both sides.

## Contributing

Rust, MSRV pinned in [`rust-toolchain.toml`](rust-toolchain.toml).
`cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` and
`cargo audit` all run in CI on every push. Issues and PRs are welcome —
Linux arm64 and Windows support for the release build matrix would make a
great first contribution.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option — the Rust ecosystem default. The
`kern-<target>-with-embedding-model` release tarball additionally
redistributes a third-party embedding model — see
[NOTICE-THIRD-PARTY](NOTICE-THIRD-PARTY) for license and attribution.

---

<sub>Powered by [Helyx](https://helyx.build).</sub>
