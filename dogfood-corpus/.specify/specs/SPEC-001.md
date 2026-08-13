---
id: SPEC-001
kind: spec
status: approved
depends_on: []
implements: []
---

# CSV export for the usage dashboard

## Problem

Customers ask for their usage data in a spreadsheet-friendly format at least once a week, and today the only way to get it is a support ticket that pulls a manual export from the database.

## Requirements

- A logged-in user can export the usage table currently shown on their dashboard as a CSV file.
- The export respects whatever date range and filters are active on the dashboard at export time — not a full-table dump.
- Export must work for accounts with up to 500k rows without timing out.
