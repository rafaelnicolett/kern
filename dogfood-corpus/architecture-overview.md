# Architecture overview

This document has no frontmatter on purpose — it's the kind of free-form
prose a real project accumulates alongside its structured specs, and it
exercises kern's LLM-driven extraction path instead of the deterministic
frontmatter parse the SPEC/PLAN/TASK files use.

## Services

The product is a single Rails monolith backed by Postgres, with one
background worker process (Sidekiq) handling everything asynchronous:
scheduled report dispatch, webhook delivery, the nightly data-retention
purge job, and SAML assertion validation for SSO logins that arrive via a
redirect rather than the main request cycle.

There is no separate services layer per feature — CSV export, dashboard
filters, saved searches, and bulk actions all live in the same Rails app,
sharing one Postgres database and one Redis instance used both for
Sidekiq's queue and for rate-limiting counters.

## Authorization

Every feature that gates an action funnels through the same `can(user,
action)` helper introduced by user roles. This was a deliberate choice
after an early prototype had CSV export, audit trail, and bulk actions
each implementing their own ad hoc role checks — three slightly different
implementations of the same idea, one of which had a bug that let a
viewer trigger a bulk archive. Centralizing authorization in one helper
closed that whole class of bug at once instead of auditing every caller.

## Data model

Custom fields store their values as a single JSON column on the events
table rather than dynamic per-account schema columns. This was chosen
specifically so data retention's nightly purge job can delete an event row
and have its custom field values disappear via cascade, with no separate
purge step for custom field data. A dynamic-column approach would have
made that cascade impossible without per-account migrations.

## Rate limiting and webhooks

Webhook delivery shares the same per-account rate-limit budget as inbound
API traffic, enforced by the same edge rate limiter API rate limiting
introduced. This was not the original design — the first webhook
prototype had its own separate outbound rate limit, which meant a chatty
webhook subscription could still exhaust the account's inbound API budget
by proxy, defeating the point of rate limiting inbound traffic in the
first place. Sharing one budget across both directions closed that gap.

## Notifications

Every feature that sends email or triggers a webhook checks
`should_notify(user, category)` from notification preferences before
sending. Scheduled reports and webhook failure alerts are the two real
callers today; the category registry is deliberately open-ended so a
future feature can register a new notification category without changing
the preferences page itself.
