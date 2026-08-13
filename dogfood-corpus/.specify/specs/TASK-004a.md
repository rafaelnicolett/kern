---
id: TASK-004a
kind: task
status: done
depends_on: []
implements: [PLAN-004]
---

# Add the role column and migration

Backfills every existing membership row to 'admin' so no existing user's access silently changes on deploy.
