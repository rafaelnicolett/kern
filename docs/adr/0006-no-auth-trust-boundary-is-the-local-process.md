# ADR-0006 — No Authentication: the Trust Boundary Is the Local Process

> Status: **accepted**

## Context

A default architecture template would call for an auth pattern (Bearer
JWT / OAuth2 / API Key) on every endpoint. kern v0 has no HTTP endpoint at
all — it's a local process spawned by an MCP host over stdio, or invoked
directly by the operator. Forcing an HTTP auth model wouldn't reflect how
the system actually works.

## Decision

**No authentication mechanism in v0.** The trust boundary is "whoever can
spawn/invoke the local process":

- Communication happens via **stdio** (MCP) or the **CLI** (direct
  invocation) — both already assume whoever starts the process has
  filesystem access to the user's machine, a higher trust level than any
  token could add.
- Data isolation is per **project** (its own SQLite file + LanceDB
  directory), not per authenticated user identity.
- When optional local HTTP lands (a future phase, out of v0 scope), this ADR
  needs revisiting — at that point there's a network surface, even if
  `localhost`-only, and "who can call this" stops having an obvious answer.

## Consequences

### Positive
- No token/credential management complexity in a binary that aims to be
  "zero setup friction" — auth would directly break the ~2-minute
  time-to-useful-response target.
- The trust model is honest about what kern actually is: a local tool, not a multi-user service.

### Negative
- If the binary is exposed beyond the local process without revisiting this
  ADR (e.g. someone exposes a future HTTP port publicly), there is no
  protection at all — a risk that needs documenting in whatever future work
  introduces HTTP.

### Neutral
- No `security`/`securitySchemes` field appears in the MCP tool contract —
  a deliberate absence, not an oversight.
