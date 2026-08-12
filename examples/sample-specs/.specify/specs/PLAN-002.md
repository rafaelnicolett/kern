---
id: PLAN-002
kind: plan
status: approved
depends_on: []
implements: [SPEC-002]
---

# Implementation plan: Filterable usage dashboard

Two independent filter components, both writing to the same URL query
string so they can be built and shipped in either order.

1. A date-range filter component (presets + a custom range picker),
   serializing to `?range=` in the URL.
2. A plan-tier filter component, serializing to `?tier=` in the URL, only
   rendered for accounts that actually have sub-accounts on multiple tiers.

Both components read their initial state from the URL on page load, so a
bookmarked or shared link reproduces the exact same filtered view.
