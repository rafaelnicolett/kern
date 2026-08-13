---
id: SPEC-004
kind: spec
status: approved
depends_on: []
implements: []
---

# User roles and permissions

## Problem

Every user today has full access to every feature — several customers with larger teams have asked for a way to limit what junior team members can see or change.

## Requirements

- Three built-in roles: admin, editor, viewer.
- Role assignment is per-account, managed by an admin.
- Every other feature that gates an action must check the acting user's role before allowing it — this spec defines the role model, not every place it's enforced.
