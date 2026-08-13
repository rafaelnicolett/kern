---
id: TASK-015b
kind: task
status: done
depends_on: [TASK-015a]
implements: [PLAN-015]
---

# Add the version history panel and restore action

Restoring a version reuses the saved search's existing reapplication path directly — the panel only decides *which* filter string to reapply, it doesn't reimplement reapplication.
