---
id: TASK-012b
kind: task
status: done
depends_on: [TASK-012a]
implements: [PLAN-012]
---

# Purge stale audit trail entries and log the purge itself

Logs a single audit event summarizing what was purged *before* deleting the old rows — otherwise there would be no audit trail of the purge, defeating the audit trail's own purpose.
