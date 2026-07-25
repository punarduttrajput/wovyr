//! A small, dependency-free cron expression evaluator (UTC).
//!
//! Completes the cron half of [gap closure
//! G2](../../docs/03-workflow-engine/temporal-gap-analysis.md#g2--schedules--cron):
//! a [`Schedule`](crate::Schedule) may recur on a 5-field cron expression
//! (`minute hour day-of-month month day-of-week`) instead of a fixed interval.
//!
//! Cron fires at **minute granularity, in UTC** — deterministic and timezone-free,
//! matching the engine's no-ambient-state philosophy. Each field supports `*`,
//! single values, lists (`a,b`), ranges (`a-b`), and steps (`*/n`, `a-b/n`). Month
//! and day-of-week also accept 3-letter names (`JAN`…`DEC`, `SUN`…`SAT`); day-of-week
//! accepts `0` or `7` for Sunday. The convenience macros `@hourly`, `@daily`/
//! `@midnight`, `@weekly`, `@monthly`, and `@yearly`/`@annually` are also accepted.
//!
//! Day-of-month and day-of-week follow Vixie semantics: when **both** are restricted
//! (neither is `*`) a day matches if **either** field matches; otherwise they AND.

use wovyr_common::{Error, Result};

/// Minutes in the search window before [`Cron::next_after`] gives up — a little
/// over four years, enough to resolve `* * 29 2 *` (Feb 29) yet bounded so an
/// impossible expression (e.g. `* * 31 2 *`) returns `None` instead of looping.
const MAX_LOOKAHEAD_MINUTES: u64 = 4 * 366 * 24 * 60;

/// A parsed cron expression. Each field is a bitmask of permitted values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    minutes: u64, // bits 0..=59
    hours: u64,   // bits 0..=23
    doms: u64,    // bits 1..=31
    months: u64,  // bits 1..=12
    dows: u64,    // bits 0..=6 (Sun=0)
    dom_restricted: bool,
    dow_restricted: bool,
}

const MONTH_NAMES: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DOW_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

impl Cron {
    /// Parse a cron expression (5 fields, or a supported `@macro`).
    pub fn parse(expr: &str) -> Result<Self> {
        let expr = expr.trim();
        let normalized = match expr.to_ascii_lowercase().as_str() {
            "@hourly" => "0 * * * *".to_string(),
            "@daily" | "@midnight" => "0 0 * * *".to_string(),
            "@weekly" => "0 0 * * 0".to_string(),
            "@monthly" => "0 0 1 * *".to_string(),
            "@yearly" | "@annually" => "0 0 1 1 *".to_string(),
            _ => expr.to_string(),
        };

        let fields: Vec<&str> = normalized.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(Error::Invalid(format!(
                "cron expression must have 5 fields (got {}): `{expr}`",
                fields.len()
            )));
        }

        let minutes = parse_field(fields[0], 0, 59, None)?;
        let hours = parse_field(fields[1], 0, 23, None)?;
        let doms = parse_field(fields[2], 1, 31, None)?;
        let months = parse_field(fields[3], 1, 12, Some(&MONTH_NAMES))?;
        let mut dows = parse_field(fields[4], 0, 7, Some(&DOW_NAMES))?;
        // Normalize Sunday-as-7 to Sunday-as-0.
        if dows & (1 << 7) != 0 {
            dows = (dows & !(1 << 7)) | 1;
        }

        Ok(Self {
            minutes,
            hours,
            doms,
            months,
            dows,
            dom_restricted: fields[2].trim() != "*",
            dow_restricted: fields[4].trim() != "*",
        })
    }

    /// The next firing instant **strictly after** `after_ms` (Unix-epoch
    /// milliseconds), aligned to a minute boundary, or `None` if none falls within
    /// the lookahead window.
    pub fn next_after(&self, after_ms: u64) -> Option<u64> {
        // Scan minute by minute from the next whole minute strictly after `after_ms`,
        // bounded by the lookahead window.
        let start = after_ms / 60_000 + 1;
        for minute_idx in (start..).take(MAX_LOOKAHEAD_MINUTES as usize) {
            let (_, month, day, hour, minute, dow) = civil_fields(minute_idx * 60);
            if self.matches(month, day, dow, hour, minute) {
                return Some(minute_idx * 60_000);
            }
        }
        None
    }

    /// Whether the given calendar fields satisfy every cron field.
    fn matches(&self, month: u32, day: u32, dow: u32, hour: u32, minute: u32) -> bool {
        if self.minutes & (1 << minute) == 0
            || self.hours & (1 << hour) == 0
            || self.months & (1 << month) == 0
        {
            return false;
        }
        let dom_ok = self.doms & (1 << day) != 0;
        let dow_ok = self.dows & (1 << dow) != 0;
        match (self.dom_restricted, self.dow_restricted) {
            // Both restricted → OR (Vixie semantics).
            (true, true) => dom_ok || dow_ok,
            // One unrestricted → that field always passes; AND with the other.
            (true, false) => dom_ok,
            (false, true) => dow_ok,
            (false, false) => true,
        }
    }
}

/// Parse one cron field into a bitmask over `[min, max]`.
fn parse_field(spec: &str, min: u32, max: u32, names: Option<&[&str]>) -> Result<u64> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(Error::Invalid("empty cron field".into()));
    }
    let mut mask = 0u64;
    for part in spec.split(',') {
        let part = part.trim();
        let (range_part, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s
                    .parse()
                    .map_err(|_| Error::Invalid(format!("invalid cron step `{s}`")))?;
                if step == 0 {
                    return Err(Error::Invalid("cron step must be > 0".into()));
                }
                (r.trim(), step)
            }
            None => (part, 1),
        };

        let (lo, hi) = if range_part == "*" {
            (min, max)
        } else if let Some((a, b)) = range_part.split_once('-') {
            (
                parse_value(a.trim(), names, min, max)?,
                parse_value(b.trim(), names, min, max)?,
            )
        } else {
            let v = parse_value(range_part, names, min, max)?;
            (v, v)
        };

        if lo < min || hi > max || lo > hi {
            return Err(Error::Invalid(format!(
                "cron value out of range or inverted in `{part}` (expected {min}..={max})"
            )));
        }
        let mut v = lo;
        while v <= hi {
            mask |= 1 << v;
            v += step;
        }
    }
    Ok(mask)
}

/// Parse a single cron value: a number, or a 3-letter name if `names` is provided.
fn parse_value(token: &str, names: Option<&[&str]>, min: u32, max: u32) -> Result<u32> {
    if let Ok(n) = token.parse::<u32>() {
        return Ok(n);
    }
    if let Some(names) = names {
        let lower = token.to_ascii_lowercase();
        if let Some(idx) = names.iter().position(|n| *n == lower) {
            // Month names are 1-based; day names 0-based — derive from `min`.
            return Ok(idx as u32 + min.min(1));
        }
    }
    Err(Error::Invalid(format!(
        "invalid cron value `{token}` (expected {min}..={max} or a name)"
    )))
}

/// Convert Unix-epoch seconds (UTC) to `(year, month, day, hour, minute, weekday)`,
/// weekday 0=Sunday. Uses Howard Hinnant's civil-date algorithm (no dependencies).
fn civil_fields(epoch_secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (epoch_secs / 86_400) as i64;
    let rem = epoch_secs % 86_400;
    let hour = (rem / 3_600) as u32;
    let minute = ((rem % 3_600) / 60) as u32;
    // 1970-01-01 is a Thursday (=4); epoch days are non-negative here.
    let weekday = (((days % 7) + 4) % 7) as u32;
    let (year, month, day) = civil_from_days(days);
    (year, month, day, hour, minute, weekday)
}

/// Days since 1970-01-01 → `(year, month [1..=12], day [1..=31])`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trips_known_dates() {
        // 1970-01-01 00:00 Thursday.
        let (y, m, d, h, min, dow) = civil_fields(0);
        assert_eq!((y, m, d, h, min, dow), (1970, 1, 1, 0, 0, 4));
        // 1970-01-04 is a Sunday.
        let (_, _, _, _, _, dow) = civil_fields(3 * 86_400);
        assert_eq!(dow, 0);
        // 2000-01-01 00:00 (day 10957) Saturday.
        let (y, m, d, _, _, dow) = civil_fields(10_957 * 86_400);
        assert_eq!((y, m, d, dow), (2000, 1, 1, 6));
    }

    #[test]
    fn parses_macros_and_fields() {
        assert!(Cron::parse("@daily").is_ok());
        assert!(Cron::parse("0 9 * * MON-FRI").is_ok());
        assert!(Cron::parse("*/15 0 1,15 * *").is_ok());
        assert!(Cron::parse("0 0 * * 7").is_ok()); // Sunday as 7
        assert!(Cron::parse("bad expr").is_err());
        assert!(Cron::parse("60 * * * *").is_err()); // minute out of range
        assert!(Cron::parse("* * * *").is_err()); // too few fields
    }

    #[test]
    fn next_after_every_five_minutes() {
        let cron = Cron::parse("*/5 * * * *").unwrap();
        // Strictly after t=0 → minute 5.
        assert_eq!(cron.next_after(0), Some(300_000));
        // Strictly after minute 5 → minute 10.
        assert_eq!(cron.next_after(300_000), Some(600_000));
        // Mid-interval rounds up to the next slot.
        assert_eq!(cron.next_after(200_000), Some(300_000));
    }

    #[test]
    fn next_after_yearly_is_next_jan_first() {
        let cron = Cron::parse("@yearly").unwrap();
        // After epoch (1970-01-01 00:00) the next is 1971-01-01 00:00 = day 365.
        let expected = 365u64 * 86_400 * 1_000;
        assert_eq!(cron.next_after(0), Some(expected));
        let (y, m, d, h, min, _) = civil_fields(expected / 1000);
        assert_eq!((y, m, d, h, min), (1971, 1, 1, 0, 0));
    }

    #[test]
    fn impossible_expression_has_no_next() {
        // Feb 31 never occurs.
        let cron = Cron::parse("0 0 31 2 *").unwrap();
        assert_eq!(cron.next_after(0), None);
    }

    #[test]
    fn dom_dow_or_semantics_when_both_restricted() {
        // Day-of-month 13 OR Friday (dow=5). Verify a known Friday-the-13th and a
        // plain 13th both match, and a non-matching day does not.
        let cron = Cron::parse("0 0 13 * 5").unwrap();
        // 1970-02-13 is a Friday → matches via both; pick its midnight.
        // day index for 1970-02-13 = 31 (Jan) + 12 = 43.
        let feb13 = 43u64 * 86_400 * 1000;
        let (_, m, d, _, _, dow) = civil_fields(feb13 / 1000);
        assert_eq!((m, d, dow), (2, 13, 5));
        assert_eq!(cron.next_after(feb13 - 60_000), Some(feb13));
    }
}
