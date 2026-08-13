---
id: TASK-011b
kind: task
status: done
depends_on: [TASK-011a]
implements: [PLAN-011]
---

# Surface custom field columns in the dashboard and CSV export

Reads the account's field definitions to decide which JSON keys to render as columns, so the export feature needs no changes of its own beyond reading this definition list.
