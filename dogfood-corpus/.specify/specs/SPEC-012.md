---
id: SPEC-012
kind: spec
status: approved
depends_on: [SPEC-011, SPEC-005]
implements: []
---

# Data retention policy

## Problem

Some customers are contractually required to delete usage data after a fixed window, and today kern retains everything indefinitely with no purge mechanism at all.

## Requirements

- An admin can configure a retention window (90/180/365 days, or indefinite).
- Purge runs nightly and removes events, their custom field values, and their audit trail entries older than the window.
- A purge is itself recorded as an audit event before the purged rows are gone, so there's a record that a purge happened even though its subjects are deleted.
