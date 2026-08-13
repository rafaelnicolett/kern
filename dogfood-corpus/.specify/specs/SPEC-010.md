---
id: SPEC-010
kind: spec
status: approved
depends_on: [SPEC-009, SPEC-008]
implements: []
---

# Webhook subscriptions

## Problem

Customers integrating kern-hosted dashboards into their own tools currently have to poll the API for changes — several have asked for push notifications instead.

## Requirements

- A user can register a webhook URL and choose which event categories to receive.
- Webhook delivery retries with backoff on failure, and a webhook that fails repeatedly is auto-disabled with a notification to the owner.
- Outbound webhook delivery is itself subject to the same rate-limiting infrastructure as inbound API calls.
