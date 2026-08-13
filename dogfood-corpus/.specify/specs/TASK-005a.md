---
id: TASK-005a
kind: task
status: done
depends_on: []
implements: [PLAN-005]
---

# Add the audit_events table and write helper

A single `record_audit_event(actor, action, description)` helper other features call from their existing write paths, rather than each feature writing to the table directly.
