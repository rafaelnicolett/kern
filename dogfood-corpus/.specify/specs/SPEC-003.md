---
id: SPEC-003
kind: spec
status: approved
depends_on: [SPEC-001, SPEC-002]
implements: []
---

# Scheduled email reports

## Problem

Some customers check the dashboard purely to eyeball the same weekly numbers — they've asked for that view to just show up in their inbox instead.

## Requirements

- A user can schedule a recurring email of their current filtered dashboard view as a CSV attachment.
- Schedule options are daily, weekly, or monthly, at a user-chosen time.
- A scheduled report reuses the CSV export path — no separate export format.
