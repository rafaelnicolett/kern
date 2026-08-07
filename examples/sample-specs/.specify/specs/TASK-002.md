---
id: TASK-002
kind: task
status: in_progress
depends_on: [TASK-001]
implements: [PLAN-001]
---

# Add a Download CSV button to the dashboard

Calls the `/export` endpoint with the dashboard's active filters and date
range serialized as query parameters, then triggers a browser download of
the response. Blocked until TASK-001 ships, since there is nothing to call
before then.
