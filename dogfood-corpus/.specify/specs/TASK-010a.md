---
id: TASK-010a
kind: task
status: done
depends_on: []
implements: [PLAN-010]
---

# Add webhook_subscriptions table and delivery worker

The delivery worker shares the rate limiter's per-account budget with inbound API traffic, so a chatty webhook can't starve the account's own API calls.
