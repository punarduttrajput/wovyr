//! OSV/CVE vulnerability feed for the publish scanner (RM-AIM-P3 ECO-305).
//!
//! Replaces the *only-manual* half of the SBOM story: [`crate::scan`]'s operator
//! deny-list still exists (it's the right tool for an emergency block), but known
//! vulnerabilities now come from an [OSV](https://osv.dev)-format feed matched
//! against every SBOM component's `name@version` at publish time.
//!
//! **Deterministic by construction, like the rest of this crate**: the feed is
//! operator-supplied *data* (a JSON file of OSV records — a bulk export, an
//! `osv.dev` API response body, or a hand-curated subset), parsed once with
//! [`VulnFeed::from_osv_json`] and matched purely. `scan` never touches the
//! network; refreshing the feed is the operator's (or a cron job's) concern, and
//! the same package + feed always yields the same report.
//!
//! The parser accepts the OSV schema subset that drives matching (`id`,
//! `summary`, `aliases`, `affected[].package.name`, `affected[].versions`,
//! `affected[].ranges[]` of type `SEMVER`) and ignores unknown fields — OSV
//! records carry far more than matching needs, and a feed newer than this parser
//! should still match on the stable core. Malformed JSON or a record without an
//! `id` fails closed ([`apex_common::Error::Invalid`]) rather than silently
//! scanning against a half-loaded feed. Range semantics follow OSV: a version is
//! affected within `[introduced, fixed)` or `[introduced, last_affected]`;
//! non-`SEMVER` range types (`GIT`, `ECOSYSTEM`) are not range-matched (their
//! explicit `versions` lists still are), because guessing ecosystem-specific
//! ordering would trade a false negative for a wrong answer.

use apex_common::{Error, Result};
use semver::Version;
use serde::Deserialize;
use std::collections::BTreeMap;

/// A parsed, immutable vulnerability feed, indexed by package name.
#[derive(Clone, Debug, Default)]
pub struct VulnFeed {
    by_package: BTreeMap<String, Vec<Advisory>>,
}

/// One advisory a component matched (borrowed from the feed by
/// [`VulnFeed::advisories_for`]).
#[derive(Clone, Debug)]
pub struct Advisory {
    /// The OSV record id (`GHSA-…`, `RUSTSEC-…`, `CVE-…`).
    pub id: String,
    /// Human-readable one-line summary (may be empty — not every record has one).
    pub summary: String,
    /// Alternate ids for the same vulnerability (typically the CVE).
    pub aliases: Vec<String>,
    affected: Vec<Affected>,
}

#[derive(Clone, Debug)]
struct Affected {
    package: String,
    /// Explicitly enumerated affected versions (exact string match).
    versions: Vec<String>,
    /// `SEMVER` ranges, already parsed.
    ranges: Vec<SemverRange>,
}

#[derive(Clone, Debug)]
struct SemverRange {
    /// `None` ⇒ affected from the beginning (OSV's `"introduced": "0"`).
    introduced: Option<Version>,
    /// Exclusive upper bound.
    fixed: Option<Version>,
    /// Inclusive upper bound (used when there is no `fixed` event).
    last_affected: Option<Version>,
}

impl SemverRange {
    fn contains(&self, v: &Version) -> bool {
        if let Some(intro) = &self.introduced
            && v < intro
        {
            return false;
        }
        match (&self.fixed, &self.last_affected) {
            (Some(fixed), _) => v < fixed,
            (None, Some(last)) => v <= last,
            (None, None) => true,
        }
    }
}

// --- raw OSV wire shapes (tolerant of extra fields by design) ---------------

#[derive(Deserialize)]
struct RawEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    affected: Vec<RawAffected>,
}

#[derive(Deserialize)]
struct RawAffected {
    package: Option<RawPackage>,
    #[serde(default)]
    versions: Vec<String>,
    #[serde(default)]
    ranges: Vec<RawRange>,
}

#[derive(Deserialize)]
struct RawPackage {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct RawRange {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    events: Vec<RawEvent>,
}

#[derive(Deserialize)]
struct RawEvent {
    introduced: Option<String>,
    fixed: Option<String>,
    last_affected: Option<String>,
}

/// Parse `x`, `x.y`, or `x.y.z` into a semver `Version` (OSV events often say
/// `"0"` or `"1.2"`). Returns `None` for anything that still doesn't parse —
/// the caller treats an unparseable version as unmatchable-by-range rather than
/// guessing.
fn parse_lenient(s: &str) -> Option<Version> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    for candidate in [s.to_string(), format!("{s}.0"), format!("{s}.0.0")] {
        if let Ok(v) = Version::parse(&candidate) {
            return Some(v);
        }
    }
    None
}

impl VulnFeed {
    /// Parse an OSV JSON document: either a top-level array of OSV records or an
    /// object with a `vulns` array (the shape the osv.dev query API returns).
    /// Fail-closed: malformed JSON, or any record without an `id`, is an
    /// [`Error::Invalid`] — never a silently smaller feed.
    pub fn from_osv_json(json: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct Wrapped {
            vulns: Vec<RawEntry>,
        }
        let entries: Vec<RawEntry> =
            match serde_json::from_str::<Vec<RawEntry>>(json) {
                Ok(v) => v,
                Err(_) => serde_json::from_str::<Wrapped>(json)
                    .map_err(|e| {
                        Error::Invalid(format!(
                            "OSV feed is neither an array of records nor {{\"vulns\": […]}}: {e}"
                        ))
                    })?
                    .vulns,
            };

        let mut by_package: BTreeMap<String, Vec<Advisory>> = BTreeMap::new();
        for raw in entries {
            if raw.id.is_empty() {
                return Err(Error::Invalid(
                    "OSV feed record without an `id` — refusing a feed that can't \
                     name its own advisories"
                        .into(),
                ));
            }
            let mut affected = Vec::new();
            for a in raw.affected {
                let Some(pkg) = a.package else { continue };
                if pkg.name.is_empty() {
                    continue;
                }
                let ranges = a
                    .ranges
                    .iter()
                    .filter(|r| r.kind == "SEMVER")
                    .map(|r| {
                        // OSV events arrive as a flat sequence; fold them into
                        // one range per `ranges` entry (the overwhelmingly common
                        // shape — one introduced + one fixed/last_affected).
                        let mut range = SemverRange {
                            introduced: None,
                            fixed: None,
                            last_affected: None,
                        };
                        for e in &r.events {
                            if let Some(v) = e.introduced.as_deref() {
                                // "0" means "from the beginning" — leave None.
                                if v != "0" {
                                    range.introduced = parse_lenient(v);
                                }
                            }
                            if let Some(v) = e.fixed.as_deref() {
                                range.fixed = parse_lenient(v);
                            }
                            if let Some(v) = e.last_affected.as_deref() {
                                range.last_affected = parse_lenient(v);
                            }
                        }
                        range
                    })
                    .collect();
                affected.push(Affected {
                    package: pkg.name,
                    versions: a.versions,
                    ranges,
                });
            }
            // Index the advisory under every package it names.
            let advisory = Advisory {
                id: raw.id,
                summary: raw.summary,
                aliases: raw.aliases,
                affected,
            };
            let mut names: Vec<&str> = advisory
                .affected
                .iter()
                .map(|a| a.package.as_str())
                .collect();
            names.sort_unstable();
            names.dedup();
            for name in names {
                by_package
                    .entry(name.to_string())
                    .or_default()
                    .push(advisory.clone());
            }
        }
        Ok(Self { by_package })
    }

    /// Whether the feed carries no advisories at all.
    pub fn is_empty(&self) -> bool {
        self.by_package.is_empty()
    }

    /// Total number of distinct package names the feed covers.
    pub fn package_count(&self) -> usize {
        self.by_package.len()
    }

    /// Every advisory affecting `name@version`, in feed order. A version that
    /// doesn't parse as (lenient) semver can still match an advisory's explicit
    /// `versions` list, just not its ranges.
    pub fn advisories_for(&self, name: &str, version: &str) -> Vec<&Advisory> {
        let Some(advisories) = self.by_package.get(name) else {
            return Vec::new();
        };
        let parsed = parse_lenient(version);
        advisories
            .iter()
            .filter(|adv| {
                adv.affected.iter().any(|a| {
                    a.package == name
                        && (a.versions.iter().any(|v| v == version)
                            || parsed
                                .as_ref()
                                .is_some_and(|v| a.ranges.iter().any(|r| r.contains(v))))
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic OSV record subset: a semver range with introduced+fixed, an
    /// alias, and a second record using an explicit versions list.
    const FEED: &str = r#"[
      {
        "id": "RUSTSEC-2026-0001",
        "aliases": ["CVE-2026-11111"],
        "summary": "widget-lib deserializes attacker input into shell commands",
        "affected": [
          {
            "package": { "ecosystem": "crates.io", "name": "widget-lib" },
            "ranges": [
              { "type": "SEMVER",
                "events": [ { "introduced": "1.0.0" }, { "fixed": "1.4.2" } ] }
            ]
          }
        ],
        "database_specific": { "severity": "HIGH" }
      },
      {
        "id": "GHSA-xxxx-yyyy-zzzz",
        "summary": "leftpad token exfiltration",
        "affected": [
          { "package": { "name": "leftpad" }, "versions": ["0.9.0", "0.9.1"] }
        ]
      }
    ]"#;

    #[test]
    fn semver_range_matches_inside_and_not_outside() {
        let feed = VulnFeed::from_osv_json(FEED).unwrap();
        assert_eq!(feed.package_count(), 2);

        // Inside [1.0.0, 1.4.2): affected.
        let hits = feed.advisories_for("widget-lib", "1.2.3");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "RUSTSEC-2026-0001");
        assert_eq!(hits[0].aliases, ["CVE-2026-11111"]);

        // The fix version and beyond: clean. Before introduced: clean.
        assert!(feed.advisories_for("widget-lib", "1.4.2").is_empty());
        assert!(feed.advisories_for("widget-lib", "2.0.0").is_empty());
        assert!(feed.advisories_for("widget-lib", "0.9.9").is_empty());
        // A different package entirely: clean.
        assert!(feed.advisories_for("other-lib", "1.2.3").is_empty());
    }

    #[test]
    fn explicit_versions_list_matches_exactly() {
        let feed = VulnFeed::from_osv_json(FEED).unwrap();
        assert_eq!(feed.advisories_for("leftpad", "0.9.1").len(), 1);
        assert!(feed.advisories_for("leftpad", "0.9.2").is_empty());
    }

    #[test]
    fn lenient_semver_pads_partial_versions() {
        // OSV events often say "1.2"; component versions may too.
        let json = r#"[{ "id": "X-1", "affected": [ { "package": {"name": "p"},
          "ranges": [{"type": "SEMVER", "events": [{"introduced": "0"}, {"fixed": "2"}]}] } ] }]"#;
        let feed = VulnFeed::from_osv_json(json).unwrap();
        assert_eq!(feed.advisories_for("p", "1.9").len(), 1);
        assert!(feed.advisories_for("p", "2.0").is_empty());
    }

    #[test]
    fn last_affected_is_an_inclusive_bound() {
        let json = r#"[{ "id": "X-2", "affected": [ { "package": {"name": "p"},
          "ranges": [{"type": "SEMVER", "events": [{"introduced": "1.0.0"}, {"last_affected": "1.2.0"}]}] } ] }]"#;
        let feed = VulnFeed::from_osv_json(json).unwrap();
        assert_eq!(feed.advisories_for("p", "1.2.0").len(), 1, "inclusive");
        assert!(feed.advisories_for("p", "1.2.1").is_empty());
    }

    #[test]
    fn wrapped_vulns_object_and_unknown_fields_parse() {
        let json = r#"{ "vulns": [ { "id": "OSV-1", "modified": "2026-01-01T00:00:00Z",
          "affected": [ { "package": {"name": "p", "purl": "pkg:cargo/p"}, "versions": ["1.0.0"],
                          "ecosystem_specific": {"functions": []} } ],
          "references": [{"type": "WEB", "url": "https://example.com"}] } ] }"#;
        let feed = VulnFeed::from_osv_json(json).unwrap();
        assert_eq!(feed.advisories_for("p", "1.0.0").len(), 1);
    }

    #[test]
    fn malformed_feed_and_missing_id_fail_closed() {
        assert!(VulnFeed::from_osv_json("not json").is_err());
        assert!(VulnFeed::from_osv_json(r#"[{"summary": "no id"}]"#).is_err());
    }

    #[test]
    fn non_semver_ranges_do_not_range_match() {
        // A GIT range must not be guessed at; only explicit versions match.
        let json = r#"[{ "id": "X-3", "affected": [ { "package": {"name": "p"},
          "versions": ["deadbeef"],
          "ranges": [{"type": "GIT", "events": [{"introduced": "0"}]}] } ] }]"#;
        let feed = VulnFeed::from_osv_json(json).unwrap();
        assert!(feed.advisories_for("p", "1.0.0").is_empty());
        assert_eq!(feed.advisories_for("p", "deadbeef").len(), 1);
    }
}
