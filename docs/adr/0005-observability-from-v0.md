# ADR-0005 — Observability: Structured From v0

> Status: **accepted** (see implementation note below — scope narrowed from the original decision)

## Context

The project's North Star KPI is the **fallback rate to `judge()`** — without
a real, queryable metric, that rate stays stuck in unstructured logs and the
project's central thesis can't be validated rigorously.

## Decision

**Structured observability from the first commit**, not a later addition:

- **Traces**: one span per use case (`process_candidate`, `query_ontological`
  routing, promotion to canonical, etc.), with attributes including the
  bounded context and project id.
- **Metrics**: `kern.ontology.fallback_rate` (the metric that decides
  whether the thesis holds) exposed per processing round via `FallbackMetrics`
  — not just logged. Complementary: entity/relation type counts, instance
  counts.
- **Logs**: structured, never free-form prose — each domain event becomes a
  structured log entry named after the event.

> **Implementation note (added after the fact)**: this ADR originally
> specified full OpenTelemetry SDK integration (trace/metric export to a
> backend like Jaeger or Prometheus). What's actually implemented today is
> structured logging and spans via the `tracing` crate (`tracing::instrument`,
> structured fields, `FallbackMetrics` as a queryable in-process counter) —
> real and genuinely structured, but **not** wired to an OpenTelemetry
> exporter. Exporting to an external backend remains future work, not done
> in v0.

## Consequences

### Positive
- The metric that validates (or kills) the project's thesis is first-class data, not a manual log-analysis artifact.

### Negative
- Instrumentation overhead on each use case — accepted, small compared to the cost of not being able to measure the central thesis.

### Neutral
- There's no requirement to export to a specific backend in v0 — the
  structured span/metric/log itself is the deliverable; where it gets
  exported to (Jaeger, Prometheus, stdout) is a configuration detail, not
  something this ADR decides.
