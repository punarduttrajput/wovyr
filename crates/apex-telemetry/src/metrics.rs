//! An in-process metrics registry that renders Prometheus text exposition.
//!
//! Follows the [metric taxonomy](../../docs/14-observability/metrics.md): counters
//! for traffic/errors, histograms for latency (the RED golden signals), and bounded
//! labels (no high-cardinality ids). Cloning a [`Metrics`] shares the same registry.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Default histogram buckets (seconds), matching typical Prometheus latency buckets.
pub const DEFAULT_SECONDS_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// A shareable metrics registry.
#[derive(Clone, Default)]
pub struct Metrics {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    /// metric name → (label-set string → value)
    counters: BTreeMap<String, BTreeMap<String, f64>>,
    /// metric name → (label-set string → histogram)
    histograms: BTreeMap<String, BTreeMap<String, Hist>>,
}

struct Hist {
    /// Upper bounds; the implicit final bucket is `+Inf`.
    bounds: Vec<f64>,
    /// Per-bucket counts (length `bounds.len() + 1`; last is `+Inf`).
    counts: Vec<u64>,
    sum: f64,
    count: u64,
}

impl Hist {
    fn new(bounds: &[f64]) -> Self {
        Self {
            bounds: bounds.to_vec(),
            counts: vec![0; bounds.len() + 1],
            sum: 0.0,
            count: 0,
        }
    }

    fn observe(&mut self, value: f64) {
        let idx = self
            .bounds
            .iter()
            .position(|&b| value <= b)
            .unwrap_or(self.bounds.len());
        self.counts[idx] += 1;
        self.sum += value;
        self.count += 1;
    }
}

impl Metrics {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment a counter by `value`.
    pub fn counter_add(&self, name: &str, labels: &[(&str, &str)], value: f64) {
        let key = render_labels(labels);
        let mut inner = self.inner.lock().expect("metrics mutex poisoned");
        *inner
            .counters
            .entry(name.to_string())
            .or_default()
            .entry(key)
            .or_insert(0.0) += value;
    }

    /// Increment a counter by 1.
    pub fn counter_inc(&self, name: &str, labels: &[(&str, &str)]) {
        self.counter_add(name, labels, 1.0);
    }

    /// Observe a value into a histogram (using the default seconds buckets).
    pub fn histogram_observe(&self, name: &str, labels: &[(&str, &str)], value: f64) {
        let key = render_labels(labels);
        let mut inner = self.inner.lock().expect("metrics mutex poisoned");
        inner
            .histograms
            .entry(name.to_string())
            .or_default()
            .entry(key)
            .or_insert_with(|| Hist::new(DEFAULT_SECONDS_BUCKETS))
            .observe(value);
    }

    /// Render the registry in Prometheus text exposition format.
    pub fn render_prometheus(&self) -> String {
        let inner = self.inner.lock().expect("metrics mutex poisoned");
        let mut out = String::new();

        for (name, series) in &inner.counters {
            out.push_str(&format!("# TYPE {name} counter\n"));
            for (labels, value) in series {
                out.push_str(&format!("{name}{labels} {}\n", trim_float(*value)));
            }
        }

        for (name, series) in &inner.histograms {
            out.push_str(&format!("# TYPE {name} histogram\n"));
            for (labels, hist) in series {
                let mut cumulative = 0u64;
                for (i, bound) in hist.bounds.iter().enumerate() {
                    cumulative += hist.counts[i];
                    out.push_str(&format!(
                        "{name}_bucket{} {cumulative}\n",
                        with_label(labels, "le", &trim_float(*bound))
                    ));
                }
                cumulative += hist.counts[hist.bounds.len()];
                out.push_str(&format!(
                    "{name}_bucket{} {cumulative}\n",
                    with_label(labels, "le", "+Inf")
                ));
                out.push_str(&format!("{name}_sum{labels} {}\n", trim_float(hist.sum)));
                out.push_str(&format!("{name}_count{labels} {}\n", hist.count));
            }
        }

        out
    }
}

/// Render a label set as `{k="v",k2="v2"}` (sorted by key), or `""` if empty.
fn render_labels(labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<(&str, &str)> = labels.to_vec();
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    let body = pairs
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", escape(v)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

/// Insert an extra label (e.g. `le`) into an already-rendered label string.
fn with_label(rendered: &str, key: &str, value: &str) -> String {
    let extra = format!("{key}=\"{value}\"");
    if rendered.is_empty() {
        format!("{{{extra}}}")
    } else {
        // rendered is `{...}`; splice the extra label in before the closing brace.
        format!(
            "{}{}{}",
            &rendered[..rendered.len() - 1],
            format_args!(",{extra}"),
            "}"
        )
    }
}

/// Escape a label value for Prometheus exposition.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Trim a float to a compact representation (integers print without a decimal).
fn trim_float(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_render_with_labels() {
        let m = Metrics::new();
        m.counter_inc(
            "apex_api_requests_total",
            &[("route", "run"), ("status", "200")],
        );
        m.counter_inc(
            "apex_api_requests_total",
            &[("route", "run"), ("status", "200")],
        );
        let out = m.render_prometheus();
        assert!(out.contains("# TYPE apex_api_requests_total counter"));
        assert!(
            out.contains(r#"apex_api_requests_total{route="run",status="200"} 2"#),
            "got:\n{out}"
        );
    }

    #[test]
    fn histogram_renders_buckets_sum_count() {
        let m = Metrics::new();
        m.histogram_observe(
            "apex_api_request_duration_seconds",
            &[("route", "run")],
            0.03,
        );
        let out = m.render_prometheus();
        assert!(out.contains("# TYPE apex_api_request_duration_seconds histogram"));
        assert!(out.contains(r#"le="+Inf"} 1"#), "got:\n{out}");
        assert!(out.contains("apex_api_request_duration_seconds_count{route=\"run\"} 1"));
        // 0.03 falls in the 0.05 bucket but not 0.025.
        assert!(out.contains(r#"le="0.05"} 1"#), "got:\n{out}");
        assert!(out.contains(r#"le="0.025"} 0"#), "got:\n{out}");
    }

    #[test]
    fn shared_clone_sees_writes() {
        let m = Metrics::new();
        let m2 = m.clone();
        m2.counter_add("apex_llm_cost_usd_total", &[("model", "x")], 0.5);
        assert!(m.render_prometheus().contains("apex_llm_cost_usd_total"));
    }
}
