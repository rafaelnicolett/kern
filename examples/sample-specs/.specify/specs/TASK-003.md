---
id: TASK-003
kind: task
status: done
depends_on: []
implements: [PLAN-002]
---

# Add a date-range filter component

Presets (last 7 days, last 30 days, this quarter) plus a custom range
picker for anything else. Serializes to `?range=` in the URL and reads its
initial state from there on page load, so a bookmarked filtered view
reproduces exactly.
