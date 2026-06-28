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
//! and (behind the `otlp` feature) OpenTelemetry **trace export** of the `tracing`
//! spans emitted across the platform. **Deferred:** exemplars and OTLP metrics/logs.

mod logging;
mod metrics;
#[cfg(feature = "otlp")]
mod otlp;

pub use logging::{TelemetryGuard, init_logging};
pub use metrics::{DEFAULT_SECONDS_BUCKETS, Metrics};
