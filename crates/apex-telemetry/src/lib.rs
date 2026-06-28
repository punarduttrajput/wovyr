//! Observability baseline for the Apex AI Platform.
//!
//! Implements the v0.2 core of the
//! [observability section](../../docs/14-observability/index.md): a Prometheus
//! [`Metrics`] registry (counters + histograms following the
//! `apex_<subsystem>_<name>_<unit>` [naming convention](../../docs/14-observability/metrics.md))
//! exposed in text exposition format, plus a [structured-logging](../../docs/14-observability/logging.md)
//! initializer.
//!
//! v0.2 slice scope: in-process metrics rendered at `/metrics`, and JSON/text log
//! init. **Deferred:** OpenTelemetry trace export (OTLP), exemplars, and a push
//! pipeline — the `tracing` spans are already emitted, just not exported yet.

mod logging;
mod metrics;

pub use logging::init_logging;
pub use metrics::{DEFAULT_SECONDS_BUCKETS, Metrics};
