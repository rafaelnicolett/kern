---
id: TASK-001
kind: task
status: done
depends_on: []
implements: [PLAN-001]
---

# Add a streaming /export endpoint

Streams CSV rows as they're read from the database instead of buffering the
whole result set in memory — the requirement that drives this is the 500k
row target from SPEC-001, which would not fit comfortably in memory
buffered all at once.
