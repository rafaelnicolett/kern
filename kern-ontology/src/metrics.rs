//! Fallback rate for `judge()` — the project's North Star KPI. Exposed as
//! a queryable metric, not just logged.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct FallbackMetrics {
    total: AtomicU64,
    fallback: AtomicU64,
}

impl FallbackMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a processed candidate — `was_fallback` is `true` only when
    /// the candidate fell into the ambiguous zone and `judge()` was
    /// called.
    pub fn record(&self, was_fallback: bool) {
        self.total.fetch_add(1, Ordering::Relaxed);
        if was_fallback {
            self.fallback.fetch_add(1, Ordering::Relaxed);
        }
        tracing::info!(
            target: "kern.ontology.fallback_rate",
            fallback = was_fallback,
            total = self.total.load(Ordering::Relaxed),
            fallback_total = self.fallback.load(Ordering::Relaxed),
            rate = self.fallback_rate(),
            "candidate processed"
        );
    }

    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    pub fn fallback_total(&self) -> u64 {
        self.fallback.load(Ordering::Relaxed)
    }

    /// `0.0` if nothing has been processed yet — never `NaN`.
    pub fn fallback_rate(&self) -> f64 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        self.fallback.load(Ordering::Relaxed) as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_starts_at_zero_without_processing_anything() {
        let m = FallbackMetrics::new();
        assert_eq!(m.fallback_rate(), 0.0);
        assert_eq!(m.total(), 0);
    }

    #[test]
    fn rate_reflects_the_fallback_proportion() {
        let m = FallbackMetrics::new();
        m.record(false);
        m.record(false);
        m.record(true);
        m.record(false);

        assert_eq!(m.total(), 4);
        assert_eq!(m.fallback_total(), 1);
        assert!((m.fallback_rate() - 0.25).abs() < f64::EPSILON);
    }
}
