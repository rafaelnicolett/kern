# Decision log: usage dashboard exports

Free-form, no frontmatter — a running log of small technical decisions
across the CSV export, dashboard filters, and scheduled reports work,
kept in one place instead of scattered across PR descriptions.

**Cursor pagination over OFFSET/LIMIT for the export endpoint.** Offset
pagination degrades badly past a few hundred thousand rows, which is
exactly the range the CSV export needs to handle for our larger accounts.
A cursor on the row's primary key avoids the growing-offset cost entirely.

**Filters live in the URL, not in a server-side session.** Both the
date-range and plan-tier filters serialize to query parameters rather
than server-stored state. This was the deciding factor that made
scheduled reports simple: a schedule just stores a filter query string,
with no session or cookie to keep alive between sends.

**Scheduled reports call the existing export endpoint, not a new one.**
Considered and rejected: a separate, simplified export path for
schedules, without plan-tier filter support, to ship the scheduler
faster. Rejected because it would let a scheduled report and a manually
downloaded report show different data for the same account and the same
filters — worse than shipping a week later.

**No partial sends on schedule edits.** Editing or canceling a schedule
takes effect strictly on the next send, never retroactively and never
mid-send. A schedule that's already generating a report when it's edited
finishes with the old configuration.
