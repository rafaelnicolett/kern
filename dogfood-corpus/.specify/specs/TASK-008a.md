---
id: TASK-008a
kind: task
status: done
depends_on: []
implements: [PLAN-008]
---

# Add notification_prefs table and should_notify() helper

Missing rows default to enabled, matching the spec's default — the helper treats absence as opt-in, not opt-out.
