//! Structured logging initialization.
//!
//! Implements the [logging standards](../../docs/14-observability/logging.md):
//! leveled logs to stderr, level configurable via `APEX_LOG` (falling back to
//! `RUST_LOG`), and structured **JSON** output when `APEX_LOG_FORMAT=json`. Default
//! is human-readable text at `warn` so normal CLI runs have clean output.

use tracing_subscriber::EnvFilter;

/// Initialize the global logging subscriber. Safe to call once at startup.
///
/// - Level: `APEX_LOG` → `RUST_LOG` → `warn`.
/// - Format: `APEX_LOG_FORMAT=json` for one-JSON-event-per-line, else text.
pub fn init_logging() {
    let filter = std::env::var("APEX_LOG")
        .ok()
        .and_then(|v| EnvFilter::try_new(v).ok())
        .or_else(|| EnvFilter::try_from_default_env().ok())
        .unwrap_or_else(|| EnvFilter::new("warn"));

    let json = std::env::var("APEX_LOG_FORMAT")
        .map(|v| v == "json")
        .unwrap_or(false);

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);

    if json {
        builder.json().init();
    } else {
        builder.init();
    }
}
