---
id: TASK-009a
kind: task
status: done
depends_on: []
implements: [PLAN-009]
---

# Add the edge rate limiter keyed by account + role

Reads the acting user's role via the existing can()-adjacent role lookup and applies the role's configured limit before the request reaches application code.
