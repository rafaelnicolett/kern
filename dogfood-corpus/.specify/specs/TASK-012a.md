---
id: TASK-012a
kind: task
status: done
depends_on: []
implements: [PLAN-012]
---

# Add the retention_window setting and nightly purge job

Deletes events (and their custom field JSON via cascade) older than the configured window; runs before the audit-trail purge task so the purge-of-audit-trail step can log against a still-intact table.
