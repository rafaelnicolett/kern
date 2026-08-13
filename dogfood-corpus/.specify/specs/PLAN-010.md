---
id: PLAN-010
kind: plan
status: approved
depends_on: []
implements: [SPEC-010]
---

# Implementation plan: Webhook subscriptions

Event categories reuse the notification-category registry from notification preferences, and delivery failure alerts route through the same should_notify() check rather than a separate webhook-specific notification path.
