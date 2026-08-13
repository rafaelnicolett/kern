---
id: PLAN-004
kind: plan
status: approved
depends_on: []
implements: [SPEC-004]
---

# Implementation plan: User roles and permissions

A single `role` column on the account-membership table, checked by a shared authorization helper other features call rather than each reimplementing role checks.
