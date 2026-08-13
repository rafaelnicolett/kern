---
id: SPEC-008
kind: spec
status: approved
depends_on: []
implements: []
---

# Notification preferences

## Problem

As more features start sending email (scheduled reports, and planned webhook failure alerts), users need one place to control what they receive rather than an all-or-nothing setting per feature.

## Requirements

- A per-user preferences page listing every notification category this product sends, each independently toggleable.
- A new notification category defaults to enabled unless the spec introducing it says otherwise.
- Every feature that sends a notification must check this preference before sending, not just show it in the UI.
