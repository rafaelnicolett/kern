---
id: PLAN-009
kind: plan
status: approved
depends_on: []
implements: [SPEC-009]
---

# Implementation plan: API rate limiting

Limits are looked up via the same role a user already has (from user roles) rather than a separate rate-limit tier concept — one less thing for admins to configure per account.
