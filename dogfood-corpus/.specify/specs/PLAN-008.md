---
id: PLAN-008
kind: plan
status: approved
depends_on: []
implements: [SPEC-008]
---

# Implementation plan: Notification preferences

A single notification_prefs table keyed by (user, category), checked by a shared should_notify(user, category) helper other features call before sending — mirrors how user roles centralized authorization.
