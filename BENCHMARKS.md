# Benchmarks

> **Status**: early, small-corpus numbers from the one real example corpus
> in this repo — not a claim about behavior at scale. Every number here was
> measured, not estimated; the methodology to reproduce each one is next to
> it. If a number looks too good (or too small) to be meaningful, that's
> usually because the corpus is small — see the caveat on each section.

## Methodology (applies to every section below)

- **Corpus**: [`examples/sample-specs/`](examples/sample-specs) — 5 Markdown
  files (~80 lines total), 4 with frontmatter (`SPEC-001`, `PLAN-001`,
  `TASK-001`, `TASK-002`) and 1 free-form (`design-notes.md`). This is a
  *toy* corpus, chosen for the README's worked examples — not a
  representative real-world Spec-Driven Development project. Numbers here
  will look different (almost certainly worse on latency, likely similar
  on relative fallback-rate behavior) on a corpus with hundreds of files.
- **Hardware**: Apple M4 Max (arm64), 32 GB RAM, local SSD.
- **Model backend**: Ollama (`all-minilm` for embeddings, `llama3.2` for
  extraction/judging/frontmatter-schema interpretation) — the opportunistic
  path, not the bundled `llama-server` path.
- **Command**: `kern project create demo --path examples/sample-specs
  --embedding-provider ollama --embedding-model all-minilm
  --extraction-provider ollama --extraction-model llama3.2` then `kern
  serve --project demo`, driven over real MCP stdio (not a mock) — see the
  transcript driver referenced in the README's
  [Examples](README.md#examples) section. Provider setup (`project
  create`) is a one-time step per project, timed separately from
  everything below — every number here is from `kern serve` onward, not
  including it.
- Every run starts from a cold project (`.kern/` deleted first) — no
  benchmark here measures a warm/incremental re-index.

---

## 1. Fallback rate — the North Star metric

The whole thesis behind kern's ontology is that most candidates should be
classifiable *without* an LLM call (`judge()`) — either the frontmatter
already says what they are (deterministic, zero model cost beyond the
one-time schema interpretation), or the embedding distance to the nearest
known type is unambiguous enough to merge or create a new type without
asking. `judge()` — the fallback path — is reserved for the genuinely
ambiguous middle.

**Real measurement, this corpus**: 34 prose-extracted candidates processed
(from the free-form file plus the non-frontmatter body text of the
frontmatter files), **0 fell into the ambiguous zone** — a 0% fallback
rate. The 4 frontmatter-driven entities and their `depends_on`/`implements`
relations bypassed candidate classification entirely (deterministic path),
so they aren't part of this count at all.

**Read this number carefully — it is not "kern has a 0% fallback rate":**
- The sample is tiny (34 candidates from 5 files). A 0% rate on a sample
  this size is not statistically meaningful; it just means no candidate in
  *this specific small corpus* happened to land between the low/high
  distance thresholds (`low_distance_max: 0.15`, `high_distance_min: 0.35`
  — themselves [documented placeholders, not tuned](docs/adr) values).
- This measures the **entity-extraction** fallback rate only. Relation
  extraction from prose doesn't exist yet (only frontmatter produces real
  relations today — see the repo's sprint notes for the current gap list),
  so there's no relation-side fallback rate to report.
- A real measurement of this metric that means something requires a real,
  larger, messier corpus — dogfooding against an actual project's specs is
  the next real step here, not a bigger synthetic example.

**How to reproduce**: run `kern serve` against any project and grep its
stderr for `kern.ontology.fallback_rate` — every line carries the running
`total`, `fallback_total`, and `rate` as of that candidate.

---

## 2. Memory and CPU

Measured with `/usr/bin/time -l` (macOS) wrapping a full `kern serve`
session: startup, catch-up indexing of the whole corpus, and one real
`query_ontological` call, until the client closes the connection.

| Metric | Value |
|---|---|
| Peak RSS (maximum resident set size) | 63.6 MB |
| Peak memory footprint (macOS-specific, generally lower than RSS) | 17.4 MB |
| User CPU time | 0.40 s |
| System CPU time | 0.18 s |
| Wall-clock indexing time (5 files, 15 chunks) | 18.4 s |

**Caveats, explicit:**
- This is one combined number across startup + indexing + one query, not a
  clean breakdown of "idle" vs. "indexing" vs. "with the judge model
  loaded" — getting a reliable *phase-by-phase* memory profile needs
  instrumentation this v0 doesn't have yet (sampling an external process's
  RSS at a precise phase boundary via shell tooling proved unreliable
  enough in practice that reporting a fabricated-looking clean breakdown
  felt less honest than reporting the one number actually measured
  end-to-end). Flagging this as a known methodology gap rather than
  presenting three numbers that would imply more precision than what was
  really measured.
- CPU time is tiny (0.58 s total) relative to the 18.4 s wall-clock
  indexing time — almost all of that wall time is kern waiting on Ollama's
  HTTP responses (embedding + extraction calls), not kern's own compute.
  On a corpus this small, Ollama's per-call overhead dominates; this ratio
  will shift with a larger corpus and is worth re-measuring then.
- Memory is dominated by the LanceDB/Arrow/DataFusion dependency tree
  loaded into the process, not by corpus size at this scale — expect this
  floor to matter more than corpus-driven growth until the corpus is much
  larger than 5 files.

---

## 3. Time to useful response

**Not measured yet, on purpose.** The v0 acceptance target (~2 minutes,
dominated by the one-time model download) requires the automatic
model-weight download this v0 doesn't implement yet (see the README's
[Model backend](README.md#model-backend) section) — today, resolving a
model backend is a manual step (`ollama pull ...` or placing a `.gguf` by
hand) that happens once, outside of anything `kern` itself times or
controls. Reporting a number here before that piece exists would either
measure something misleading (excluding the dominant real-world cost) or
require fabricating a download-time estimate. This section gets filled in
once automatic download lands.

---

## Comparison with other approaches

No comparison against Semantica (or any other tool) is included here.
Doing that honestly requires declaring exactly which configuration of the
other tool is being compared — Semantica in particular has a lightweight
SQLite/in-memory path in addition to its Neo4j/Docker path, and a
comparison that doesn't say which one it's measuring against is the kind
of thing any technical reader would (correctly) discount as a cheap shot.
That declaration requires research this session didn't do — adding it here
without verified numbers would mean fabricating them. A real comparison,
if one gets added later, will name the exact configuration and command
line on both sides.
