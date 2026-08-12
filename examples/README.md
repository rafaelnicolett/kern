# kern examples — a real, worked tutorial

Every command and every JSON transcript on this page is real — copied
verbatim from actually running `kern` against the corpus in this folder,
over the real MCP protocol (not simulated). Where something behaved worse
than you'd expect, that's said explicitly instead of edited out — see
[What actually happened, honestly](#what-actually-happened-honestly) at
the end.

## 1. The corpus

[`sample-specs/`](sample-specs) is a small, realistic Spec-Driven
Development project: a hypothetical "usage dashboard" product with **3
related features**, not one isolated toy example — so the ontology has
real cross-feature dependencies to traverse, not just a single
spec→plan→task chain.

```
sample-specs/
├── .specify/specs/
│   ├── SPEC-001.md, PLAN-001.md, TASK-001.md, TASK-002.md   # Feature A: CSV export
│   ├── SPEC-002.md, PLAN-002.md, TASK-003.md, TASK-004.md   # Feature B: dashboard filters
│   └── SPEC-003.md, PLAN-003.md, TASK-005.md, TASK-006.md   # Feature C: scheduled reports
├── architecture-overview.md   # free-form prose, no frontmatter
├── decision-log.md            # free-form prose, no frontmatter
└── design-notes.md            # free-form prose, no frontmatter
```

**Feature A** (CSV export) and **Feature B** (dashboard filters) are
independent of each other. **Feature C** (scheduled email reports)
genuinely depends on both — `SPEC-003.md`'s frontmatter has
`depends_on: [SPEC-001, SPEC-002]`, `TASK-006.md` depends on `TASK-004`
(a Feature B task) *and* `TASK-005` (its own sibling). That's not
decorative — it's what makes a multi-hop graph traversal in §4 actually
demonstrate something a human reading one file couldn't see at a glance.

The three prose files have **no frontmatter on purpose** — they exercise
kern's free-form path (LLM extraction + distance-based classification)
instead of the deterministic frontmatter parse the 12 spec/plan/task
files use.

## 2. Build kern and index the corpus

```bash
cargo build --release -p kern-cli
```

`kern project create` resolves a model provider as part of creating the
project (see the main [README's Model backend](../README.md#model-backend)
section) — non-interactively, so this reproduces exactly:

```bash
target/release/kern project create demo --path examples/sample-specs \
  --embedding-provider ollama --embedding-model all-minilm \
  --extraction-provider ollama --extraction-model llama3.2
```

Requires `ollama pull all-minilm` and `ollama pull llama3.2` first. Run it
interactively (drop both `--embedding-*`/`--extraction-*` flag pairs) in a
real terminal instead, and you get the guided setup wizard.

**This corpus is 3-4x the size of a toy example, and ontology enrichment
is a real LLM call per candidate — indexing it for the first time
realistically takes a few minutes on CPU**, not seconds. That's not a
hang; `kern serve`'s stderr (visible if you run it directly, not through
the driver scripts below) logs one `kern.ontology.fallback_rate` line per
candidate as it works through the corpus, so you can watch it progress.

## 3. The two driver scripts

Both spawn the real compiled `kern` binary and speak MCP JSON-RPC over
stdio directly — no mocking, no simulated transport.

- **`query_ontological.py`** — calls the one tool most agent interactions
  actually use: `query_ontological`, the router that decides between graph
  traversal and vector search for you.
- **`call_tool.py`** — calls *any* of kern's 6 tools with arbitrary JSON
  arguments, for when you want a specific, deterministic answer rather
  than the router's best guess.

```bash
python3 examples/query_ontological.py target/release/kern demo "<question>"
python3 examples/call_tool.py target/release/kern demo <tool-name> '<json-args>'
```

Both scripts start a **fresh** `kern serve` each time — `kern` re-indexes
the whole corpus on every `serve` invocation today (there's no
across-process cache yet), so each call below really does pay the full
indexing cost from §2. That's real, current behavior, not a mistake in
these scripts.

## 4. Walking through each tool, with real output

### `query_by_concept` — resolve a name to its real id

Most of kern's tools take a real `entity_id` (a UUID), not a human-readable
name — `query_by_concept` is how you go from a name/description to the
real id in the first place. This is the step an agent (or the skill in
§6) does first, before calling anything else by id.

```
$ python3 examples/call_tool.py target/release/kern demo \
    query_by_concept '{"concept": "TASK-006"}'

{
  "entity": {
    "id": "32e767a1-be76-490d-96fc-dbcc3322bd4c",
    "canonical_name": "TASK-006",
    "type_id": "b4732f67-6a7c-4837-81b0-143c23d2dd61"
  },
  "direct_relations": [
    { "type": "d11986a8-...", "source_entity_id": "32e767a1-...", "target_entity_id": "89fd375a-...", "evidence_chunk_id": "5caf08f6-..." },
    { "type": "d11986a8-...", "source_entity_id": "32e767a1-...", "target_entity_id": "db513646-...", "evidence_chunk_id": "5caf08f6-..." },
    { "type": "71c107a1-...", "source_entity_id": "32e767a1-...", "target_entity_id": "8707e727-...", "evidence_chunk_id": "5caf08f6-..." }
  ]
}
```

(Real response, `direct_relations` truncated here — see
[§5](#5-a-real-limitation-relation-deduplication) for why the real array
has more entries than this.) Relation *type* ids are UUIDs too — cross-
reference them against `get_ontology_schema` (below) if you need the
human name, or just use `query_ontological` for natural-language access
instead of raw ids.

### `get_related_entities` — multi-hop graph traversal

This is the payoff of having 3 features with real cross-references
instead of one isolated toy chain. Starting from `TASK-006` (Feature C)
at depth 2:

```
$ python3 examples/call_tool.py target/release/kern demo \
    get_related_entities '{"entity_id": "32e767a1-be76-490d-96fc-dbcc3322bd4c", "depth": 2}'

{
  "subgraph": {
    "entities": [
      { "canonical_name": "TASK-004", "id": "89fd375a-...", ... },
      { "canonical_name": "TASK-005", "id": "db513646-...", ... },
      { "canonical_name": "PLAN-003", "id": "8707e727-...", ... },
      { "canonical_name": "PLAN-001", "id": "6347b885-...", ... },
      { "canonical_name": "PLAN-002", "id": "fe39d3c9-...", ... },
      { "canonical_name": "SPEC-003", "id": "5228fa54-...", ... },
      { "canonical_name": "TASK-001", "id": "85b4ed1b-...", ... }
    ]
  }
}
```

Two real hops from one task file surfaced its direct dependencies
(`TASK-004`, `TASK-005`), what it implements (`PLAN-003`), *and* — one hop
further — the plans and spec that Feature C's plan itself depends on
(`PLAN-001`, `PLAN-002`, `SPEC-003`) and a dependency of `TASK-005`
(`TASK-001`, from Feature A). None of that cross-feature web is visible
from reading `TASK-006.md` alone — that's the actual point of the
ontology existing at all.

### `get_ontology_schema` — the full type vocabulary

```
$ python3 examples/call_tool.py target/release/kern demo get_ontology_schema '{}'

{
  "entity_types": [
    { "name": "task", "instance_count": 6 },
    { "name": "spec", "instance_count": 3 },
    { "name": "plan", "instance_count": 3 },
    { "name": "component", "instance_count": 22 },
    { "name": "concept", "instance_count": 17 },
    ... 32 more, mostly single-instance ...
  ],
  "relation_types": [
    { "name": "depends_on", "status": "canonical", "instance_count": 82 },
    { "name": "implements", "status": "canonical", "instance_count": 93 },
    { "name": "causes", "status": "canonical", "instance_count": 0 },
    ... 5 more canonical types, 0 instances each ...
  ]
}
```

`task: 6`, `spec: 3`, `plan: 3` are exactly right — those come from the 12
frontmatter files' deterministic `kind` field, one entity per file, no
ambiguity possible. Everything else in that 37-entity-type list came from
LLM extraction over the 3 free-form prose files — see
[§5](#5-a-real-limitation-relation-deduplication) for what that actually
looks like and why it's noisier.

### `query_ontological` — the router

```
$ python3 examples/query_ontological.py target/release/kern demo \
    "what is the depends_on relation for TASK-002?"

{
  "mode": "vector_fallback",
  "answer": "id: TASK-004\nkind: task\nstatus: done\ndepends_on: []\nimplements: [PLAN-002]\n---\n\n",
  "evidence": [ ... ]
}
```

This is a real, unedited transcript, and it's a worse answer than
`get_related_entities` gave above — it retrieved `TASK-004`, not `TASK-002`.
Kept here on purpose, not swapped for a cleaner-looking run — see the next
section for why, and what to do about it.

## 5. A real limitation: relation deduplication and routing at scale

Two honest findings from building this specific corpus, not present (or
not visible) in a smaller one:

**`direct_relations` has duplicate entries.** `TASK-006` was extracted as
a candidate from more than one chunk (the frontmatter pass, plus every
prose chunk that mentions "TASK-006"), and each pass recorded the same
`depends_on`/`implements` relation again with a different
`evidence_chunk_id`. The real response in §4 has 21 entries where 3
distinct relations would do. kern doesn't deduplicate relations across
separate extraction passes today — a real gap, not a display artifact.

**`query_ontological`'s routing gets less reliable as the corpus grows.**
The 5-file corpus this repo's main README uses got a clean
`graph_traversal` result for "what is the depends_on relation for
TASK-002?" On *this* 15-file corpus, the identical question falls back to
vector search — and worse, the vector search itself returns `TASK-004`
instead of `TASK-002`, because every task file's frontmatter block
(`id: ...\nkind: task\nstatus: ...\ndepends_on: [...]\nimplements: [...]`)
is short and structurally near-identical, so their embeddings sit very
close together. This is real, reproduced behavior — confirmed by running
the exact same question the smaller corpus answers correctly.

**What this means in practice**: for anything where you need the specific
right entity, prefer `query_by_concept` → `get_related_entities` (§4) over
`query_ontological` once your corpus has more than a handful of
structurally-similar files — this is exactly what the [skill](#6-a-real-skill-using-kerns-mcp-tools)
below does, and exactly why it exists instead of just telling an agent to
"use query_ontological."

## 6. A real skill using kern's MCP tools

[`skills/spec-context/SKILL.md`](skills/spec-context/SKILL.md) is a
standard Claude Code skill — drop the `spec-context` folder into your own
project's `.claude/skills/` — that teaches an agent to check kern's
ontology *before* implementing or modifying a task, using the real
two-step `query_by_concept` → `get_related_entities`/`explain_relation`
flow from §4, precisely because §5 showed `query_ontological` alone isn't
reliable enough for this once a corpus has some real size to it.

## 7. Reproduce this whole page yourself

```bash
cargo build --release -p kern-cli
ollama pull all-minilm && ollama pull llama3.2
target/release/kern project create demo --path examples/sample-specs \
  --embedding-provider ollama --embedding-model all-minilm \
  --extraction-provider ollama --extraction-model llama3.2

python3 examples/call_tool.py target/release/kern demo query_by_concept '{"concept": "TASK-006"}'
python3 examples/call_tool.py target/release/kern demo get_related_entities '{"entity_id": "<id from the previous call>", "depth": 2}'
python3 examples/call_tool.py target/release/kern demo get_ontology_schema '{}'
python3 examples/query_ontological.py target/release/kern demo "what is the depends_on relation for TASK-002?"
```

Entity ids are generated fresh (`Uuid::new_v4()`) on every real index, so
yours won't match the ones on this page — that's expected, substitute
your own from the `query_by_concept` response.
