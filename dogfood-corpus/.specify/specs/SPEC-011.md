---
id: SPEC-011
kind: spec
status: approved
depends_on: []
implements: []
---

# Custom fields

## Problem

Different customers track different metadata about their usage events, and today the schema only has kern's own fixed columns — customers have been asking to attach their own key/value data.

## Requirements

- An admin can define up to 20 custom fields per account, each typed as text, number, or boolean.
- Custom field values are included in CSV exports and visible as extra columns on the dashboard table.
- Deleting a custom field definition removes its values but does not delete the underlying events.
