---
id: SPEC-015
kind: spec
status: approved
depends_on: [SPEC-002, SPEC-006]
implements: []
---

# Saved-view version history

## Problem

Users editing a saved search sometimes want to go back to an earlier version of it after changing filters, and today saving over a saved search silently discards the previous filter set.

## Requirements

- Every save of an existing saved search keeps the prior version instead of overwriting it.
- A user can view up to the last 10 versions of a saved search and restore any of them as the current version.
- Restoring a version applies its filters exactly as saved-search reapplication already does — no new apply mechanism.
