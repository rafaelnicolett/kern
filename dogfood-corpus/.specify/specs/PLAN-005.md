---
id: PLAN-005
kind: plan
status: approved
depends_on: []
implements: [SPEC-005]
---

# Implementation plan: Audit trail viewer

A single `audit_events` table written to by the other features' existing write paths, and a read-only admin-gated view on top of it — no new write path of its own.
