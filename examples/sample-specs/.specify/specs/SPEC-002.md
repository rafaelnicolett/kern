---
id: SPEC-002
kind: spec
status: approved
depends_on: []
implements: []
---

# Filterable usage dashboard

## Problem

The usage dashboard currently shows all-time data for every account, with
no way to narrow it down. Customers with long account histories say the
dashboard is close to useless for answering "how are we trending this
quarter" without exporting everything and filtering in a spreadsheet.

## Requirements

- A date-range filter (last 7 days, last 30 days, this quarter, custom
  range) that narrows every chart and table on the dashboard at once.
- A plan-tier filter, for accounts on our Enterprise plan that manage
  multiple sub-accounts on different tiers.
- Whatever filters are active must be reflected in the URL, so a filtered
  view can be bookmarked or shared with a teammate.
