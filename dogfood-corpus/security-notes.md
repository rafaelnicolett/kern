# Security notes

Free-form, no frontmatter — notes from a security review pass across
authentication, authorization, and access-control-adjacent features.

## Role model

The three built-in roles (admin, editor, viewer) are intentionally coarse.
A finer-grained permission system (per-feature toggles rather than three
fixed tiers) was considered and explicitly deferred — every additional
permission dimension is another thing an admin has to configure correctly,
and this product's customer base skews toward small teams where "can this
person change settings" is close enough to "can this person do anything
destructive" that three tiers cover the real cases seen in support
tickets so far.

## SSO assertion validation

The SAML assertion consumer endpoint validates the incoming assertion's
signature against the account's configured identity-provider certificate
before trusting any claim inside it, including the role-mapping claim
that decides whether a provisioned user lands as admin, editor, or
viewer. An unmapped role attribute defaults to viewer, not admin —
failing closed on an unrecognized claim rather than granting broad access
by default.

## Rate limiting as a security boundary, not just a performance one

API rate limiting was originally scoped purely as a performance
protection (stopping one account's scripted polling from degrading
response times for others), but it also functions as a coarse defense
against credential-stuffing-style abuse of the API, since a viewer-tier
account's low limit caps how fast an attacker with a leaked
low-privilege credential could enumerate anything through the API even
before any application-level abuse detection would kick in.

## Audit trail as an incident-response tool

The audit trail's append-only guarantee (no edit or delete affordance,
even for admins) exists specifically so it can be trusted during incident
response — if an account is compromised, the audit trail is one place
that account's own admin credentials can't have been used to cover tracks
in, since even a compromised admin account has no delete or edit path
into that table. Data retention's nightly purge is the *only* thing that
ever removes audit entries, and even that purge logs its own audit event
describing what it removed before removing it.

## Webhook delivery is not currently signed

Outbound webhook payloads today aren't signed with a per-subscription
secret the receiving endpoint could use to verify authenticity — a
receiving endpoint currently has no cryptographic way to confirm a
webhook delivery actually came from this product and not a third party
that guessed or leaked the subscription's URL. Flagged during this review
as a real gap, not yet scoped into a spec.
