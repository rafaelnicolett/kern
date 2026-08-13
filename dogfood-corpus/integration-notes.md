# Integration notes

Free-form, no frontmatter — notes on how the outward-facing integration
surfaces (API, webhooks, SSO) fit together, written after a customer
asked whether their webhook traffic counted against their API rate limit
(it does).

## Rate limiting applies to webhooks too

The edge rate limiter introduced for inbound API traffic enforces the
same per-account, per-role budget against outbound webhook delivery — a
webhook subscription configured to fire on every event for a
high-volume account can exhaust that account's rate limit budget just as
inbound polling could, and once exhausted, further webhook deliveries
queue and retry with backoff rather than bypassing the limit. This
surprised at least one customer during early testing, who assumed
webhook traffic was accounted separately from their API usage — it isn't,
by design, specifically to prevent the rate limiter from being trivially
bypassed by routing traffic through webhooks instead of polling.

## SSO and API access are separate concerns

Enabling SSO for an account changes how *browser* login works — it has no
effect on API authentication, which continues to use per-account API
keys regardless of whether the account has SSO enabled. A customer who
enabled SSO and then asked why their existing API integration still
worked with the old key had this explained: SSO governs the login form,
not the API key issuance or validation path, and there's currently no
plan to require SSO-equivalent authentication for API access.

## Webhook event categories mirror notification categories

The event categories a webhook subscription can select from are the same
category registry notification preferences uses for email, not a
separate webhook-specific taxonomy. This means a new feature that
registers a notification category automatically becomes a webhook event
category too, with no extra registration step — the two features
deliberately share one registry rather than maintaining parallel lists
that could drift out of sync with each other.

## What isn't integrated yet

Custom field values are not currently included in webhook payloads, only
in CSV exports and the dashboard table — a webhook subscriber today only
receives the fixed-schema event fields. Also not yet integrated: there is
no webhook event for data-retention purges, so a subscriber has no way to
learn that a batch of events they'd previously received has since been
purged from the source system.
