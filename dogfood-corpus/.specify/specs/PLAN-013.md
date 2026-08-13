---
id: PLAN-013
kind: plan
status: approved
depends_on: []
implements: [SPEC-013]
---

# Implementation plan: SSO integration (SAML)

Role mapping writes into the exact same membership.role column user roles already defined, so every other feature's role checks work unchanged for SSO-provisioned users — SSO is a new way to set that column, not a new authorization model.
