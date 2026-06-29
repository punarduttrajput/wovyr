//! Observability baseline for the Apex AI Platform.
//!
//! Implements the v0.2 core of the
//! [observability section](../../docs/14-observability/index.md): a Prometheus
//! [`Metrics`] registry (counters + histograms following the
//! `apex_<subsystem>_<name>_<unit>` [naming convention](../../docs/14-observability/metrics.md))
//! exposed in text exposition format, plus a [structured-logging](../../docs/14-observability/logging.md)
//! initializer.
//!
//! v0.2 slice scope: in-process metrics rendered at `/metrics`, JSON/text log init,
//! and (behind the `otlp` feature) OpenTelemetry export of **traces** (the `tracing`
//! spans emitted across the platform), **metrics** (the registry dual-writes to an
//! OTLP push exporter via [`Metrics::with_otlp_export`]), and **logs** (log events
//! bridged to OTLP) — all activated only when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.

mod logging;
mod metrics;
#[cfg(feature = "otlp")]
mod otlp;

pub use logging::{TelemetryGuard, init_logging};
pub use metrics::{DEFAULT_SECONDS_BUCKETS, Metrics};
