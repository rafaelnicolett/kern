---
id: TASK-003a
kind: task
status: done
depends_on: []
implements: [PLAN-003]
---

# Add a report_schedules table and cron dispatcher

A background worker polls due schedules and triggers the existing /export endpoint with the schedule's saved filters, then emails the resulting CSV as an attachment.
