---
id: TASK-007b
kind: task
status: done
depends_on: [TASK-007a]
implements: [PLAN-007]
---

# Add the bulk archive action

Gated by the can() helper (editor or above) — archives every row matching the current selection criterion in one background job, not row by row from the client.
