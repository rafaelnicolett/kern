---
id: TASK-005
kind: task
status: in_progress
depends_on: [TASK-001]
implements: [PLAN-003]
---

# Add a report-scheduling backend

Stores each schedule's recurrence (weekly/monthly) and filter query
string, and calls the existing streaming `/export` endpoint from TASK-001
at send time. Deliberately does not implement its own CSV generation —
depends directly on TASK-001 so the emailed file and the manually
downloaded file can never drift apart.
