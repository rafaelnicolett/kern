---
id: SPEC-007
kind: spec
status: approved
depends_on: [SPEC-006, SPEC-004]
implements: []
---

# Bulk actions on table rows

## Problem

Editors managing large accounts want to act on many rows at once (e.g. archive) instead of one row at a time, especially when working from a saved search that already narrows the set down.

## Requirements

- A user can select multiple rows via checkboxes, including a select-all-matching-filter option.
- The only bulk action in this spec is archive — other bulk actions are out of scope.
- Bulk archive requires editor role or above.
