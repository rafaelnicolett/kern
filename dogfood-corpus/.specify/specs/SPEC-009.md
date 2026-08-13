---
id: SPEC-009
kind: spec
status: approved
depends_on: [SPEC-004]
implements: []
---

# API rate limiting

## Problem

A handful of accounts have written scripts that poll the API aggressively enough to affect response times for other customers sharing the same database.

## Requirements

- Per-account rate limits on the public API, with limits varying by role (viewer gets the lowest limit, admin the highest).
- A rate-limited request returns 429 with a Retry-After header, never a silent drop.
- Rate limit state is enforced at the edge, before the request reaches application code.
