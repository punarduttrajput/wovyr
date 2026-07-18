//! An in-process metrics registry that renders Prometheus text exposition.
//!
//! Follows the [metric taxonomy](../../docs/14-observability/metrics.md): counters
//! for traffic/errors, histograms for latency (the RED golden signals), gauges for
//! point-in-time depths (queue/backlog/DLQ sizes — OBS-301), and bounded labels
//! (no high-cardinality ids). Cloning a [`Metrics`] shares the same registry.

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
    /// metric name → (label-set string → point-in-time value), set not added
    gauges: BTreeMap<String, BTreeMap<String, f64>>,
    /// metric name → (label-set string → histogram)
    histograms: BTreeMap<String, BTreeMap<String, Hist>>,
    /// Optional OTLP metrics sink: when present, record calls are mirrored to it
    /// (a dual-write alongside the in-process Prometheus aggregation above).
    #[cfg(feature = "otlp")]
    otel: Option<crate::otlp::OtelMetrics>,
}

/// An OpenMetrics exemplar: a sample value tagged with the trace it came from, so a
/// latency bucket links back to a representative trace
/// ([metrics §8](../../docs/14-observability/metrics.md)).
#[derive(Clone)]
struct Exemplar {
    trace_id: String,
    value: f64,
    timestamp_s: f64,
}

struct Hist {
    /// Upper bounds; the implicit final bucket is `+Inf`.
    bounds: Vec<f64>,
    /// Per-bucket counts (length `bounds.len() + 1`; last is `+Inf`).
    counts: Vec<u64>,
    /// Most recent exemplar per bucket (same length as `counts`).
    exemplars: Vec<Option<Exemplar>>,
    sum: f64,
    count: u64,
}

impl Hist {
    fn new(bounds: &[f64]) -> Self {
        Self {
            bounds: bounds.to_vec(),
            counts: vec![0; bounds.len() + 1],
            exemplars: vec![None; bounds.len() + 1],
            sum: 0.0,
            count: 0,
        }
    }

    fn observe(&mut self, value: f64, exemplar: Option<Exemplar>) {
        let idx = self
            .bounds
            .iter()
            .position(|&b| value <= b)
            .unwrap_or(self.bounds.len());
        self.counts[idx] += 1;
        self.sum += value;
        self.count += 1;
        // Keep the latest exemplar for the bucket this sample fell into.
        if exemplar.is_some() {
            self.exemplars[idx] = exemplar;
        }
    }
}

impl Metrics {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a registry that, in addition to in-process Prometheus aggregation, also
    /// exports to an OTLP collector when built with the `otlp` feature and
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` is set (`service` tags `service.name`). Without
    /// the feature, or when unconfigured, this is equivalent to [`Self::new`]. Must be
    /// called inside a Tokio runtime (the exporter's periodic reader runs on it).
    pub fn with_otlp_export(service: &str) -> Self {
        let m = Self::new();
        #[cfg(feature = "otlp")]
        {
            if let Some(otel) = crate::otlp::OtelMetrics::build(service) {
                m.inner.lock().expect("metrics mutex poisoned").otel = Some(otel);
            }
        }
        let _ = service;
        m
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
        #[cfg(feature = "otlp")]
        if let Some(otel) = inner.otel.as_mut() {
            otel.add_counter(name, value, labels);
        }
    }

    /// Increment a counter by 1.
    pub fn counter_inc(&self, name: &str, labels: &[(&str, &str)]) {
        self.counter_add(name, labels, 1.0);
    }

    /// Set a gauge to a point-in-time value (OBS-301). Unlike counters, gauges are
    /// **set**, not accumulated — the natural fit for depths (queue length, DLQ
    /// size, in-flight runs) that the scrape handler recomputes from the source of
    /// truth on every render, so the exposed value can never drift from reality
    /// across restarts or missed updates.
    pub fn gauge_set(&self, name: &str, labels: &[(&str, &str)], value: f64) {
        let key = render_labels(labels);
        let mut inner = self.inner.lock().expect("metrics mutex poisoned");
        inner
            .gauges
            .entry(name.to_string())
            .or_default()
            .insert(key, value);
        #[cfg(feature = "otlp")]
        if let Some(otel) = inner.otel.as_mut() {
            otel.set_gauge(name, value, labels);
        }
    }

    /// Observe a value into a histogram (using the default seconds buckets),
    /// attaching a trace exemplar from the current span when one is active.
    pub fn histogram_observe(&self, name: &str, labels: &[(&str, &str)], value: f64) {
        self.histogram_observe_with_trace(name, labels, value, current_trace_id().as_deref());
    }

    /// Like [`Self::histogram_observe`] but with an explicit `trace_id` exemplar
    /// (`None` records no exemplar). Used when the caller knows the trace context.
    pub fn histogram_observe_with_trace(
        &self,
        name: &str,
        labels: &[(&str, &str)],
        value: f64,
        trace_id: Option<&str>,
    ) {
        let exemplar = trace_id.map(|tid| Exemplar {
            trace_id: tid.to_string(),
            value,
            timestamp_s: unix_seconds(),
        });
        let key = render_labels(labels);
        let mut inner = self.inner.lock().expect("metrics mutex poisoned");
        inner
            .histograms
            .entry(name.to_string())
            .or_default()
            .entry(key)
            .or_insert_with(|| Hist::new(DEFAULT_SECONDS_BUCKETS))
            .observe(value, exemplar);
        #[cfg(feature = "otlp")]
        if let Some(otel) = inner.otel.as_mut() {
            otel.record_histogram(name, value, labels);
        }
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

        for (name, series) in &inner.gauges {
            out.push_str(&format!("# TYPE {name} gauge\n"));
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

    /// Render in **OpenMetrics** text format — like [`Self::render_prometheus`] but
    /// histogram bucket lines carry trace **exemplars** and the body ends with the
    /// required `# EOF` ([metrics §8](../../docs/14-observability/metrics.md)). Serve
    /// with content type `application/openmetrics-text`.
    pub fn render_openmetrics(&self) -> String {
        let inner = self.inner.lock().expect("metrics mutex poisoned");
        let mut out = String::new();

        for (name, series) in &inner.counters {
            out.push_str(&format!("# TYPE {name} counter\n"));
            for (labels, value) in series {
                out.push_str(&format!("{name}{labels} {}\n", trim_float(*value)));
            }
        }

        for (name, series) in &inner.gauges {
            out.push_str(&format!("# TYPE {name} gauge\n"));
            for (labels, value) in series {
                out.push_str(&format!("{name}{labels} {}\n", trim_float(*value)));
            }
        }

        for (name, series) in &inner.histograms {
            out.push_str(&format!("# TYPE {name} histogram\n"));
            for (labels, hist) in series {
                let mut cumulative = 0u64;
                for i in 0..=hist.bounds.len() {
                    cumulative += hist.counts[i];
                    let le = if i < hist.bounds.len() {
                        trim_float(hist.bounds[i])
                    } else {
                        "+Inf".to_string()
                    };
                    let line = format!(
                        "{name}_bucket{} {cumulative}",
                        with_label(labels, "le", &le)
                    );
                    out.push_str(&line);
                    if let Some(ex) = &hist.exemplars[i] {
                        // OpenMetrics exemplar: ` # {trace_id="…"} <value> <timestamp>`.
                        out.push_str(&format!(
                            " # {{trace_id=\"{}\"}} {} {}",
                            escape(&ex.trace_id),
                            trim_float(ex.value),
                            trim_float(ex.timestamp_s)
                        ));
                    }
                    out.push('\n');
                }
                out.push_str(&format!("{name}_sum{labels} {}\n", trim_float(hist.sum)));
                out.push_str(&format!("{name}_count{labels} {}\n", hist.count));
            }
        }

        out.push_str("# EOF\n");
        out
    }
}

/// Current span's trace id (32-hex) when OTLP tracing is active, else `None`.
#[cfg(feature = "otlp")]
fn current_trace_id() -> Option<String> {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let context = tracing::Span::current().context();
    let span = context.span();
    let sc = span.span_context();
    sc.is_valid().then(|| sc.trace_id().to_string())
}

#[cfg(not(feature = "otlp"))]
fn current_trace_id() -> Option<String> {
    None
}

/// Seconds since the Unix epoch (for exemplar timestamps).
fn unix_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
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
    fn gauges_set_not_accumulate_and_render_in_both_formats() {
        let m = Metrics::new();
        m.gauge_set("apex_webhook_dlq_size", &[], 3.0);
        // A later set replaces the value — gauges are point-in-time, not summed.
        m.gauge_set("apex_webhook_dlq_size", &[], 1.0);
        m.gauge_set("apex_workflow_timers_pending", &[("shard", "0")], 7.0);

        let prom = m.render_prometheus();
        assert!(
            prom.contains("# TYPE apex_webhook_dlq_size gauge"),
            "{prom}"
        );
        assert!(prom.contains("apex_webhook_dlq_size 1\n"), "{prom}");
        assert!(
            prom.contains(r#"apex_workflow_timers_pending{shard="0"} 7"#),
            "{prom}"
        );

        let om = m.render_openmetrics();
        assert!(om.contains("# TYPE apex_webhook_dlq_size gauge"), "{om}");
        assert!(om.trim_end().ends_with("# EOF"), "{om}");
    }

    #[test]
    fn shared_clone_sees_writes() {
        let m = Metrics::new();
        let m2 = m.clone();
        m2.counter_add("apex_llm_cost_usd_total", &[("model", "x")], 0.5);
        assert!(m.render_prometheus().contains("apex_llm_cost_usd_total"));
    }

    #[test]
    fn openmetrics_attaches_exemplar_to_the_observed_bucket() {
        let m = Metrics::new();
        m.histogram_observe_with_trace(
            "apex_api_request_duration_seconds",
            &[("route", "run")],
            0.03,
            Some("abc123def456"),
        );
        let out = m.render_openmetrics();

        // The exemplar rides the bucket the sample fell into (0.05), not earlier ones.
        let bucket_line = out
            .lines()
            .find(|l| l.contains(r#"le="0.05""#))
            .expect("0.05 bucket line");
        assert!(
            bucket_line.contains(r#"# {trace_id="abc123def456"} 0.03"#),
            "exemplar missing on observed bucket: {bucket_line}"
        );
        // An earlier (empty) bucket carries no exemplar.
        let empty_bucket = out
            .lines()
            .find(|l| l.contains(r#"le="0.025""#))
            .expect("0.025 bucket line");
        assert!(!empty_bucket.contains("trace_id"), "got: {empty_bucket}");
        // OpenMetrics requires the EOF terminator.
        assert!(out.trim_end().ends_with("# EOF"), "got:\n{out}");
    }

    #[test]
    fn with_otlp_export_still_aggregates_in_process() {
        // With no endpoint configured, `with_otlp_export` behaves like `new` — the
        // in-process Prometheus aggregation is unaffected (the OTLP sink is inert),
        // so `/metrics` keeps working whether or not export is on.
        let m = Metrics::with_otlp_export("apex");
        m.counter_inc("apex_api_requests_total", &[("route", "run")]);
        m.histogram_observe(
            "apex_api_request_duration_seconds",
            &[("route", "run")],
            0.03,
        );
        let out = m.render_prometheus();
        assert!(
            out.contains("apex_api_requests_total{route=\"run\"} 1"),
            "got:\n{out}"
        );
        assert!(
            out.contains("apex_api_request_duration_seconds_count{route=\"run\"} 1"),
            "got:\n{out}"
        );
    }

    #[test]
    fn no_exemplar_without_trace_context() {
        let m = Metrics::new();
        m.histogram_observe_with_trace("apex_api_request_duration_seconds", &[], 0.03, None);
        let out = m.render_openmetrics();
        assert!(!out.contains("trace_id"), "no trace → no exemplar:\n{out}");
        // Classic Prometheus render is unchanged (no exemplars, no EOF).
        assert!(!m.render_prometheus().contains("# EOF"));
    }
}
