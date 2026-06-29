//! `apex plugin` commands: the local plugin lifecycle surface
//! ([Plugin System Overview §6](../../docs/08-plugin-sdk/overview.md#6-installation-lifecycle)).
//!
//! Drives the [Plugin Engine](apex_plugin) against durable state under
//! `~/.apex/plugins/`:
//!
//! - `trust.json` — the [`TrustStore`] of trusted publisher ed25519 public keys.
//! - `catalog.json` — the installed-plugin catalog ([`InstalledPlugin`] records).
//! - `staging/` — content-addressed staged artifacts (so the WASM loader can run them).
//!
//! Commands: `keygen` + `sign` (publisher tooling), `trust` (register a publisher key),
//! `install` (verify + stage + register, disabled), `list`, `enable`/`disable`, and
//! `uninstall`. Enabling a plugin flips its persisted state; enabled plugin tools are
//! wired into `apex agents run --local` (the loader executes them when the CLI is built
//! with `--features plugin-wasi`).

use crate::config;
use apex_common::{Error, Result};
use apex_plugin::{
    CapabilityKind, InstalledPlugin, Package, PluginEngine, PluginManifest, PluginState, TrustStore,
};
use apex_tools::{ToolContext, ToolRegistry, ToolRequest};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The platform-API version the engine checks plugin `compatibility` ranges against.
fn platform_api() -> semver::Version {
    semver::Version::new(1, 0, 0)
}

/// The durable plugin directory, `~/.apex/plugins`.
fn plugins_dir() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("plugins"))
}

/// Where verified artifacts are staged, `~/.apex/plugins/staging`.
fn staging_dir() -> Result<PathBuf> {
    Ok(plugins_dir()?.join("staging"))
}

fn trust_path() -> Result<PathBuf> {
    Ok(plugins_dir()?.join("trust.json"))
}

fn catalog_path() -> Result<PathBuf> {
    Ok(plugins_dir()?.join("catalog.json"))
}

/// Load the persisted trust store (empty if none exists yet).
fn load_trust() -> Result<TrustStore> {
    match std::fs::read(trust_path()?) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::config(format!("corrupt trust store: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TrustStore::new()),
        Err(e) => Err(Error::Io(e)),
    }
}

fn save_trust(trust: &TrustStore) -> Result<()> {
    std::fs::create_dir_all(plugins_dir()?)?;
    std::fs::write(trust_path()?, serde_json::to_vec_pretty(trust)?)?;
    Ok(())
}

/// Load the persisted installed-plugin catalog (empty if none exists yet).
fn load_catalog() -> Result<Vec<InstalledPlugin>> {
    match std::fs::read(catalog_path()?) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::config(format!("corrupt plugin catalog: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::Io(e)),
    }
}

fn save_catalog(catalog: &[InstalledPlugin]) -> Result<()> {
    std::fs::create_dir_all(plugins_dir()?)?;
    std::fs::write(catalog_path()?, serde_json::to_vec_pretty(catalog)?)?;
    Ok(())
}

/// Build an engine from the durable trust store + catalog, staging into
/// `~/.apex/plugins/staging`. With `--features plugin-wasi` it carries the real WASM
/// capability loader; otherwise capabilities register but error on call.
pub fn engine() -> Result<PluginEngine> {
    let trust = load_trust()?;
    let catalog = load_catalog()?;
    let engine = PluginEngine::new(platform_api(), trust)
        .with_staging_dir(staging_dir()?)
        .with_catalog(catalog);
    Ok(with_runtime(engine))
}

#[cfg(feature = "plugin-wasi")]
fn with_runtime(engine: PluginEngine) -> PluginEngine {
    match apex_plugin::WasiCapabilityRuntime::new() {
        Ok(rt) => engine.with_runtime(std::sync::Arc::new(rt)),
        Err(_) => engine,
    }
}

#[cfg(not(feature = "plugin-wasi"))]
fn with_runtime(engine: PluginEngine) -> PluginEngine {
    engine
}

/// `apex plugin keygen --publisher <name> [--dir <dir>]` — generate an ed25519 signing
/// keypair for a publisher: a private PKCS#8 key and its raw public key.
pub fn keygen_cmd(publisher: &str, dir: &str) -> Result<()> {
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|_| Error::config("ed25519 key generation failed"))?;
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| Error::config("generated key was rejected"))?;

    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    let key_path = dir.join(format!("{publisher}.key"));
    let pub_path = dir.join(format!("{publisher}.pub"));
    std::fs::write(&key_path, pkcs8.as_ref())?;
    restrict(&key_path);
    std::fs::write(&pub_path, kp.public_key().as_ref())?;

    println!("Generated signing keypair for `{publisher}`:");
    println!("  private key: {}  (keep secret)", key_path.display());
    println!("  public key:  {}", pub_path.display());
    println!(
        "Next: `apex plugin trust {publisher} --key {}`",
        pub_path.display()
    );
    Ok(())
}

/// `apex plugin sign --key <key> --manifest <plugin.yaml> [--out <sig>]` — produce a
/// detached ed25519 signature over the manifest bytes (the bytes the engine verifies).
pub fn sign_cmd(key: &str, manifest: &str, out: Option<String>) -> Result<()> {
    use ring::signature::Ed25519KeyPair;

    let pkcs8 = std::fs::read(key)
        .map_err(|e| Error::config(format!("could not read signing key {key}: {e}")))?;
    let kp = Ed25519KeyPair::from_pkcs8(&pkcs8)
        .map_err(|_| Error::config(format!("`{key}` is not a valid PKCS#8 ed25519 key")))?;
    let message = std::fs::read(manifest)
        .map_err(|e| Error::config(format!("could not read manifest {manifest}: {e}")))?;
    let sig = kp.sign(&message);

    let out = out.unwrap_or_else(|| default_sig_path(manifest));
    std::fs::write(&out, sig.as_ref())?;
    println!("Wrote signature to {out}");
    Ok(())
}

/// `<manifest-dir>/plugin.sig` next to the manifest, by default.
fn default_sig_path(manifest: &str) -> String {
    Path::new(manifest)
        .parent()
        .map(|p| p.join("plugin.sig"))
        .unwrap_or_else(|| PathBuf::from("plugin.sig"))
        .to_string_lossy()
        .into_owned()
}

/// `apex plugin trust <publisher> --key <pubkey-file>` — register a publisher's raw
/// ed25519 public key so its packages verify on install.
pub fn trust_cmd(publisher: &str, key: &str) -> Result<()> {
    let public = std::fs::read(key)
        .map_err(|e| Error::config(format!("could not read public key {key}: {e}")))?;
    let mut trust = load_trust()?;
    trust.trust(publisher, public);
    save_trust(&trust)?;
    println!("Trusting publisher `{publisher}`.");
    Ok(())
}

/// Read a package directory (`plugin.yaml` + `plugin.sig` + declared artifacts) into a
/// [`Package`], returning it alongside the parsed manifest.
fn read_package(dir: &str) -> Result<(Package, PluginManifest)> {
    let dir = Path::new(dir);
    let manifest_yaml = std::fs::read_to_string(dir.join("plugin.yaml"))
        .map_err(|e| Error::config(format!("could not read {}/plugin.yaml: {e}", dir.display())))?;
    let signature = std::fs::read(dir.join("plugin.sig")).map_err(|e| {
        Error::config(format!(
            "could not read {}/plugin.sig (sign the manifest with `apex plugin sign`): {e}",
            dir.display()
        ))
    })?;
    let manifest = PluginManifest::from_yaml(&manifest_yaml)?;
    let mut package = Package::new(manifest_yaml, signature);
    for artifact in &manifest.artifacts {
        let bytes = std::fs::read(dir.join(&artifact.path)).map_err(|e| {
            Error::config(format!(
                "could not read artifact {}/{}: {e}",
                dir.display(),
                artifact.path
            ))
        })?;
        package = package.with_artifact(artifact.path.clone(), bytes);
    }
    Ok((package, manifest))
}

/// `apex plugin install <dir> [--grant <perm>]` — read a package directory, verify its
/// signature, stage artifacts, and register it (disabled). `grants` must cover every
/// permission the manifest requests.
pub fn install_cmd(dir: &str, grants: Vec<String>) -> Result<()> {
    let (package, manifest) = read_package(dir)?;
    let mut engine = engine()?;
    let installed = engine.install(&package, &grants)?;
    let reference = installed.manifest.reference();
    let caps: Vec<String> = installed
        .manifest
        .capabilities
        .iter()
        .map(|c| c.id.clone())
        .collect();
    save_catalog(&engine.catalog())?;

    println!("Installed {reference} (disabled).");
    if !caps.is_empty() {
        println!("  capabilities: {}", caps.join(", "));
    }
    println!(
        "Enable it with `apex plugin enable {}`.",
        manifest.qualified_id()
    );
    Ok(())
}

/// `apex plugin upgrade <dir> [--grant <perm>]` — swap an installed plugin to the
/// version in the package, retaining the prior version for rollback. New permissions
/// must be granted; the upgrade is refused if it would break an installed dependent.
pub fn upgrade_cmd(dir: &str, grants: Vec<String>) -> Result<()> {
    let (package, manifest) = read_package(dir)?;
    let id = manifest.qualified_id();
    let mut engine = engine()?;
    let from = engine
        .get(&id)
        .map(|p| p.manifest.metadata.version.clone())
        .unwrap_or_default();
    let mut registry = ToolRegistry::new();
    engine.upgrade(&package, &grants, &mut registry)?;
    save_catalog(&engine.catalog())?;
    println!("Upgraded {id} {from} -> {}.", manifest.metadata.version);
    println!("Roll back with `apex plugin rollback {id}`.");
    Ok(())
}

/// `apex plugin rollback <id>` — revert a plugin to its retained previous version.
pub fn rollback_cmd(id: &str) -> Result<()> {
    let mut engine = engine()?;
    let mut registry = ToolRegistry::new();
    engine.rollback(id, &mut registry)?;
    save_catalog(&engine.catalog())?;
    let now = engine
        .get(id)
        .map(|p| p.manifest.metadata.version.clone())
        .unwrap_or_default();
    println!("Rolled back {id} to {now}.");
    Ok(())
}

/// `apex plugin run <capability> [--input <json>]` — invoke an enabled plugin tool
/// capability directly through the engine's runtime (an operator test path; the same
/// route an agent uses). Enforces the owning plugin's granted permissions. Requires the
/// CLI built with `--features plugin-wasi` for the capability to actually execute.
pub async fn run_cmd(capability: &str, input: &str) -> Result<()> {
    let engine = engine()?;
    let mut registry = ToolRegistry::new();
    engine.register_enabled(&mut registry);
    if !registry.contains(capability) {
        return Err(Error::config(format!(
            "no enabled plugin capability `{capability}` (install + enable a plugin that provides it)"
        )));
    }

    // Run with the owning plugin's granted permissions, so the registry's permission
    // check reflects what the operator consented to at install.
    let grants = engine.catalog().into_iter().find_map(|p| {
        p.manifest
            .capabilities
            .iter()
            .any(|c| c.id == capability)
            .then_some(p.granted_permissions)
    });
    let ctx = ToolContext {
        execution_id: format!("plugin-run-{capability}"),
        agent_id: "apex-cli".to_string(),
        workdir: ".".to_string(),
        granted_permissions: grants,
    };
    let params: Value =
        serde_json::from_str(input).unwrap_or_else(|_| Value::String(input.to_string()));

    let resp = registry
        .execute(capability, &ctx, ToolRequest::new(params))
        .await
        .map_err(|e| Error::Tool(format!("capability `{capability}` failed: {e}")))?;
    println!("{}", serde_json::to_string_pretty(&resp.payload)?);
    Ok(())
}

/// `apex plugin list` — show installed plugins with their state and capabilities.
pub fn list_cmd() -> Result<()> {
    let catalog = load_catalog()?;
    if catalog.is_empty() {
        println!("No plugins installed.");
        return Ok(());
    }
    for p in &catalog {
        let state = match p.state {
            PluginState::Enabled => "enabled",
            PluginState::Disabled => "disabled",
        };
        println!(
            "{}@{}  [{state}]",
            p.manifest.qualified_id(),
            p.manifest.metadata.version
        );
        for cap in &p.manifest.capabilities {
            let kind = match cap.kind {
                CapabilityKind::Tool => "tool",
                CapabilityKind::Provider => "provider",
                CapabilityKind::MemoryBackend => "memory_backend",
                CapabilityKind::Policy => "policy",
                CapabilityKind::WorkflowActivity => "workflow_activity",
            };
            println!("    - {kind}: {}", cap.id);
        }
    }
    Ok(())
}

/// `apex plugin enable <id>` — make a plugin's capabilities live (persisted).
pub fn enable_cmd(id: &str) -> Result<()> {
    let mut engine = engine()?;
    // The registry handed to enable is ephemeral here; the durable effect is the
    // state flip persisted to the catalog (agents rehydrate tools via register_enabled).
    let mut scratch = ToolRegistry::new();
    engine.enable(id, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    println!("Enabled {id}.");
    Ok(())
}

/// `apex plugin disable <id>` — withdraw a plugin's capabilities (state retained).
pub fn disable_cmd(id: &str) -> Result<()> {
    let mut engine = engine()?;
    let mut scratch = ToolRegistry::new();
    engine.disable(id, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    println!("Disabled {id}.");
    Ok(())
}

/// `apex plugin uninstall <id>` — withdraw and drop from the catalog, removing staged
/// artifacts.
pub fn uninstall_cmd(id: &str) -> Result<()> {
    let mut engine = engine()?;
    // Capture the staged dir before removal so we can clean it up afterwards.
    let staged = engine.get(id).and_then(|p| p.artifact_dir.clone());
    let mut scratch = ToolRegistry::new();
    engine.uninstall(id, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    if let Some(dir) = staged {
        let _ = std::fs::remove_dir_all(dir);
    }
    println!("Uninstalled {id}.");
    Ok(())
}

/// Restrict a private-key file to owner-only access where supported.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}
