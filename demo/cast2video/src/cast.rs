//! asciinema cast parsing — v2 (JSON Lines) and v1 (single JSON object).

use serde_json::Value;

pub struct Event {
    /// Seconds from the start of the recording.
    pub t: f64,
    pub data: String,
}

pub struct Cast {
    pub cols: usize,
    pub rows: usize,
    pub title: Option<String>,
    pub events: Vec<Event>,
}

impl Cast {
    pub fn parse(src: &str) -> Result<Cast, String> {
        let first = src
            .lines()
            .find(|l| !l.trim().is_empty())
            .ok_or("cast file is empty")?;
        let head: Value =
            serde_json::from_str(first).map_err(|e| format!("cast header is not JSON: {e}"))?;
        let version = head.get("version").and_then(|v| v.as_u64()).unwrap_or(2);

        let cols = head
            .get("width")
            .and_then(|v| v.as_u64())
            .ok_or("cast header has no `width`")? as usize;
        let rows = head
            .get("height")
            .and_then(|v| v.as_u64())
            .ok_or("cast header has no `height`")? as usize;
        let title = head
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        let mut events = Vec::new();
        match version {
            1 => {
                // v1 stores [relative_delay, data] pairs inside the header object.
                let stdout = head
                    .get("stdout")
                    .and_then(|v| v.as_array())
                    .ok_or("v1 cast has no `stdout` array")?;
                let mut t = 0.0;
                for e in stdout {
                    let a = e.as_array().ok_or("v1 stdout entry is not an array")?;
                    t += a.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
                    if let Some(d) = a.get(1).and_then(|v| v.as_str()) {
                        events.push(Event {
                            t,
                            data: d.to_owned(),
                        });
                    }
                }
            }
            2 => {
                for (n, line) in src.lines().enumerate().skip(1) {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let v: Value = serde_json::from_str(line)
                        .map_err(|e| format!("cast line {}: {e}", n + 1))?;
                    let a = v
                        .as_array()
                        .ok_or_else(|| format!("cast line {} is not an array", n + 1))?;
                    let t = a.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
                    // Only output events paint the screen. Input ("i"), resize
                    // ("r") and marker ("m") events are skipped deliberately —
                    // a resize mid-recording would need a grid rebuild, which
                    // this tool does not do, so it is ignored rather than
                    // half-applied.
                    if a.get(1).and_then(|v| v.as_str()) == Some("o") {
                        if let Some(d) = a.get(2).and_then(|v| v.as_str()) {
                            events.push(Event {
                                t,
                                data: d.to_owned(),
                            });
                        }
                    }
                }
            }
            v => {
                return Err(format!(
                    "unsupported cast version {v} (this reads v1 and v2)"
                ));
            }
        }

        if events.is_empty() {
            return Err("cast contains no output events".into());
        }
        Ok(Cast {
            cols,
            rows,
            title,
            events,
        })
    }

    pub fn duration(&self) -> f64 {
        self.events.last().map(|e| e.t).unwrap_or(0.0)
    }

    /// Rewrite timings: clamp any gap longer than `idle_cap` seconds, then divide
    /// by `speed`. Returns the adjusted time for each event, in order.
    ///
    /// Capping is what makes a 90-second recording with deliberate reading pauses
    /// usable as a short loop, without re-recording it at a different pace.
    pub fn retime(&self, idle_cap: Option<f64>, speed: f64) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.events.len());
        let mut shift = 0.0;
        let mut prev = 0.0;
        for e in &self.events {
            if let Some(cap) = idle_cap {
                let gap = e.t - prev;
                if gap > cap {
                    shift += gap - cap;
                }
            }
            prev = e.t;
            out.push(((e.t - shift) / speed).max(0.0));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V2: &str = concat!(
        "{\"version\":2,\"width\":10,\"height\":3,\"title\":\"t\"}\n",
        "[0.0,\"o\",\"a\"]\n",
        "[1.0,\"i\",\"ignored\"]\n",
        "[5.0,\"o\",\"b\"]\n"
    );

    #[test]
    fn v2_parses_output_events_only() {
        let c = Cast::parse(V2).unwrap();
        assert_eq!((c.cols, c.rows), (10, 3));
        assert_eq!(c.title.as_deref(), Some("t"));
        assert_eq!(c.events.len(), 2);
        assert_eq!(c.duration(), 5.0);
    }

    #[test]
    fn v1_accumulates_relative_delays() {
        let src = "{\"version\":1,\"width\":8,\"height\":2,\"stdout\":[[0.5,\"x\"],[0.25,\"y\"]]}";
        let c = Cast::parse(src).unwrap();
        assert_eq!(c.events.len(), 2);
        assert!((c.events[1].t - 0.75).abs() < 1e-9);
    }

    #[test]
    fn idle_capping_shortens_only_the_long_gap() {
        let c = Cast::parse(V2).unwrap();
        let t = c.retime(Some(1.0), 1.0);
        // The 5s gap collapses to 1s; the first event stays at 0.
        assert_eq!(t[0], 0.0);
        assert!((t[1] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn speed_scales_the_whole_timeline() {
        let c = Cast::parse(V2).unwrap();
        let t = c.retime(None, 2.0);
        assert!((t[1] - 2.5).abs() < 1e-9);
    }

    #[test]
    fn an_empty_cast_is_a_clear_error() {
        assert!(Cast::parse("").is_err());
        assert!(Cast::parse("{\"version\":2,\"width\":1,\"height\":1}").is_err());
    }
}
