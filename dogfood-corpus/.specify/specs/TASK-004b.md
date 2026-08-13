---
id: TASK-004b
kind: task
status: done
depends_on: [TASK-004a]
implements: [PLAN-004]
---

# Add the shared can(user, action) authorization helper

Centralizes role-to-permission mapping in one place so other features gate actions through this helper instead of duplicating role logic.
