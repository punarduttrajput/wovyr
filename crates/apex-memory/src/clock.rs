//! Wall-clock injection for the memory engine (RM-AIM-P2 RAG-205).
//!
//! The house rule keeps core logic clock-free: time is read only at the
//! engine's boundaries — ingestion stamps [`MemoryRecord::created_ms`]
//! (crate::MemoryRecord::created_ms), and a query reads "now" once for
//! recency decay and time-range filters — through this trait, so ranking
//! itself stays a pure function of its inputs. The same pattern as
//! `apex-workflow`'s `Clock`/`SystemClock`/`ManualClock`.

use std::sync::atomic::{AtomicU64, Ordering};

/// A source of wall-clock time in epoch milliseconds.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

/// The real system clock (the default).
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// A manually-driven clock for deterministic tests: starts at a fixed
/// instant and only moves when told to.
pub struct ManualClock(AtomicU64);

impl ManualClock {
    /// A clock frozen at `now_ms`.
    pub fn new(now_ms: u64) -> Self {
        Self(AtomicU64::new(now_ms))
    }

    /// Advance the clock by `delta_ms`.
    pub fn advance(&self, delta_ms: u64) {
        self.0.fetch_add(delta_ms, Ordering::SeqCst);
    }

    /// Jump the clock to an absolute instant.
    pub fn set(&self, now_ms: u64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_starts_fixed_and_advances_on_demand() {
        let clock = ManualClock::new(1_000);
        assert_eq!(clock.now_ms(), 1_000);
        clock.advance(500);
        assert_eq!(clock.now_ms(), 1_500);
        clock.set(10);
        assert_eq!(clock.now_ms(), 10);
    }

    #[test]
    fn system_clock_is_past_2020() {
        // Sanity only: a real epoch-milliseconds reading, not a stub zero.
        assert!(SystemClock.now_ms() > 1_577_836_800_000);
    }
}
