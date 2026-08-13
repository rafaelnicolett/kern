---
id: SPEC-013
kind: spec
status: approved
depends_on: [SPEC-004]
implements: []
---

# SSO integration (SAML)

## Problem

Enterprise customers' security teams require single sign-on before they'll approve rolling kern-hosted dashboards out beyond a pilot team.

## Requirements

- An admin can configure a SAML identity provider for their account.
- Once SSO is enabled, password login is disabled for that account's non-admin users — admins keep a password fallback.
- A SAML assertion's role attribute maps to one of the three built-in roles; unmapped attributes default to viewer.
