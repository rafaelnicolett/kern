---
id: SPEC-003
kind: spec
status: approved
depends_on: [SPEC-001, SPEC-002]
implements: []
---

# Scheduled email reports

## Problem

Some customers export the same filtered CSV every Monday morning by hand,
every week, forever. They've asked, more than once, for kern's usage
dashboard to just email it to them on a schedule instead.

## Requirements

- A user can schedule a recurring email (weekly or monthly) that attaches
  the same CSV export defined in SPEC-001, with whatever dashboard filters
  (SPEC-002) were active when the schedule was created.
- Changing or canceling a schedule takes effect on the next send — no
  partially-sent reports.
- The emailed CSV must be byte-identical to what a user would get by
  clicking "Download CSV" with the same filters at send time, not a
  separately-generated approximation.
