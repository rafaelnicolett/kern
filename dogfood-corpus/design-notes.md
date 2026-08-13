# Design notes

Working notes, no frontmatter — closer to how this team actually writes
things down day to day than a formal spec is.

Bulk actions' select-all-matching-filter was the trickiest piece of UX in
this whole batch of features to get right. The obvious approach —
materialize every matching row's id client-side when the user clicks
"select all" — breaks the moment new rows arrive between selection and
the archive action actually running, especially since bulk archive runs
as a background job rather than synchronously. Storing the filter
criterion itself as the selection, and re-evaluating it inside the
archive job, means the action always operates on "whatever matches right
now" rather than a stale snapshot — a small design choice that avoids a
whole category of race condition.

Notification preferences shipped before webhooks specifically so webhooks
would have somewhere to route its failure alerts from day one, instead of
bolting a notification mechanism onto webhooks after the fact. Scheduled
reports similarly waited on both CSV export and dashboard filters landing
first — a scheduled report is really just "a saved filter plus a cron
trigger, delivered through the export path," and building it before
export and filters existed would have meant inventing a parallel export
mechanism just for schedules.

The audit trail's append-only property was almost compromised early on —
someone suggested letting admins edit an audit entry's description field
to correct typos. Rejected in review specifically because "the audit
trail is append-only, no exceptions" is a much easier property to reason
about and trust than "the audit trail is append-only except for
description edits by admins" — the moment there's an exception, an
auditor has to wonder whether *this* row is the edited kind.

SSO's password-login-disable check had to be enforced server-side at the
login form, not just hidden from the UI once SSO is configured for an
account. An earlier version only hid the password field in the frontend
when SSO was enabled — someone testing the feature found they could still
POST directly to the password-login endpoint and authenticate a
non-admin user that way, since the actual login handler never checked
whether SSO was configured for that account. That gap is why the current
version validates on the server, not just hides UI.

Custom fields and data retention interact in a way that wasn't obvious
until the retention spec was being written: because custom field values
live in a JSON column on the events table rather than a separate table,
deleting an event row during the nightly purge automatically removes its
custom field values too, via the database's normal row deletion —
no separate purge step needed for custom field data specifically. That
fell out of the JSON-column decision almost by accident, but it's now the
reason data retention's spec explicitly calls out that custom fields
don't need their own purge logic.
