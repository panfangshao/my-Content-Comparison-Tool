use std::collections::BTreeMap;

/// Aggregates request timings per route.
///
/// Uses a BTreeMap so the report comes out in a stable order.
pub struct Metrics {
    counts: BTreeMap<String, u64>,
    total_ms: u64,
    slowest: u64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            counts: BTreeMap::new(),
            total_ms: 0,
            slowest: 0,
        }
    }

    pub fn record(&mut self, route: &str, ms: u64) {
        *self.counts.entry(route.to_string()).or_insert(0) += 1;
        self.total_ms += ms;
        self.slowest = self.slowest.max(ms);
    }

    pub fn average(&self) -> f64 {
        let n: u64 = self.counts.values().sum();
        if n == 0 {
            return 0.0;
        }
        self.total_ms as f64 / n as f64
    }

    pub fn slowest(&self) -> u64 {
        self.slowest
    }

    pub fn report(&self) -> String {
        let mut out = String::new();
        for (route, count) in &self.counts {
            out.push_str(&format!("{route:<24} {count:>6}\n"));
        }
        out
    }
}
