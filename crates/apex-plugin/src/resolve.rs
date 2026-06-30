//! Plugin dependency resolution
//! ([versioning §4](../../docs/08-plugin-sdk/versioning.md#4-dependency-resolution)).
//!
//! Plugins may declare dependencies on other plugins (`name` + a semver `version`
//! range). The engine resolves these against the **installed catalog**: install
//! requires a plugin's dependencies to already be installed and version-compatible
//! (deps first), enable brings them up in dependency order, and disable/uninstall are
//! blocked while dependents still rely on a plugin. Fetching missing dependencies from
//! a registry is the future marketplace slice; here resolution is local and
//! fail-closed.
//!
//! The functions are pure over a `&BTreeMap<qualified_id, InstalledPlugin>` so they're
//! deterministic and unit-testable independent of the engine.

use crate::engine::InstalledPlugin;
use crate::manifest::{Dependency, parse_version_req};
use apex_common::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

/// The catalog the resolver reads: installed plugins keyed by qualified id.
type Catalog = BTreeMap<String, InstalledPlugin>;

/// Whether `plugin` could satisfy `dep` by **name** — either its fully-qualified id
/// (`publisher/name`) or its bare `name`.
fn name_matches(dep: &Dependency, plugin: &InstalledPlugin) -> bool {
    dep.name == plugin.manifest.qualified_id() || dep.name == plugin.manifest.metadata.name
}

/// Whether `plugin` satisfies `dep` (name match **and** its version meets the range).
pub(crate) fn satisfies(dep: &Dependency, plugin: &InstalledPlugin) -> Result<bool> {
    if !name_matches(dep, plugin) {
        return Ok(false);
    }
    let req = parse_version_req(&dep.version)?;
    let version = semver::Version::parse(&plugin.manifest.metadata.version)
        .map_err(|e| Error::invalid(format!("installed version is not valid semver: {e}")))?;
    Ok(req.matches(&version))
}

/// Resolve `dep` to the qualified id of a satisfying installed plugin, preferring the
/// **highest** compatible version when several match
/// ([versioning §4](../../docs/08-plugin-sdk/versioning.md#4-dependency-resolution)).
/// Fail-closed: a clear error when nothing matches the name, or when matches exist but
/// none satisfy the version range (an unsatisfiable constraint).
pub(crate) fn resolve_dep(dep: &Dependency, catalog: &Catalog) -> Result<String> {
    let by_name: Vec<&InstalledPlugin> =
        catalog.values().filter(|p| name_matches(dep, p)).collect();
    if by_name.is_empty() {
        return Err(Error::invalid(format!(
            "dependency `{}` ({}) is not installed",
            dep.name, dep.version
        )));
    }
    let req = parse_version_req(&dep.version)?;
    let best = by_name
        .iter()
        .filter_map(|p| {
            semver::Version::parse(&p.manifest.metadata.version)
                .ok()
                .filter(|v| req.matches(v))
                .map(|v| (v, p.manifest.qualified_id()))
        })
        .max_by(|a, b| a.0.cmp(&b.0));
    match best {
        Some((_, id)) => Ok(id),
        None => {
            let installed: Vec<String> = by_name
                .iter()
                .map(|p| p.manifest.metadata.version.clone())
                .collect();
            Err(Error::invalid(format!(
                "dependency `{}` requires `{}`, but installed version(s) {:?} do not satisfy it",
                dep.name, dep.version, installed
            )))
        }
    }
}

/// Topologically order `target` and its transitive dependencies, **dependencies
/// first** (so callers enable in this order). Errors on a missing dependency or a
/// dependency cycle.
pub(crate) fn enable_order(target: &str, catalog: &Catalog) -> Result<Vec<String>> {
    let mut order = Vec::new();
    let mut done = BTreeSet::new();
    let mut on_path = BTreeSet::new();
    visit(target, catalog, &mut order, &mut done, &mut on_path)?;
    Ok(order)
}

fn visit(
    id: &str,
    catalog: &Catalog,
    order: &mut Vec<String>,
    done: &mut BTreeSet<String>,
    on_path: &mut BTreeSet<String>,
) -> Result<()> {
    if done.contains(id) {
        return Ok(());
    }
    if !on_path.insert(id.to_string()) {
        return Err(Error::invalid(format!(
            "plugin dependency cycle involving `{id}`"
        )));
    }
    let plugin = catalog
        .get(id)
        .ok_or_else(|| Error::NotFound(format!("plugin `{id}` is not installed")))?;
    for dep in &plugin.manifest.dependencies {
        let dep_id = resolve_dep(dep, catalog)?;
        visit(&dep_id, catalog, order, done, on_path)?;
    }
    on_path.remove(id);
    done.insert(id.to_string());
    order.push(id.to_string());
    Ok(())
}

/// The installed plugins that directly depend on `target` (each has a declared
/// dependency that `target` satisfies). Sorted by qualified id.
pub(crate) fn dependents(target: &str, catalog: &Catalog) -> Vec<String> {
    let Some(target_plugin) = catalog.get(target) else {
        return Vec::new();
    };
    catalog
        .iter()
        .filter(|(id, _)| id.as_str() != target)
        .filter(|(_, p)| {
            p.manifest
                .dependencies
                .iter()
                .any(|dep| satisfies(dep, target_plugin).unwrap_or(false))
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// The installed dependents of `target` whose version requirement would **not** be
/// met if `target` were at `new_version` — i.e. dependents an upgrade would break.
/// Sorted by qualified id. (Plugin name/publisher are stable across versions, so name
/// matching uses the currently-installed `target`.)
pub(crate) fn dependents_broken_by(
    target: &str,
    new_version: &semver::Version,
    catalog: &Catalog,
) -> Vec<String> {
    let Some(target_plugin) = catalog.get(target) else {
        return Vec::new();
    };
    catalog
        .iter()
        .filter(|(id, _)| id.as_str() != target)
        .filter(|(_, p)| {
            p.manifest.dependencies.iter().any(|dep| {
                name_matches(dep, target_plugin)
                    && parse_version_req(&dep.version)
                        .map(|req| !req.matches(new_version))
                        .unwrap_or(true)
            })
        })
        .map(|(id, _)| id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::PluginState;
    use crate::manifest::PluginManifest;

    /// An installed (disabled) catalog entry from a manifest with the given name,
    /// version, and dependency list (`(name, version_req)` pairs).
    fn entry(name: &str, version: &str, deps: &[(&str, &str)]) -> InstalledPlugin {
        let mut yaml = format!(
            "apiVersion: plugin.apex.io/v1\nkind: Plugin\nmetadata:\n  name: {name}\n  version: {version}\n  publisher: acme\n"
        );
        if !deps.is_empty() {
            yaml.push_str("dependencies:\n");
            for (n, v) in deps {
                yaml.push_str(&format!("  - name: {n}\n    version: \"{v}\"\n"));
            }
        }
        InstalledPlugin {
            manifest: PluginManifest::from_yaml(&yaml).unwrap(),
            state: PluginState::Disabled,
            granted_permissions: Vec::new(),
            artifact_dir: None,
            previous: None,
        }
    }

    fn catalog(entries: Vec<InstalledPlugin>) -> Catalog {
        entries
            .into_iter()
            .map(|p| (p.manifest.qualified_id(), p))
            .collect()
    }

    #[test]
    fn resolves_by_bare_and_qualified_name() {
        let cat = catalog(vec![entry("http-core", "1.2.0", &[])]);
        let dep_bare = Dependency {
            name: "http-core".into(),
            version: "^1.0.0".into(),
        };
        let dep_qual = Dependency {
            name: "acme/http-core".into(),
            version: ">=1.0.0 <2.0.0".into(),
        };
        assert_eq!(resolve_dep(&dep_bare, &cat).unwrap(), "acme/http-core");
        assert_eq!(resolve_dep(&dep_qual, &cat).unwrap(), "acme/http-core");
    }

    #[test]
    fn reports_missing_and_unsatisfiable() {
        let cat = catalog(vec![entry("http-core", "1.2.0", &[])]);
        // Missing name.
        let missing = Dependency {
            name: "redis".into(),
            version: "^1.0.0".into(),
        };
        assert!(
            resolve_dep(&missing, &cat)
                .unwrap_err()
                .to_string()
                .contains("not installed")
        );
        // Present but version conflict.
        let conflict = Dependency {
            name: "http-core".into(),
            version: "^2.0.0".into(),
        };
        assert!(
            resolve_dep(&conflict, &cat)
                .unwrap_err()
                .to_string()
                .contains("do not satisfy")
        );
    }

    #[test]
    fn enable_order_is_dependencies_first() {
        // app → http-core, db ; db → http-core
        let cat = catalog(vec![
            entry("http-core", "1.0.0", &[]),
            entry("db", "1.0.0", &[("http-core", "^1.0.0")]),
            entry("app", "1.0.0", &[("http-core", "^1.0.0"), ("db", "^1.0.0")]),
        ]);
        let order = enable_order("acme/app", &cat).unwrap();
        // Each dependency must appear before its dependents.
        let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
        assert!(pos("acme/http-core") < pos("acme/db"));
        assert!(pos("acme/db") < pos("acme/app"));
        assert!(pos("acme/http-core") < pos("acme/app"));
        assert_eq!(*order.last().unwrap(), "acme/app");
    }

    #[test]
    fn enable_order_detects_cycles() {
        let cat = catalog(vec![
            entry("a", "1.0.0", &[("b", "^1.0.0")]),
            entry("b", "1.0.0", &[("a", "^1.0.0")]),
        ]);
        assert!(
            enable_order("acme/a", &cat)
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
    }

    #[test]
    fn dependents_lists_dependers() {
        let cat = catalog(vec![
            entry("http-core", "1.0.0", &[]),
            entry("db", "1.0.0", &[("http-core", "^1.0.0")]),
            entry("app", "1.0.0", &[("db", "^1.0.0")]),
        ]);
        assert_eq!(
            dependents("acme/http-core", &cat),
            vec!["acme/db".to_string()]
        );
        assert_eq!(dependents("acme/db", &cat), vec!["acme/app".to_string()]);
        assert!(dependents("acme/app", &cat).is_empty());
    }
}
