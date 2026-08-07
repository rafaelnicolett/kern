---
id: PLAN-001
kind: plan
status: approved
depends_on: []
implements: [SPEC-001]
---

# Implementation plan: CSV export

Two pieces of work, one backend and one frontend, landing in that order
since the button has nothing to call until the endpoint exists.

1. A streaming `/export` endpoint on the API so memory usage stays flat
   regardless of row count.
2. A "Download CSV" button on the dashboard that calls the endpoint with
   the dashboard's current filters attached.
