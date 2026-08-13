---
id: TASK-007a
kind: task
status: done
depends_on: []
implements: [PLAN-007]
---

# Add row selection state and select-all-matching-filter

Selecting "all matching" stores the active filter set as the selection criterion rather than materializing every row id client-side, so it stays correct if new rows arrive mid-session.
