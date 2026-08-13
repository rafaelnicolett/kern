---
id: SPEC-005
kind: spec
status: approved
depends_on: [SPEC-004]
implements: []
---

# Audit trail viewer

## Problem

Admins on regulated customers' accounts have asked who changed a given setting and when — today that information exists in logs but isn't visible to the customer at all.

## Requirements

- Every role change, filter-schedule creation, and export is logged with actor, timestamp, and a human-readable description.
- Only admins can view the audit trail (enforced via the role helper).
- The audit trail is append-only — no edit or delete affordance, even for admins.
