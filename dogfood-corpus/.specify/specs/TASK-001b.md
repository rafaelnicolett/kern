---
id: TASK-001b
kind: task
status: done
depends_on: [TASK-001a]
implements: [PLAN-001]
---

# Add the Download CSV button

Wires the dashboard's active filter state into the query parameters of the /export call — the button is disabled while a download is already in flight to avoid duplicate exports.
