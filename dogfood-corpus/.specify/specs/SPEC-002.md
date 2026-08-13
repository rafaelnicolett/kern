---
id: SPEC-002
kind: spec
status: approved
depends_on: []
implements: []
---

# Dashboard filters

## Problem

The usage table shows every event by default, and customers with high event volume say the default view is too noisy to be useful day to day.

## Requirements

- Filters for date range, event type, and account status, combinable with AND semantics.
- Filter state persists in the URL so a filtered view can be bookmarked or shared.
- Clearing all filters returns to the unfiltered default view in one click.
