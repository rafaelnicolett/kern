---
name: spec-context
description: Before implementing or modifying a spec, plan, or task in a kern-indexed Spec-Driven Development project, check kern's ontology for what it depends on, what implements it, and what related work already exists — so you don't duplicate a prior decision or miss a dependency. Use whenever a task file with frontmatter (id, kind, depends_on, implements) is being read, written, or referenced.
---

# spec-context

This project's specs, plans, and tasks are indexed by [kern](https://github.com/rafaelnicolett/kern),
exposed over MCP as the `kern` server (`search_hybrid`, `query_by_concept`,
`get_related_entities`, `get_ontology_schema`, `explain_relation`,
`query_ontological`). Its value only shows up if you call it before
writing code, not after.

## When starting work on a task

1. **Resolve the task's name to its id first.** Most of kern's tools take
   an `entity_id` (a UUID), not a human-readable name — `query_by_concept`
   is how you get it, and it also returns the entity's direct relations
   in the same call.

   ```
   query_by_concept({"concept": "TASK-006"})
   → { "entity": { "id": "32e767a1-...", "canonical_name": "TASK-006", ... },
       "direct_relations": [ ... ] }
   ```

2. **Walk the dependency graph before touching code**, using that id with
   `get_related_entities` at depth 2 or more — a task's `implements`
   relation points at a plan, which itself `implements` a spec, and that
   spec may `depends_on` other specs entirely. Reading only the task file
   in isolation misses that chain.

   ```
   get_related_entities({"entity_id": "32e767a1-...", "depth": 2})
   ```

3. **Prefer this two-step flow over `query_ontological` once the project
   has more than a handful of similar files.** Not a style preference —
   `query_ontological`'s semantic router gets measurably less reliable as
   more structurally similar files (several tasks with near-identical
   frontmatter, say) enter the corpus, and can return a plausible-looking
   answer about the wrong entity instead of erroring (see [`examples/README.md`](../README.md#5-relation-deduplication-and-routing-at-scale)
   for the reproduced example). `query_ontological` is fine for open-ended
   exploration where you don't already know the entity's name; once you
   do, resolve it explicitly instead of trusting the router to.

4. **Cite evidence, don't paraphrase from memory.** If a specific relation
   matters to a decision you're about to make, pull its path and evidence
   with `explain_relation`, using both entities' ids — rather than
   describing the relation from the frontmatter alone.

   ```
   explain_relation({"entity_id_a": "32e767a1-...", "entity_id_b": "89fd375a-..."})
   → { "path": [ { "type": "...", "from": "...", "to": "...", "evidence_chunk_id": "..." } ] }
   ```

5. **Before proposing a new spec, check whether the idea already has a
   decision attached to it** — a rejected approach written down in a
   free-form decision log (no frontmatter) is exactly the kind of context
   `search_hybrid` surfaces that a fresh spec draft would otherwise
   silently re-litigate.

   ```
   search_hybrid({"query": "why not a separate export path for schedules", "top_k": 3})
   ```

## What this catches in practice

- A task that already depends on something you were about to duplicate.
- A spec-level dependency (one feature's spec on two others, say) that
  isn't visible from the task file you happen to have open.
- A prior decision, written down in free-form prose with no frontmatter,
  that already ruled out the approach you're about to propose.

## What this does not do

kern's ontology only knows what's in the indexed corpus — it has no
opinion on code outside the specs/plans/tasks folder, and its relation
extraction from free-form prose (as opposed to frontmatter) only produces
entities today, not relations (see kern's own `BENCHMARKS.md`). Treat a
`query_ontological` `vector_fallback` response as retrieval, not as a
verified graph fact the way `get_related_entities`/`explain_relation`
are — they walk the stored graph, not a similarity search.
