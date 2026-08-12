---
id: PLAN-003
kind: plan
status: approved
depends_on: [PLAN-001, PLAN-002]
implements: [SPEC-003]
---

# Implementation plan: Scheduled email reports

Reuses both prior features rather than re-implementing export or
filtering: the scheduler's job is only to decide *when* to call the
existing `/export` endpoint from PLAN-001, with the same filter query
parameters PLAN-002's components already produce.

1. A backend scheduler that stores each schedule's recurrence and filter
   query string, and calls the existing `/export` endpoint at send time —
   never a second, parallel export code path.
2. A "Schedule this report" button next to the existing "Download CSV"
   button, capturing whatever filters are currently active in the URL.
