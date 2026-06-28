//! OpenTelemetry OTLP trace export.
//!
//! Builds a batching OTLP span exporter and a [`tracing_opentelemetry`] layer so the
//! `tracing` spans emitted across the platform are shipped to an OTLP collector
//! ([observability §traces](../../docs/14-observability/index.md)). Enabled by the
//! `otlp` cargo feature and activated only when `OTEL_EXPORTER_OTLP_ENDPOINT` is set,
//! so default and unconfigured runs do nothing.

use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::runtime;
use opentelemetry_sdk::trace::TracerProvider;
use tracing_subscriber::Layer;
use tracing_subscriber::registry::LookupSpan;

/// The OTLP endpoint env var ([spec](https://opentelemetry.io/docs/specs/otel/protocol/exporter/)).
const ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

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
    let endpoint = std::env::var(ENDPOINT_ENV).ok().filter(|e| !e.is_empty())?;

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

    let resource = Resource::new(vec![KeyValue::new("service.name", service.to_string())]);

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, runtime::Tokio)
        .with_resource(resource)
        .build();

    let tracer = provider.tracer("apex");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // Make the provider the global one (so `opentelemetry::global` APIs see it too).
    opentelemetry::global::set_tracer_provider(provider.clone());

    Some((layer, OtelGuard { provider }))
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::Registry;

    #[test]
    fn disabled_without_endpoint() {
        // An empty/unset endpoint disables export (no provider, no network).
        // Safety: edition-2024 env mutation; this test touches only its own var.
        unsafe { std::env::set_var(super::ENDPOINT_ENV, "") };
        assert!(
            super::layer::<Registry>("apex").is_none(),
            "export must be disabled when the OTLP endpoint is unset"
        );
    }
}
