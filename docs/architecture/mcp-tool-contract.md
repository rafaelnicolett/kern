# MCP Tool Contract — kern

> kern exposes no REST API in v0 (see [ADR-0007](../adr/0007-mcp-as-the-primary-contract.md)).
> The real contract is the 6-tool surface served over **MCP stdio**, plus the
> CLI. This document reflects the current `kern-mcp`/`kern-cli` source
> directly — it is not a pre-implementation design doc, and should be updated
> whenever the Rust types it describes change.
>
> Naming convention: tool names and fields are `snake_case` (MCP/JSON
> convention, not REST camelCase).

## Error shape

Every tool returns `Result<Json<T>, String>` on the Rust side — MCP surfaces
that as `isError: true` with the string as the human-readable error text.
There is **no structured error envelope** (no `_meta.kern_error_code`, no
machine-parseable error code field) — this was an earlier design intent that
was never implemented. Some error paths do use an informal `PREFIX.CODE:`
text convention (e.g. `ONTOLOGY.CONCEPT_NOT_FOUND: no entity matches '...'`),
but it isn't applied consistently across every tool, and callers should not
rely on parsing it — treat error text as informational, meant for an agent
to read and react to in natural language, not as a stable API.

---

## `search_hybrid`

Vector similarity search over indexed chunks, re-ranked with a keyword-match
boost (Reciprocal Rank Fusion — see `kern-vector`).

| Input field | Type | Required |
|---|---|---|
| `query` | string | yes |
| `top_k` | integer (default: 10) | no |

**Output**:
```json
{ "chunks": [ { "chunk_id": "...", "file_path": "...", "content": "...", "score": 0.0 } ] }
```

## `query_by_concept`

Look up an entity by name/description and return its direct relations.

| Input field | Type | Required |
|---|---|---|
| `concept` | string (entity name or description) | yes |

**Output**:
```json
{ "entity": { "id": "...", "canonical_name": "...", "type_id": "..." },
  "direct_relations": [ { "type": "...", "source_entity_id": "...", "target_entity_id": "...", "evidence_chunk_id": "..." } ] }
```
**Error**: `"ONTOLOGY.CONCEPT_NOT_FOUND: no entity matches '<concept>'"` if nothing matches.

## `get_related_entities`

Local subgraph starting from an entity, walked outward up to a given depth.

| Input field | Type | Required |
|---|---|---|
| `entity_id` | string (uuid) | yes |
| `depth` | integer (default: 1) | no |

**Output**:
```json
{ "subgraph": { "entities": [ { "id": "...", "canonical_name": "...", "type_id": "..." } ] } }
```
Returns an empty `entities` list if `entity_id` doesn't exist or has no
related entities within `depth` hops — this is not treated as an error.

**Error**: `"invalid entity id: <value>"` if `entity_id` isn't a valid UUID.

## `get_ontology_schema`

No input.

**Output**: the current entity and relation type vocabulary, with counts.
```json
{ "entity_types": [ { "name": "...", "instance_count": 0 } ],
  "relation_types": [ { "name": "...", "status": "candidate|canonical", "instance_count": 0 } ] }
```

## `explain_relation`

Shortest path (BFS, up to 6 hops) between two entities, with the evidence
chunk backing each edge on the path.

| Input field | Type | Required |
|---|---|---|
| `entity_id_a` | string (uuid) | yes |
| `entity_id_b` | string (uuid) | yes |

**Output**:
```json
{ "path": [ { "type": "...", "from": "...", "to": "...", "evidence_chunk_id": "..." } ] }
```
**Errors**: `"invalid entity id: <value>"` for a malformed UUID;
`"ONTOLOGY.PATH_NOT_FOUND"` if no path connects the two entities.

## `query_ontological`

The main entry point. Embeds the question, semantically routes it against
known relation-type descriptions (cosine similarity, threshold `0.3`); if a
relation type scores above threshold *and* an entity mentioned in the
question is found, answers via graph traversal. Otherwise falls back to
`search_hybrid`.

| Input field | Type | Required |
|---|---|---|
| `question` | string (natural language) | yes |

**Output**:
```json
{ "mode": "graph_traversal | vector_fallback",
  "answer": "...",
  "evidence": [ { "chunk_id": "...", "excerpt": "..." } ] }
```

---

## CLI (outside the MCP protocol, part of the same agent-facing surface)

| Command | Description |
|---|---|
| `kern project create <name> --path <folder>` | Creates an isolated project |
| `kern serve --project <name>` | Starts the process (MCP over stdio) |
| `kern status [--project <name>]` | Reports health: chunk/entity/relation-type counts. Without `--project`, lists registered projects. Plain text output — there is no `--json` flag today. |
