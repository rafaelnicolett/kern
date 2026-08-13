---
id: TASK-014a
kind: task
status: done
depends_on: []
implements: [PLAN-014]
---

# Add the comments table scoped to a row id

Stores author, row id, body, and timestamp; read access checked via can() at query time rather than filtering client-side.
