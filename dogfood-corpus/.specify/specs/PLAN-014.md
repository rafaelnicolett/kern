---
id: PLAN-014
kind: plan
status: approved
depends_on: []
implements: [SPEC-014]
---

# Implementation plan: In-app comments

Comment visibility reuses the can() role helper directly (viewer can read, editor can write) rather than a separate comment-specific permission, keeping one authorization model across the product.
