---
id: PLAN-012
kind: plan
status: approved
depends_on: []
implements: [SPEC-012]
---

# Implementation plan: Data retention policy

Purging custom field values reuses the JSON column from custom fields directly (deleting the parent event row cascades), and the audit-trail purge has to run last, after logging its own audit event, or there would be nothing left to prove the purge happened.
