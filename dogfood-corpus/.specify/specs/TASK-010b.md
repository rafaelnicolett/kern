---
id: TASK-010b
kind: task
status: done
depends_on: [TASK-010a]
implements: [PLAN-010]
---

# Add auto-disable after repeated failures

After 10 consecutive failed deliveries, disables the subscription and notifies the owner via should_notify(), respecting whatever preference they've set for that category.
