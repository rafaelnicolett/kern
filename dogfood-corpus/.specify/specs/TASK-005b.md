---
id: TASK-005b
kind: task
status: done
depends_on: [TASK-005a]
implements: [PLAN-005]
---

# Add the admin-only audit trail viewer page

Gated by the can() helper from user roles — a viewer or editor hitting this route gets a 403, not a redirect that implies the page doesn't exist.
