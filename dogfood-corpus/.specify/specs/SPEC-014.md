---
id: SPEC-014
kind: spec
status: approved
depends_on: [SPEC-004]
implements: []
---

# In-app comments

## Problem

Teams reviewing the same dashboard together have been leaving context in shared documents outside the product because there's no way to annotate a specific row or chart in kern-hosted dashboards directly.

## Requirements

- A user can leave a comment attached to a specific table row.
- Comments are visible to every team member with viewer role or above — commenting itself requires editor role or above.
- A comment thread on a row shows in chronological order with author and timestamp.
