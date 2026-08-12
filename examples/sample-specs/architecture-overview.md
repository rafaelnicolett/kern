# Architecture overview: usage dashboard exports

This file has no frontmatter on purpose, same as `design-notes.md` — it's
meant to exercise kern's free-form prose path, where entities and
relations come from LLM extraction and distance-based classification
rather than a deterministic frontmatter parse.

The three shipped and in-flight features around the usage dashboard share
one export code path on purpose, rather than three separate ones:

- The CSV export endpoint (streaming, cursor-based pagination) is the only
  place that ever queries the usage table for an export. Both the manual
  "Download CSV" button and the scheduled email reports call the exact
  same endpoint with the exact same query parameters.
- The dashboard's filter components (date range, plan tier) are the only
  place that ever builds those query parameters. A schedule stores the
  filter query string it was created with, verbatim — it does not
  re-derive filters from some separate stored representation.
- This is deliberate: an earlier internal draft of the scheduled-reports
  design considered giving the scheduler its own simplified filter model
  (just a date range, no plan-tier support) to ship faster. That was
  rejected specifically because it would have meant a scheduled report and
  a manually downloaded report could silently show different data for the
  same account — the kind of bug that's hard to notice and worse to
  explain to a customer.

None of this is groundbreaking architecture — it's a small dashboard. The
discipline that matters here is narrow: one export path, one filter
representation, reused everywhere, so "the scheduled report doesn't match
what I see on screen" is a bug class that can't happen by construction.
