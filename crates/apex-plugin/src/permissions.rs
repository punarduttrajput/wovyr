//! The plugin permission model ([Permissions](../../docs/08-plugin-sdk/permissions.md)).
//!
//! Permissions are structured `domain:action:resource` strings. The model is
//! least-privilege and fail-closed: a capability enables only when every permission
//! it *declares* is *granted*. Grants may use `*` and trailing-`*` prefix wildcards
//! (e.g. `net:egress:*`, `event:publish:plugin.*`), which broaden a single grant to
//! cover many requests.

/// Whether the set of `granted` permissions covers every one in `declared`.
/// Returns the declared permissions that are **not** satisfied (empty ⇒ all granted),
/// so the caller can fail closed and report exactly what is missing.
pub fn missing_grants(declared: &[String], granted: &[String]) -> Vec<String> {
    declared
        .iter()
        .filter(|d| !granted.iter().any(|g| grant_covers(g, d)))
        .cloned()
        .collect()
}

/// Whether `granted` (which may use wildcards) covers the `wanted` permission.
/// Each `:`-separated segment must match; a granted segment matches if it equals the
/// wanted segment, is `*`, or is a `<prefix>*` covering the wanted segment.
pub fn grant_covers(granted: &str, wanted: &str) -> bool {
    let g: Vec<&str> = granted.split(':').collect();
    let w: Vec<&str> = wanted.split(':').collect();
    if g.len() != w.len() {
        return false;
    }
    g.iter().zip(&w).all(|(gs, ws)| segment_matches(gs, ws))
}

/// Match one permission segment: exact, `*` (any), or `<prefix>*` (prefix wildcard).
fn segment_matches(granted: &str, wanted: &str) -> bool {
    if let Some(prefix) = granted.strip_suffix('*') {
        wanted.starts_with(prefix)
    } else {
        granted == wanted
    }
}

/// Whether a permission string is "broad" (uses a wildcard) — flagged for elevated
/// grant approval per [Permissions §3](../../docs/08-plugin-sdk/permissions.md#3-permission-syntax).
pub fn is_broad(permission: &str) -> bool {
    permission.contains('*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_grant_covers_exact_request() {
        assert!(grant_covers(
            "net:egress:api.github.com",
            "net:egress:api.github.com"
        ));
        assert!(!grant_covers(
            "net:egress:api.github.com",
            "net:egress:evil.com"
        ));
    }

    #[test]
    fn wildcards_broaden_grants() {
        assert!(grant_covers("net:egress:*", "net:egress:api.github.com"));
        assert!(grant_covers("secret:read:*", "secret:read:github-token"));
        assert!(grant_covers(
            "event:publish:plugin.*",
            "event:publish:plugin.installed"
        ));
        // Prefix wildcard must respect the prefix boundary.
        assert!(!grant_covers(
            "event:publish:plugin.*",
            "event:publish:other.x"
        ));
        // Arity mismatch never matches.
        assert!(!grant_covers("net:egress", "net:egress:api.github.com"));
    }

    #[test]
    fn missing_grants_reports_ungranted() {
        let declared = vec![
            "net:egress:api.github.com".to_string(),
            "secret:read:github-token".to_string(),
        ];
        let granted = vec!["net:egress:*".to_string()];
        let missing = missing_grants(&declared, &granted);
        assert_eq!(missing, vec!["secret:read:github-token".to_string()]);

        let full = vec!["net:egress:*".to_string(), "secret:read:*".to_string()];
        assert!(missing_grants(&declared, &full).is_empty());
    }

    #[test]
    fn flags_broad_permissions() {
        assert!(is_broad("net:egress:*"));
        assert!(!is_broad("secret:read:github-token"));
    }
}
