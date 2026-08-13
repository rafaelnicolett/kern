---
id: TASK-009b
kind: task
status: done
depends_on: [TASK-009a]
implements: [PLAN-009]
---

# Add Retry-After to 429 responses

Computes the exact seconds until the account's rate limit window resets rather than a fixed retry hint, so well-behaved clients back off correctly.
