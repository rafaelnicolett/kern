---
id: TASK-013b
kind: task
status: done
depends_on: [TASK-013a]
implements: [PLAN-013]
---

# Disable password login for non-admin users once SSO is on

Checked at the login form, not just hidden in the UI — a direct password-login request for a non-admin user on an SSO-enabled account is rejected server-side.
