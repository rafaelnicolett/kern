---
id: TASK-013a
kind: task
status: done
depends_on: []
implements: [PLAN-013]
---

# Add SAML assertion consumer endpoint and IdP config

Validates the incoming assertion's signature against the configured IdP's certificate before trusting any of its claims, including the role-mapping claim.
