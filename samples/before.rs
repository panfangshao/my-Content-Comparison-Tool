use std::collections::HashMap;

/// Aggregates request timings per route.
pub struct Metrics {
    counts: HashMap<String, u64>,
    total_ms: u64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            counts: HashMap::new(),
            total_ms: 0,
        }
    }

    pub fn record(&mut self, route: &str, ms: u64) {
        *self.counts.entry(route.to_string()).or_insert(0) += 1;
        self.total_ms += ms;
    }

    pub fn average(&self) -> f64 {
        let n: u64 = self.counts.values().sum();
        if n == 0 {
            return 0.0;
        }
        self.total_ms as f64 / n as f64
    }

    pub fn report(&self) -> String {
        let mut out = String::new();
        for (route, count) in &self.counts {
            out.push_str(&format!("{route}: {count}\n"));
        }
        out
    }
}
