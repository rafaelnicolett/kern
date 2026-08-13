---
id: PLAN-011
kind: plan
status: approved
depends_on: []
implements: [SPEC-011]
---

# Implementation plan: Custom fields

Values are stored as a single JSON column per event rather than dynamic schema columns, keeping the 20-field cap enforceable in application code without a migration per account.
