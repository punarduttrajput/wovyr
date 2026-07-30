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
//!
//! The level filter applies to **every** layer, OTLP export included. That is the
//! documented intent, but note the consequence for an OTLP deployment: the
//! instrumented hot-path spans (`agent.run`, `gateway.chat`, `workflow.activity`,
//! `api.*`) are `INFO`-level, so at the `warn` default they are filtered out and
//! nothing is exported. Set `WOVYR_LOG=info` (or a targeted directive such as
//! `warn,wovyr_agent=info,wovyr_provider=info`) when exporting traces. Until the
//! filter-composition fix this was moot — the filter was inert, so OTLP received
//! everything down to `TRACE`, including the `hyper` connection-pool firehose.

use tracing_subscriber::layer::Layered;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// The output layers (fmt, plus the OTLP trace/log layers when enabled), erased so
/// they can be collected before the filter is composed over them.
type OutputLayers = Vec<Box<dyn Layer<Registry> + Send + Sync>>;

/// The composed subscriber: the level filter wrapped **around** the output layers.
type Composed = Layered<EnvFilter, Layered<OutputLayers, Registry>>;

/// Compose the level filter with the output layers.
///
/// The filter has to be a distinct [`Layered`] step, **not** another element of
/// `layers` — a filter pushed into the vector is silently inert, which is how
/// `WOVYR_LOG` came to have no effect at all and every binary logged at `TRACE`
/// (including the whole `hyper` connection-pool firehose). `Layer for Vec<L>`
/// combines `register_callsite` by returning the *highest* interest across its
/// elements, and `fmt::Layer` uses the default `register_callsite`, which returns
/// `Interest::always()`. The combined interest is therefore `always` for every
/// callsite, `tracing` caches it as unconditionally enabled, and the vector's
/// `enabled()` — an `all()` that would have correctly said no — is never consulted.
/// [`Layered`]'s own `pick_interest` instead short-circuits to `never` as soon as the
/// outer layer says `never`, so the filter is honored. Kept as a named function so the
/// regression test below exercises the real production composition rather than a
/// look-alike rebuilt in the test.
fn compose(filter: EnvFilter, layers: OutputLayers) -> Composed {
    Registry::default().with(layers).with(filter)
}

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

/// The level applied when neither `WOVYR_LOG` nor `RUST_LOG` says otherwise, so a
/// normal CLI run has clean output.
const DEFAULT_FILTER: &str = "warn";

/// Resolve the level filter: `WOVYR_LOG` → `RUST_LOG` → [`DEFAULT_FILTER`].
///
/// Takes the values rather than reading the environment itself so the precedence is
/// unit-testable without mutating process-global environment variables (a shared
/// global no concurrently-running test could safely rewrite). An unparseable
/// directive falls through to the next source instead of failing the process — a
/// typo in an operator's `WOVYR_LOG` shouldn't stop a binary from starting.
///
/// Note that `EnvFilter` accepts far more than level names: a bare word it doesn't
/// recognize as a level parses as a *target* directive at `trace` (`WOVYR_LOG=Warning`
/// becomes `Warning=trace`, i.e. a firehose, not an error), so only a directive whose
/// level is unrecognized (`app=nonsense`) actually falls through here.
fn resolve_filter(explicit: Option<&str>, fallback: Option<&str>) -> EnvFilter {
    explicit
        .and_then(|v| EnvFilter::try_new(v).ok())
        .or_else(|| fallback.and_then(|v| EnvFilter::try_new(v).ok()))
        .unwrap_or_else(|| EnvFilter::new(DEFAULT_FILTER))
}

/// Initialize the global logging subscriber (and OTLP export when configured). Safe
/// to call once at startup; returns a [`TelemetryGuard`] to hold until shutdown.
///
/// - Level: `WOVYR_LOG` → `RUST_LOG` → `warn`.
/// - Format: `WOVYR_LOG_FORMAT=json` for one-JSON-event-per-line, else text.
pub fn init_logging() -> TelemetryGuard {
    let filter = resolve_filter(
        std::env::var("WOVYR_LOG").ok().as_deref(),
        std::env::var("RUST_LOG").ok().as_deref(),
    );

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

    // Collect the output layers; `compose` puts the filter around them (see its
    // doc comment — the filter must not be an element of this vector).
    #[cfg_attr(not(feature = "otlp"), allow(unused_mut))]
    let mut layers: OutputLayers = vec![fmt_layer];

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

    compose(filter, layers).init();
    guard
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    /// A writer that appends everything written to it to a shared buffer, so a test
    /// can assert on what the fmt layer actually emitted.
    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn contents(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("buffer lock")).into_owned()
        }
    }

    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("buffer lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedBuf {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// The regression test for the inert-filter bug: `WOVYR_LOG` had no effect
    /// because the filter was composed as an element of the layer vector, so events
    /// below the configured level were still written. Drives the real [`compose`]
    /// with a scoped subscriber ([`tracing::subscriber::with_default`]) rather than
    /// the process-global `init()`, which can only be called once per process.
    #[test]
    fn the_filter_actually_filters_the_composed_stack() {
        let buf = SharedBuf::default();
        let layers: OutputLayers = vec![
            tracing_subscriber::fmt::layer()
                .with_writer(buf.clone())
                .boxed(),
        ];

        tracing::subscriber::with_default(compose(EnvFilter::new("warn"), layers), || {
            tracing::trace!("trace-must-not-appear");
            tracing::debug!("debug-must-not-appear");
            tracing::info!("info-must-not-appear");
            tracing::warn!("warn-must-appear");
            tracing::error!("error-must-appear");
        });

        let out = buf.contents();
        assert!(
            !out.contains("must-not-appear"),
            "events below the configured level were emitted — the filter is inert: {out}"
        );
        assert!(
            out.contains("warn-must-appear") && out.contains("error-must-appear"),
            "events at/above the configured level were dropped: {out}"
        );
    }

    /// The documented precedence, asserted on the resolved filter's own directive
    /// string: `WOVYR_LOG` wins, `RUST_LOG` is the fallback, `warn` is the default,
    /// and an unparseable directive falls through rather than taking the process down.
    #[test]
    fn the_filter_resolves_wovyr_log_then_rust_log_then_warn() {
        assert_eq!(
            resolve_filter(Some("debug"), Some("error")).to_string(),
            "debug"
        );
        assert_eq!(resolve_filter(None, Some("error")).to_string(), "error");
        assert_eq!(resolve_filter(None, None).to_string(), DEFAULT_FILTER);
        // Only an unrecognized *level* fails to parse; see `resolve_filter`'s note.
        assert_eq!(
            resolve_filter(Some("app=nonsense"), Some("error")).to_string(),
            "error",
            "an unparseable WOVYR_LOG should fall through to RUST_LOG"
        );
        assert_eq!(
            resolve_filter(Some("app=nonsense"), None).to_string(),
            DEFAULT_FILTER
        );
    }
}
