---
id: TASK-011a
kind: task
status: done
depends_on: []
implements: [PLAN-011]
---

# Add custom_field_defs and the JSON value column on events

Enforces the 20-field cap at write time in the application layer — the JSON column itself has no such limit, so this is an application invariant, not a database constraint.
