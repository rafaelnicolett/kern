---
id: TASK-001a
kind: task
status: done
depends_on: []
implements: [PLAN-001]
---

# Add a streaming /export endpoint

Streams CSV rows as they're read from the database instead of buffering the whole result set in memory — the 500k row target from the spec would not fit comfortably in memory buffered all at once.
