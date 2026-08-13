---
id: PLAN-007
kind: plan
status: approved
depends_on: []
implements: [SPEC-007]
---

# Implementation plan: Bulk actions on table rows

Select-all-matching-filter reuses the same filter serialization saved searches already store, so "select all" from a saved search is just re-running its filter, not a separate selection mechanism.
