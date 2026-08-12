---
id: TASK-004
kind: task
status: done
depends_on: []
implements: [PLAN-002]
---

# Add a plan-tier filter component

Only rendered for accounts that actually have sub-accounts on multiple
tiers — most accounts never see it. Serializes to `?tier=` in the URL,
same read-on-load convention as the date-range filter in TASK-003.
