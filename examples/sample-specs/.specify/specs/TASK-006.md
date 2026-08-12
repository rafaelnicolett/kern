---
id: TASK-006
kind: task
status: in_progress
depends_on: [TASK-004, TASK-005]
implements: [PLAN-003]
---

# Add a "Schedule this report" button to the dashboard

Sits next to the existing "Download CSV" button and captures whatever
filters are currently active in the URL — including the plan-tier filter
from TASK-004, for Enterprise accounts. Blocked on TASK-005, since there's
no scheduling backend to call yet.
