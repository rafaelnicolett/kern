# Decision log

Free-form, no frontmatter — records of decisions and rejected alternatives
that never made it into a formal spec, the kind of context `search_hybrid`
is meant to surface before a proposal accidentally re-litigates something
already settled.

## Why saved searches store a query string, not structured filter objects

Early discussion considered storing each saved search as structured JSON
(`{date_range: ..., event_type: ...}`) instead of the serialized URL query
string the filter bar already produces. Rejected: it would have meant two
representations of the same filter state to keep in sync every time the
filter bar gained a new filter type, and bulk actions' select-all-matching
feature specifically depends on saved searches and the filter bar sharing
one representation, not two.

## Why SSO doesn't get its own role model

Considered giving SAML-provisioned users a distinct role namespace from
password-login users, since some customers' IdPs return more granular
group claims than kern's three built-in roles support. Rejected in favor
of mapping every SAML role claim down to admin/editor/viewer — a separate
SSO role model would have meant every other feature's `can()` checks
needed to understand two role systems instead of one, which defeats the
entire point of centralizing authorization behind one helper.

## Why data retention purges the audit trail last, not first

An earlier draft of the retention job purged the audit trail first (oldest
data, seemed lowest-risk to remove first) and events second. This was
wrong: if the audit trail purge runs first, there's no audit trail left to
record that the event purge subsequently happened, which defeats the
audit trail's own "here's a record of what happened, even to data that's
now gone" purpose. The nightly job now purges events first, logs an audit
event describing exactly what was purged, and only then purges old audit
entries — logging happens before its own subject could be purged away.

## Why webhook auto-disable doesn't just silently stop retrying

Considered silently dropping a webhook after repeated failures with no
notification, on the theory that a broken endpoint's owner would notice
their integration stopped working. Rejected after a support ticket from
an earlier, unrelated silent-failure incident (a scheduled report that
stopped sending with no notice) — the auto-disable now explicitly
notifies the webhook's owner through `should_notify()`, respecting
whatever preference they've set for that category rather than assuming
they'll notice.

## Why custom fields cap at 20, not unlimited

Considered no hard limit on custom field count per account, letting
usage patterns decide. Capped at 20 instead, enforced in the application
layer rather than a database constraint, specifically so the cap could be
revisited later without a migration — the JSON value column itself has no
inherent limit, the constraint exists only to keep the dashboard table and
CSV export from growing an unbounded number of extra columns for accounts
that would otherwise define hundreds of fields.
