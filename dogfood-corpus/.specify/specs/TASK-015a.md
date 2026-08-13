---
id: TASK-015a
kind: task
status: done
depends_on: []
implements: [PLAN-015]
---

# Add saved_search_versions and write-on-save

Every update to an existing saved search inserts a new version row instead of mutating the current one in place, capped at the 10 most recent per saved search.
