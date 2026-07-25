//! Structured logging + trace-export initialization.
//!
//! Implements the [logging standards](../../docs/14-observability/logging.md):
//! leveled logs to stderr, level configurable via `WOVYR_LOG` (falling back to
//! `RUST_LOG`), and structured **JSON** output when `WOVYR_LOG_FORMAT=json`. Default
//! is human-readable text at `warn` so normal CLI runs have clean output.
//!
//! When built with the `otlp` feature and `OTEL_EXPORTER_OTLP_ENDPOINT` is set, the
//! same `tracing` spans are additionally exported to an OTLP collector as **traces**,
//! and log events are bridged to OTLP **logs**
//! ([observability §traces](../../docs/14-observability/index.md)).

use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// Holds anything that must live for the process's telemetry lifetime — notably the
/// OTLP tracer provider, whose `Drop` flushes batched spans. Keep it bound until exit.
#[derive(Default)]
#[must_use = "keep the guard alive for the process lifetime so spans are flushed on exit"]
pub struct TelemetryGuard {
    #[cfg(feature = "otlp")]
    _otel: Option<crate::otlp::OtelGuard>,
    #[cfg(feature = "otlp")]
    _otel_logs: Option<crate::otlp::OtelLogGuard>,
}

/// Initialize the global logging subscriber (and OTLP export when configured). Safe
/// to call once at startup; returns a [`TelemetryGuard`] to hold until shutdown.
///
/// - Level: `WOVYR_LOG` → `RUST_LOG` → `warn`.
/// - Format: `WOVYR_LOG_FORMAT=json` for one-JSON-event-per-line, else text.
pub fn init_logging() -> TelemetryGuard {
    let filter = std::env::var("WOVYR_LOG")
        .ok()
        .and_then(|v| EnvFilter::try_new(v).ok())
        .or_else(|| EnvFilter::try_from_default_env().ok())
        .unwrap_or_else(|| EnvFilter::new("warn"));

    let json = std::env::var("WOVYR_LOG_FORMAT")
        .map(|v| v == "json")
        .unwrap_or(false);

    let fmt_layer = if json {
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(std::io::stderr)
            .boxed()
    } else {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .boxed()
    };

    // Build a layer stack over the bare registry; the filter applies to all layers.
    #[cfg_attr(not(feature = "otlp"), allow(unused_mut))]
    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![filter.boxed(), fmt_layer];

    #[cfg_attr(not(feature = "otlp"), allow(unused_mut))]
    let mut guard = TelemetryGuard::default();

    #[cfg(feature = "otlp")]
    if let Some((otel_layer, otel_guard)) = crate::otlp::layer::<Registry>("wovyr") {
        layers.push(otel_layer.boxed());
        guard._otel = Some(otel_guard);
    }

    #[cfg(feature = "otlp")]
    if let Some((logs_layer, logs_guard)) = crate::otlp::logs_layer::<Registry>("wovyr") {
        layers.push(logs_layer.boxed());
        guard._otel_logs = Some(logs_guard);
    }

    Registry::default().with(layers).init();
    guard
}
