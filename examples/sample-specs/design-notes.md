# Design notes: CSV export

This file has no frontmatter on purpose — it's the free-form counterpart to
the specs/plan/tasks in `.specify/specs/`, meant to exercise kern's prose
fallback path rather than the deterministic frontmatter path.

Row streaming for the export endpoint is implemented with a cursor-based
database query rather than `OFFSET`/`LIMIT` paging, since offset pagination
degrades badly past a few hundred thousand rows — exactly the range
SPEC-001 asks this to handle. The dashboard's CSV button reuses the same
filter-serialization helper the dashboard's own data-fetching code already
uses, so the exported rows always match what's on screen.
