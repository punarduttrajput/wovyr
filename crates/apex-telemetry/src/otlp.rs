//! OpenTelemetry OTLP export of traces, metrics, and logs.
//!
//! Builds batching OTLP exporters wired into the platform's `tracing` pipeline and
//! metrics registry, shipping spans, metrics, and log events to an OTLP collector
//! ([observability](../../docs/14-observability/index.md)). Enabled by the `otlp`
//! cargo feature and activated only when `OTEL_EXPORTER_OTLP_ENDPOINT` is set, so
//! default and unconfigured runs do nothing.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter, MeterProvider as _};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::LoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::runtime;
use opentelemetry_sdk::trace::TracerProvider;
use std::collections::BTreeMap;
use tracing_subscriber::Layer;
use tracing_subscriber::registry::LookupSpan;

/// The OTLP endpoint env var ([spec](https://opentelemetry.io/docs/specs/otel/protocol/exporter/)).
const ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

/// The OTLP resource describing this service (shared by traces/metrics/logs).
fn resource(service: &str) -> Resource {
    Resource::new(vec![KeyValue::new("service.name", service.to_string())])
}

/// The configured OTLP endpoint, or `None` when unset/empty (export disabled).
fn endpoint() -> Option<String> {
    std::env::var(ENDPOINT_ENV).ok().filter(|e| !e.is_empty())
}

/// Owns the tracer provider; flushes and shuts it down on drop so buffered spans are
/// not lost at exit.
pub struct OtelGuard {
    provider: TracerProvider,
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        // Best-effort flush of any batched spans.
        let _ = self.provider.shutdown();
    }
}

/// Build the OTLP tracing layer when `OTEL_EXPORTER_OTLP_ENDPOINT` is set, tagging
/// spans with `service.name = service`. Returns the layer plus a guard that must be
/// kept alive for the process lifetime. `None` when unconfigured or on setup error
/// (export is best-effort and never blocks startup).
pub fn layer<S>(service: &str) -> Option<(impl Layer<S>, OtelGuard)>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    let endpoint = endpoint()?;

    let exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&endpoint)
        .build()
    {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("otlp: failed to build span exporter for {endpoint}: {e}");
            return None;
        }
    };

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, runtime::Tokio)
        .with_resource(resource(service))
        .build();

    let tracer = provider.tracer("apex");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // Make the provider the global one (so `opentelemetry::global` APIs see it too).
    opentelemetry::global::set_tracer_provider(provider.clone());

    Some((layer, OtelGuard { provider }))
}

/// Owns the OTLP logger provider; flushes and shuts it down on drop so buffered log
/// records are not lost at exit.
pub struct OtelLogGuard {
    provider: LoggerProvider,
}

impl Drop for OtelLogGuard {
    fn drop(&mut self) {
        let _ = self.provider.shutdown();
    }
}

/// Build a `tracing` layer that bridges log events to an OTLP **logs** exporter when
/// `OTEL_EXPORTER_OTLP_ENDPOINT` is set. Returns the layer plus a guard to keep alive
/// for the process lifetime. `None` when unconfigured or on setup error.
pub fn logs_layer<S>(service: &str) -> Option<(impl Layer<S>, OtelLogGuard)>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    let endpoint = endpoint()?;

    let exporter = match opentelemetry_otlp::LogExporter::builder()
        .with_tonic()
        .with_endpoint(&endpoint)
        .build()
    {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("otlp: failed to build log exporter for {endpoint}: {e}");
            return None;
        }
    };

    let provider = LoggerProvider::builder()
        .with_batch_exporter(exporter, runtime::Tokio)
        .with_resource(resource(service))
        .build();

    let layer = OpenTelemetryTracingBridge::new(&provider);
    Some((layer, OtelLogGuard { provider }))
}

/// An OTLP **metrics** sink: a meter plus lazily-created instruments, fed by the
/// [`Metrics`](crate::Metrics) registry's record calls (a dual-write alongside the
/// in-process Prometheus aggregation). Owns the meter provider, whose periodic reader
/// pushes to the collector and whose `Drop` flushes on exit.
pub struct OtelMetrics {
    meter: Meter,
    counters: BTreeMap<String, Counter<f64>>,
    gauges: BTreeMap<String, Gauge<f64>>,
    histograms: BTreeMap<String, Histogram<f64>>,
    provider: SdkMeterProvider,
}

impl Drop for OtelMetrics {
    fn drop(&mut self) {
        let _ = self.provider.shutdown();
    }
}

impl OtelMetrics {
    /// Build the metrics provider + meter when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
    /// `None` when unconfigured or on setup error (export is best-effort).
    pub fn build(service: &str) -> Option<Self> {
        let endpoint = endpoint()?;

        let exporter = match opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(&endpoint)
            .build()
        {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("otlp: failed to build metric exporter for {endpoint}: {e}");
                return None;
            }
        };

        let reader = PeriodicReader::builder(exporter, runtime::Tokio).build();
        let provider = SdkMeterProvider::builder()
            .with_reader(reader)
            .with_resource(resource(service))
            .build();
        let meter = provider.meter("apex");

        Some(Self {
            meter,
            counters: BTreeMap::new(),
            gauges: BTreeMap::new(),
            histograms: BTreeMap::new(),
            provider,
        })
    }

    /// Record a counter delta into the matching OTLP instrument (created on first use).
    pub fn add_counter(&mut self, name: &str, value: f64, labels: &[(&str, &str)]) {
        let meter = &self.meter;
        let counter = self
            .counters
            .entry(name.to_string())
            .or_insert_with(|| meter.f64_counter(name.to_string()).build());
        counter.add(value, &to_attrs(labels));
    }

    /// Record a gauge sample into the matching OTLP instrument (OBS-301).
    pub fn set_gauge(&mut self, name: &str, value: f64, labels: &[(&str, &str)]) {
        let meter = &self.meter;
        let gauge = self
            .gauges
            .entry(name.to_string())
            .or_insert_with(|| meter.f64_gauge(name.to_string()).build());
        gauge.record(value, &to_attrs(labels));
    }

    /// Record a histogram observation into the matching OTLP instrument.
    pub fn record_histogram(&mut self, name: &str, value: f64, labels: &[(&str, &str)]) {
        let meter = &self.meter;
        let hist = self
            .histograms
            .entry(name.to_string())
            .or_insert_with(|| meter.f64_histogram(name.to_string()).build());
        hist.record(value, &to_attrs(labels));
    }
}

/// Map Prometheus-style `(key, value)` labels to OTLP attributes.
fn to_attrs(labels: &[(&str, &str)]) -> Vec<KeyValue> {
    labels
        .iter()
        .map(|(k, v)| KeyValue::new(k.to_string(), v.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::Registry;

    #[test]
    fn disabled_without_endpoint() {
        // An empty/unset endpoint disables export across all three signals (no
        // provider, no network). Kept in one test so the shared env var isn't raced.
        // Safety: edition-2024 env mutation; this test touches only its own var.
        unsafe { std::env::set_var(super::ENDPOINT_ENV, "") };
        assert!(
            super::layer::<Registry>("apex").is_none(),
            "trace export must be disabled when the OTLP endpoint is unset"
        );
        assert!(
            super::logs_layer::<Registry>("apex").is_none(),
            "log export must be disabled when the OTLP endpoint is unset"
        );
        assert!(
            super::OtelMetrics::build("apex").is_none(),
            "metric export must be disabled when the OTLP endpoint is unset"
        );
    }
}
