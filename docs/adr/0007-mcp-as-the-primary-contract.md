# ADR-0007 — MCP as the Primary Contract, Not REST/OpenAPI

> Status: **accepted** (see implementation note below — error envelope scope narrowed from the original decision)

## Context

A default contract-first architecture template would assume OpenAPI/REST,
with a cloud C4 deployment diagram (regions, load balancers, DNS) and
PKCE/JWT authentication sequence diagrams. kern v0 has no REST — MCP over
stdio is the only external surface, and deployment is a single binary
running on the user's machine, with no network involved.

## Decision

- The primary, versioned contract is [`docs/architecture/mcp-tool-contract.md`](../architecture/mcp-tool-contract.md)
  — the 6 MCP tools with their input/output shapes, generated from the real
  `kern-mcp` source rather than written ahead of implementation.
- `api/openapi.yaml` stays in the repository as an **honest placeholder**
  (no paths), pointing to the real contract — it exists only so tooling that
  expects the file to be present doesn't break, never as a source of truth.
- Sequence and container diagrams (kept in the private delivery workspace,
  not published in this repo) cover the two flows that actually define the
  product — ingestion through an ontology decision, and `query_ontological`
  routing with its vector-search fallback — not authentication, because
  there is none (see ADR-0006).

> **Implementation note (added after the fact)**: the original decision also
> specified a structured MCP error envelope (`isError: true` +
> `_meta.kern_error_code` with a `<BOUNDED_CONTEXT>.<CODE>` naming scheme,
> mirroring a typical REST `{code,message,details,traceId}` shape). What's
> actually implemented is simpler: each tool returns `Result<Json<T>, String>`,
> and error strings are plain, human-readable text — some do carry an
> informal `PREFIX.CODE:` convention (e.g. `ONTOLOGY.CONCEPT_NOT_FOUND`), but
> there's no structured `_meta` envelope, and not every error path uses the
> prefix convention consistently. Treat the prefix strings as a loose,
> evolving convention, not a stable machine-parseable contract yet.

## Consequences

### Positive
- The architecture artifacts describe the system that actually exists, not a REST/cloud fiction that would mislead anyone reading them later.
- If optional local HTTP lands post-v0, `api/openapi.yaml` gains real content reflecting whatever endpoints exist then — without rewriting the MCP contract, which stays the primary surface.

### Neutral
- This ADR doesn't lock in the shape of a future HTTP contract — it only documents why one doesn't exist yet.
