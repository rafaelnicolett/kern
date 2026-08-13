---
id: PLAN-001
kind: plan
status: approved
depends_on: []
implements: [SPEC-001]
---

# Implementation plan: CSV export for the usage dashboard

A streaming `/export` endpoint on the API so memory usage stays flat regardless of row count, then a "Download CSV" button that calls it with the dashboard's current filters attached.
