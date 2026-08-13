# Benchmarks

> **Status**: early, small-corpus numbers from the two example corpora in
> this repo (`examples/sample-specs/`, `dogfood-corpus/`) — not a claim
> about behavior at scale. Every number here was measured, not estimated;
> the methodology to reproduce each one is next to it. If a number looks
> too good (or too small) to be meaningful, that's usually because the
> corpus is small — see the caveat on each section.

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
- Sections 2–3 use `examples/sample-specs/` (5 files, the same toy corpus
  the README's worked examples use). Sections 1 and 5 use
  [`dogfood-corpus/`](dogfood-corpus) instead — 66 files across 15
  features with real cross-feature `depends_on` chains, deliberately
  structurally repetitive (same SPEC/PLAN/TASK frontmatter shape 15 times
  over), because that repetition is exactly the condition both of those
  sections measure the effect of. Each section says which corpus it used.

---

## 1. Fallback rate — the North Star metric

The whole thesis behind kern's ontology is that most candidates should be
classifiable *without* an LLM call (`judge()`) — either the frontmatter
already says what they are (deterministic, zero model cost beyond the
one-time schema interpretation), or the embedding distance to the nearest
known type is unambiguous enough to merge or create a new type without
asking. `judge()` — the fallback path — is reserved for the genuinely
ambiguous middle.

**Real measurement, `dogfood-corpus/`**: 508 prose-extracted candidates
processed (from the corpus's 6 free-form files plus the non-frontmatter
body chunks of all 60 frontmatter files), **15 fell into the ambiguous
zone — a 2.9% fallback rate.** This supersedes an earlier measurement on
`examples/sample-specs/` (34 candidates, 0% fallback) that this file's own
prior text correctly flagged as too small a sample to mean anything — 508
is still not a large corpus, but it's the real number from the corpus this
repo actually ships, not a toy fixture, and it's over an order of
magnitude more candidates than the sample it replaces.

**Read this number carefully — it is still not "kern's true fallback
rate" for every corpus:**
- The 15/508 zone is specific to this corpus's vocabulary and the
  low/high distance thresholds (`low_distance_max: 0.15`,
  `high_distance_min: 0.35` — themselves [documented placeholders, not
  tuned](docs/adr) values). A corpus with denser or more ambiguous
  domain vocabulary would land differently.
- This measures the **entity-extraction** fallback rate only. Relation
  extraction from prose still doesn't exist — only frontmatter produces
  real relations today. That's a real, separate gap, not folded into this
  number.

**How to reproduce**: `kern project create dogfood --path dogfood-corpus
--embedding-provider ollama --embedding-model all-minilm
--extraction-provider ollama --extraction-model llama3.2`, then `kern
serve --project dogfood`, grepping its stderr for
`kern.ontology.fallback_rate` — every line carries the running `total`,
`fallback_total`, and `rate` as of that candidate; the numbers above are
from the last line.

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

## 4. Indexing concurrency

Not a numbered benchmark section with its own measured table — the corpus
used to validate this (a synthetic ~2MB fixture, generated on the fly, not
part of this repo) isn't reproducible from what's checked in here, and this
file's own bar is "every number measured, methodology reproducible next to
it." What's real and worth stating plainly instead: `kern serve` indexes
chunks concurrently, and both the client-side (`chunk_concurrency`) and
backend-side (`OLLAMA_NUM_PARALLEL`) knobs that control how much of that
concurrency is genuine (versus just a deeper queue in front of a backend
still serving one request at a time) are documented, with how to change
them and how to verify a change actually took effect, in the README's
[Indexing throughput](README.md#indexing-throughput) section. That section
also states a real, counter-intuitive finding from tuning this during
development: raising the backend's parallelism past its sweet spot made
indexing *slower*, not faster, on the hardware it was measured on — a
reminder to measure a change on your own hardware rather than assume more
concurrency is strictly better.

---

## 5. Semantic routing reliability

`query_ontological` routes a natural-language question either to a real
graph traversal (fast, deterministic, cites evidence) or a vector-search
fallback (approximate). The examples' README had already reproduced this
degrading on a 15-file corpus as an anecdote; `dogfood-corpus/`'s 15
structurally-identical features made it possible to measure the failure
rate instead of citing one example.

**Real measurement, before fixing it**: the same question shape ("what is
the `depends_on` relation for `TASK-0NNb`?") asked once per feature, 15
times. **0 of 15 used graph traversal — every single one fell back to
vector search**, and only 8 of those 15 vector-search answers happened to
land on the right entity anyway (53.3% overall correct). Root cause,
found by reading the routing code rather than guessing: the
entity-mention heuristic picked whichever question word matched *any*
known entity name and was textually longest, with no preference for an
*exact* name match over a coincidental substring one. A candidate entity
literally named `depends_on` — itself a bug, the model had extracted a
relation-type's own field name as if it were a domain entity, from prose
that discussed the frontmatter mechanism — out-lengthed the real target
identifier's matched word in every one of the 15 questions, since
`depends_on` (10 characters) is longer than `TASK-0NNb` (9).

**After fixing both the router (exact match now always wins over a
substring match) and the extraction path (a candidate whose name collides
with reserved relation-type vocabulary is rejected before it ever
reaches the entity table)**: the identical 15-question test now routes
**15 of 15 via graph traversal**, with the correct entity resolved every
time.

A third, unrelated bug surfaced during this verification: `kern serve`
reprocesses the whole corpus on every restart (no incremental cache yet —
see [Indexing throughput](README.md#indexing-throughput)), and
`record_relation` had no deduplication, so every restart against an
already-indexed project silently duplicated every frontmatter-derived
relation. `dogfood-corpus/` had been indexed twice while measuring this,
and its relation count was exactly 2x the expected 75 as a result — not
noise, a real correctness bug independent of the routing fix. Now fixed
with a `UNIQUE(type_id, source_entity_id, target_entity_id)` constraint
and `INSERT OR IGNORE`, the same idiom already used for type
deduplication.

**How to reproduce**: `kern project create dogfood --path dogfood-corpus
--embedding-provider ollama --embedding-model all-minilm
--extraction-provider ollama --extraction-model llama3.2`, `kern serve
--project dogfood` once, then ask `query_ontological` "what is the
depends_on relation for TASK-0NNb?" for `NN` in `01`–`15` and check each
response's `mode` field.

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
