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

#[cfg(feature = "plugin-wasi")]
use crate::config;
use apex_common::{Error, Result};
use apex_marketplace::{FileRegistryStore, Registry, RegistryStore, SearchQuery};
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
    apex_config::paths::plugins_dir()
}

/// Where verified artifacts are staged, `~/.apex/plugins/staging`.
fn staging_dir() -> Result<PathBuf> {
    apex_config::paths::staging_dir()
}

/// Acquire the cross-process advisory lock over `~/.apex/plugins` (RM-GA-P2
/// DUR-403), held for the duration of a mutating command. Every lifecycle
/// command here does load-trust/catalog → mutate the in-memory `PluginEngine`
/// → save-trust/catalog, against the same files the server's plugin routes
/// touch — without a lock spanning that whole sequence, a concurrent writer
/// (the CLI racing the server, or two CLI invocations) could act on the same
/// stale load and have its update silently clobbered by whichever saves last.
fn acquire_lock() -> Result<apex_common::fs::FileLock> {
    apex_common::fs::FileLock::acquire(plugins_dir()?)
        .map_err(|e| Error::config(format!("lock plugin store: {e}")))
}

fn trust_path() -> Result<PathBuf> {
    Ok(plugins_dir()?.join("trust.json"))
}

fn catalog_path() -> Result<PathBuf> {
    Ok(plugins_dir()?.join("catalog.json"))
}

fn keyless_path() -> Result<PathBuf> {
    Ok(plugins_dir()?.join("keyless.json"))
}

fn ca_key_path() -> Result<PathBuf> {
    Ok(plugins_dir()?.join("keyless-ca.key"))
}

/// The operator keyless-trust config ([ADR-0009]) at `~/.apex/plugins/keyless.json`
/// — the same file the server reads, so CLI and server verify identically.
#[derive(serde::Serialize, serde::Deserialize)]
struct KeylessConfig {
    root: apex_plugin::KeylessRoot,
    policy: apex_plugin::IdentityPolicy,
}

/// Load the keyless trust config (`None` if not configured — keyless disabled).
fn load_keyless() -> Result<Option<KeylessConfig>> {
    match std::fs::read(keyless_path()?) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Error::config(format!("corrupt keyless trust config: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
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
    apex_common::fs::atomic_write(trust_path()?, serde_json::to_vec_pretty(trust)?)?;
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
    apex_common::fs::atomic_write(catalog_path()?, serde_json::to_vec_pretty(catalog)?)?;
    Ok(())
}

/// Build an engine from the durable trust store + catalog, staging into
/// `~/.apex/plugins/staging`. With `--features plugin-wasi` it carries the real WASM
/// capability loader; otherwise capabilities register but error on call.
pub fn engine() -> Result<PluginEngine> {
    let trust = load_trust()?;
    let catalog = load_catalog()?;
    let mut engine = PluginEngine::new(platform_api(), trust)
        .with_staging_dir(staging_dir()?)
        .with_catalog(catalog);
    if let Some(k) = load_keyless()? {
        engine = engine.with_keyless(k.root, k.policy);
    }
    Ok(with_runtime(engine))
}

#[cfg(feature = "plugin-wasi")]
fn with_runtime(engine: PluginEngine) -> PluginEngine {
    match apex_plugin::WasiCapabilityRuntime::new() {
        // Make the runtime secret-aware over the local vault (`~/.apex/secrets`), so a
        // tenant-scoped run injects a plugin's `secret:read:<name>` grants into its sandbox.
        Ok(rt) => engine.with_runtime(std::sync::Arc::new(rt.with_secrets(secrets_vault()))),
        Err(_) => engine,
    }
}

/// The local secret vault over `~/.apex/secrets` (falls back to in-memory if the home
/// directory is unavailable). Encrypted-at-rest by default with the same
/// `APEX_SECRETS_PLAINTEXT=1` opt-out as the server's `default_secrets_vault`
/// (RM-AIM-P1 SEC-101) — both processes read the same directory, so they must agree
/// on which file (`secrets.json` vs `secrets.enc.json`) is live, or a plugin's
/// `secret:read:<name>` grant would silently fail to find a secret the server API
/// created (or vice versa). Both call the identical `apex-config` constructor, so
/// they agree by construction.
#[cfg(feature = "plugin-wasi")]
fn secrets_vault() -> apex_secrets::Vault {
    apex_config::secrets::build_secrets_vault(config::kms())
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

/// `apex plugin keyless-init [--allow "issuer|subject|publisher"]…` — set up keyless
/// trust ([ADR-0009]) on this node: generate the dev CA keypair
/// (`~/.apex/plugins/keyless-ca.key`) and write the pinned trust config
/// (`~/.apex/plugins/keyless.json`) with the given identity → publisher grants.
pub fn keyless_init_cmd(allow: Vec<String>) -> Result<()> {
    use apex_plugin::keyless as kl;

    let config_path = keyless_path()?;
    if config_path.exists() {
        return Err(Error::config(format!(
            "{} already exists — edit it directly, or delete it to re-init",
            config_path.display()
        )));
    }

    let rules = allow
        .iter()
        .map(|spec| {
            let parts: Vec<&str> = spec.split('|').collect();
            match parts.as_slice() {
                [issuer, subject, publisher] => Ok(kl::IdentityRule {
                    issuer: issuer.trim().to_string(),
                    subject: subject.trim().to_string(),
                    publisher: publisher.trim().to_string(),
                }),
                _ => Err(Error::config(format!(
                    "--allow must be `issuer|subject|publisher`, got `{spec}`"
                ))),
            }
        })
        .collect::<Result<Vec<_>>>()?;

    let (ca_pkcs8, ca_public_hex) = kl::generate_keypair()?;
    std::fs::create_dir_all(plugins_dir()?)?;
    let key_path = ca_key_path()?;
    std::fs::write(&key_path, &ca_pkcs8)?;
    restrict(&key_path);

    let config = KeylessConfig {
        root: apex_plugin::KeylessRoot {
            ca_public_keys: vec![ca_public_hex.clone()],
            log_public_keys: Vec::new(),
        },
        policy: apex_plugin::IdentityPolicy {
            allow: rules,
            // Dev-first default: bundles verify without a transparency log. Flip to
            // true (and pin the log key) once signing goes through `--rekor`.
            require_transparency: false,
        },
    };
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config)?)?;

    println!("Keyless trust initialized:");
    println!("  CA key:      {}  (keep secret)", key_path.display());
    println!("  CA public:   {ca_public_hex}");
    println!("  trust config: {}", config_path.display());
    if config.policy.allow.is_empty() {
        println!("No identities allowed yet (fail-closed) — add rules under `policy.allow`.");
    }
    println!(
        "Sign with `apex plugin keyless-sign --manifest <plugin.yaml> --issuer <i> --subject <s>`."
    );
    Ok(())
}

/// `apex plugin keyless-sign --manifest <plugin.yaml> --issuer <i> --subject <s>
/// [--rekor <url>] [--ca-key <path>]` — keyless-sign a manifest ([ADR-0009]): certify
/// a fresh ephemeral key with the CA, sign, optionally witness the event in a Rekor
/// transparency log, and write the bundle to `plugin.keyless.json` beside the
/// manifest (picked up by `pack`/`install`/`publish`). The ephemeral key never
/// touches disk.
pub fn keyless_sign_cmd(
    manifest: &str,
    issuer: &str,
    subject: &str,
    rekor: Option<String>,
    ca_key: Option<String>,
) -> Result<()> {
    use apex_plugin::keyless as kl;

    let ca_path = match &ca_key {
        Some(p) => PathBuf::from(p),
        None => ca_key_path()?,
    };
    let ca_pkcs8 = std::fs::read(&ca_path).map_err(|e| {
        Error::config(format!(
            "could not read CA key {} (run `apex plugin keyless-init` first): {e}",
            ca_path.display()
        ))
    })?;
    let manifest_bytes = std::fs::read(manifest)
        .map_err(|e| Error::config(format!("could not read manifest {manifest}: {e}")))?;
    let identity = kl::SignerIdentity {
        issuer: issuer.to_string(),
        subject: subject.to_string(),
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::config("system clock is before the epoch"))?
        .as_millis() as u64;

    // Run on a plain thread: the Rekor client is blocking HTTP, which must not run
    // on the async runtime the CLI dispatches from.
    let bundle = std::thread::spawn(move || -> Result<kl::KeylessBundle> {
        let ca = kl::InMemoryCa::from_pkcs8(&ca_pkcs8)?;
        let (ephemeral, _) = kl::generate_keypair()?;
        match rekor {
            None => kl::sign_keyless(&manifest_bytes, &identity, &ephemeral, &ca, None, now_ms),
            Some(url) => {
                let log = rekor_log(&url)?;
                kl::sign_keyless(
                    &manifest_bytes,
                    &identity,
                    &ephemeral,
                    &ca,
                    Some(log.as_ref()),
                    now_ms,
                )
            }
        }
    })
    .join()
    .map_err(|_| Error::Runtime("keyless signing thread panicked".into()))??;

    let out = Path::new(manifest)
        .parent()
        .map(|p| p.join("plugin.keyless.json"))
        .unwrap_or_else(|| PathBuf::from("plugin.keyless.json"));
    std::fs::write(&out, serde_json::to_vec_pretty(&bundle)?)?;

    println!(
        "Keyless-signed as {} (issuer {}).",
        bundle.cert.identity.subject, bundle.cert.identity.issuer
    );
    match &bundle.log_entry {
        Some(e) => println!(
            "Witnessed by transparency log: index {}, uuid {}.",
            e.log_index, e.uuid
        ),
        None => println!("No transparency log (pass --rekor <url> to witness the signing)."),
    }
    println!("Wrote bundle to {}", out.display());
    Ok(())
}

/// A Rekor-backed transparency log (behind the `keyless-rekor` cargo feature).
#[cfg(feature = "keyless-rekor")]
fn rekor_log(url: &str) -> Result<Box<dyn apex_plugin::keyless::TransparencyLog>> {
    Ok(Box::new(apex_plugin::rekor::RekorLog::new(url)))
}

#[cfg(not(feature = "keyless-rekor"))]
fn rekor_log(_url: &str) -> Result<Box<dyn apex_plugin::keyless::TransparencyLog>> {
    Err(Error::config(
        "this build has no Rekor client; rebuild with `--features keyless-rekor`",
    ))
}

/// `apex plugin trust <publisher> --key <pubkey-file>` — register a publisher's raw
/// ed25519 public key so its packages verify on install.
pub fn trust_cmd(publisher: &str, key: &str) -> Result<()> {
    let public = std::fs::read(key)
        .map_err(|e| Error::config(format!("could not read public key {key}: {e}")))?;
    let _lock = acquire_lock()?;
    let mut trust = load_trust()?;
    trust.trust(publisher, public);
    save_trust(&trust)?;
    println!("Trusting publisher `{publisher}`.");
    Ok(())
}

/// Load a plugin package from `source`: either a package **directory** (holding
/// `plugin.yaml`, `plugin.sig`, and artifacts) or a single **`.apexpkg`** file.
/// Returns the package alongside its parsed manifest.
fn load_package(source: &str) -> Result<(Package, PluginManifest)> {
    let path = Path::new(source);
    if path.is_file() {
        let bytes = std::fs::read(path)
            .map_err(|e| Error::config(format!("could not read {source}: {e}")))?;
        let package = Package::from_apexpkg(&bytes)?;
        let manifest = package.manifest()?;
        return Ok((package, manifest));
    }
    read_package_dir(path)
}

/// Read a package directory (`plugin.yaml` + `plugin.sig` and/or
/// `plugin.keyless.json` + declared artifacts). At least one signing mode must be
/// present: the detached publisher-key signature, or a keyless bundle ([ADR-0009]).
fn read_package_dir(dir: &Path) -> Result<(Package, PluginManifest)> {
    let manifest_yaml = std::fs::read_to_string(dir.join("plugin.yaml"))
        .map_err(|e| Error::config(format!("could not read {}/plugin.yaml: {e}", dir.display())))?;
    let keyless: Option<apex_plugin::KeylessBundle> =
        match std::fs::read(dir.join("plugin.keyless.json")) {
            Ok(bytes) => Some(serde_json::from_slice(&bytes).map_err(|e| {
                Error::config(format!(
                    "corrupt {}/plugin.keyless.json: {e}",
                    dir.display()
                ))
            })?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::Io(e)),
        };
    let signature = match std::fs::read(dir.join("plugin.sig")) {
        Ok(sig) => sig,
        // Keyless-only packages carry no publisher-key signature at all.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && keyless.is_some() => Vec::new(),
        Err(e) => {
            return Err(Error::config(format!(
                "could not read {}/plugin.sig (sign with `apex plugin sign` or \
                 `apex plugin keyless-sign`): {e}",
                dir.display()
            )));
        }
    };
    let manifest = PluginManifest::from_yaml(&manifest_yaml)?;
    let mut package = Package::new(manifest_yaml, signature);
    if let Some(bundle) = keyless {
        package = package.with_keyless(bundle);
    }
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

/// `apex plugin pack <dir> [--out <file.apexpkg>]` — bundle a package directory into a
/// single content-addressed `.apexpkg` file for distribution.
pub fn pack_cmd(dir: &str, out: Option<String>) -> Result<()> {
    let (package, manifest) = read_package_dir(Path::new(dir))?;
    let out = out.unwrap_or_else(|| {
        format!(
            "{}-{}.apexpkg",
            manifest.metadata.name, manifest.metadata.version
        )
    });
    std::fs::write(&out, package.to_apexpkg()?)?;
    println!("Packed {} -> {out}", manifest.reference());
    Ok(())
}

/// `apex plugin install <source> [--grant <perm>]` — install from a package directory
/// or a `.apexpkg` file: verify its signature, stage artifacts, and register it
/// (disabled). `grants` must cover every permission the manifest requests.
pub fn install_cmd(source: &str, grants: Vec<String>) -> Result<()> {
    let (package, manifest) = load_package(source)?;
    let _lock = acquire_lock()?;
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

/// `apex plugin upgrade <source> [--grant <perm>]` — swap an installed plugin to the
/// version in a package directory or `.apexpkg`, retaining the prior version for
/// rollback. New permissions must be granted; refused if it would break a dependent.
pub fn upgrade_cmd(source: &str, grants: Vec<String>) -> Result<()> {
    let (package, manifest) = load_package(source)?;
    let id = manifest.qualified_id();
    let _lock = acquire_lock()?;
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
    let _lock = acquire_lock()?;
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
        tenant: String::new(),
        granted_permissions: grants,
        egress_allowlist: None,
        // `plugin run` is an operator-invoked capability, already installed +
        // signature-verified — Verified, not the FirstParty default (SEC-305).
        trust_class: apex_tools::TrustClass::Verified,
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
    let _lock = acquire_lock()?;
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
    let _lock = acquire_lock()?;
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
    let _lock = acquire_lock()?;
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

// --- Marketplace -----------------------------------------------------------------
//
// The local registry mirrors the rest of the `apex plugin` surface: durable state
// under `~/.apex/marketplace/registry.json` by default, sharing the plugin trust store
// so only trusted publishers can list a plugin. Built with `--features postgres` and
// `APEX_MARKETPLACE_POSTGRES_URL` set, it shares a `PostgresRegistryStore` catalog with
// a `postgres`-built `apex-server` instead — so `apex plugin publish/search/get` see the
// same listings a running server does. (A remote registry server speaks the same
// `/api/v1/marketplace` routes.)

/// Open the durable registry store: `PostgresRegistryStore` when this binary was built
/// with the `postgres` feature *and* `APEX_MARKETPLACE_POSTGRES_URL` is set, else the
/// single-node `FileRegistryStore` at `~/.apex/marketplace/registry.json`.
fn open_store() -> Result<Box<dyn RegistryStore>> {
    #[cfg(feature = "postgres")]
    if let Some(url) = apex_config::env::marketplace_postgres_url() {
        let store = apex_marketplace::PostgresRegistryStore::connect(&url)?;
        return Ok(Box::new(store));
    }
    Ok(Box::new(FileRegistryStore::new(
        apex_config::paths::marketplace_dir()?.join("registry.json"),
    )))
}

/// A registry over the durable store, sharing the plugin trust store (and the
/// keyless trust config, when present).
fn marketplace_registry() -> Result<Registry<Box<dyn RegistryStore>>> {
    let store = open_store()?;
    let mut reg = Registry::new(store, load_trust()?);
    if let Some(k) = load_keyless()? {
        reg = reg.with_keyless(k.root, k.policy);
    }
    Ok(reg)
}

/// Runs a synchronous closure on a dedicated blocking-pool thread (RM-GA-P4/PRD-003
/// R-9.3, HLTH-902).
///
/// Every marketplace command below (`publish`/`search`/`get`/`report`/…) needs this:
/// the sync `postgres` crate's `Client` (used by `PostgresRegistryStore` when
/// `APEX_MARKETPLACE_POSTGRES_URL` is set) drives its own internal Tokio runtime for
/// *every* call including `connect`, which panics ("Cannot start a runtime from
/// within a runtime") if invoked directly — this binary's `main` is `#[tokio::main]`,
/// so a plain synchronous call from within `run()`'s async body still executes on a
/// runtime worker thread. This is the exact bug `apex-server`'s `marketplace.rs`
/// found and fixed with its own identically-shaped `with_registry` helper; this CLI
/// path never got the fix (undetectable locally — only Phase-2 CI-901's
/// `--features postgres` job exercises this code path at all).
async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(Error::Runtime(format!("blocking task panicked: {e}"))))
}

fn parse_capability_kind(s: &str) -> Option<CapabilityKind> {
    match s {
        "tool" => Some(CapabilityKind::Tool),
        "provider" => Some(CapabilityKind::Provider),
        "memory_backend" => Some(CapabilityKind::MemoryBackend),
        "policy" => Some(CapabilityKind::Policy),
        "workflow_activity" => Some(CapabilityKind::WorkflowActivity),
        _ => None,
    }
}

/// `apex plugin publish <source> [--channel <c>] [--category <cat>]` — publish a signed
/// package (directory or `.apexpkg`) to the local marketplace registry. The signature
/// must verify against a trusted publisher.
pub async fn publish_cmd(
    source: &str,
    channel: Option<String>,
    categories: Vec<String>,
) -> Result<()> {
    let source = source.to_string();
    blocking(move || {
        let (package, _manifest) = load_package(&source)?;
        let bytes = package.to_apexpkg()?;
        let out = marketplace_registry()?.publish(&bytes, &categories, channel.as_deref())?;
        println!("Published {} to channel `{}`.", out.reference, out.channel);
        if !out.scan.findings.is_empty() {
            println!("Security scan ({} finding(s)):", out.scan.findings.len());
            for f in &out.scan.findings {
                println!("  [{:?}] {}: {}", f.severity, f.code, f.message);
            }
        }
        println!("Find it with `apex plugin search {}`.", out.listing_id);
        Ok(())
    })
    .await
}

/// `apex plugin search [<query>] [--category <cat>] [--capability <kind>]` — discover
/// published listings in the marketplace registry.
pub async fn search_cmd(
    query: Option<String>,
    category: Option<String>,
    capability: Option<String>,
) -> Result<()> {
    blocking(move || {
        let listings = marketplace_registry()?.search(&SearchQuery {
            text: query.unwrap_or_default(),
            category,
            capability: capability.as_deref().and_then(parse_capability_kind),
        })?;
        if listings.is_empty() {
            println!("No matching listings.");
            return Ok(());
        }
        for l in &listings {
            let rating = l
                .rating
                .map(|r| format!("{r:.1}*"))
                .unwrap_or_else(|| "unrated".to_string());
            let verified = if l.verified { " (verified)" } else { "" };
            let latest = l.versions.first().map(String::as_str).unwrap_or("?");
            println!(
                "{}  v{latest}  [{rating}, {} installs]{verified}",
                l.id, l.installs
            );
            if !l.description.is_empty() {
                println!("    {}", l.description);
            }
            if !l.categories.is_empty() {
                println!("    categories: {}", l.categories.join(", "));
            }
            if !l.permissions.is_empty() {
                println!("    permissions: {}", l.permissions.join(", "));
            }
        }
        Ok(())
    })
    .await
}

/// `apex plugin get <id> [--version <v>] [--grant <perm>]` — download a listed package
/// from the marketplace and install it locally (disabled). `id` is `publisher/name`.
pub async fn market_install_cmd(
    id: &str,
    version: Option<String>,
    grants: Vec<String>,
) -> Result<()> {
    let id = id.to_string();
    blocking(move || {
        let reg = marketplace_registry()?;
        let bytes = reg.download(&id, version.as_deref())?;
        let package = Package::from_apexpkg(&bytes)?;
        let _lock = acquire_lock()?;
        let mut engine = engine()?;
        let installed = engine.install(&package, &grants)?;
        let reference = installed.manifest.reference();
        save_catalog(&engine.catalog())?;
        reg.record_install(&id)?;
        println!("Installed {reference} from the marketplace (disabled).");
        println!("Enable it with `apex plugin enable {id}`.");
        Ok(())
    })
    .await
}

/// `apex plugin report <id> <reason> [--reporter <name>]` — file an abuse report
/// against a marketplace listing (malware, IP infringement, deceptive metadata,
/// etc.). Feeds moderation and can trigger delisting via `resolve-abuse`.
pub async fn report_abuse_cmd(id: &str, reason: &str, reporter: Option<String>) -> Result<()> {
    let id = id.to_string();
    let reason = reason.to_string();
    blocking(move || {
        let reporter = reporter.unwrap_or_else(|| "anonymous".to_string());
        let report_id = marketplace_registry()?.report_abuse(&id, &reporter, &reason)?;
        println!("Filed report #{report_id} against {id}.");
        println!("List open reports with `apex plugin reports {id}`.");
        Ok(())
    })
    .await
}

/// `apex plugin reports <id>` — list the abuse reports filed against a marketplace
/// listing, for moderator review.
pub async fn list_abuse_reports_cmd(id: &str) -> Result<()> {
    let id = id.to_string();
    blocking(move || {
        let reports = marketplace_registry()?.abuse_reports(&id)?;
        if reports.is_empty() {
            println!("No abuse reports against {id}.");
            return Ok(());
        }
        for r in &reports {
            println!(
                "#{}  reporter: {}  reason: {}  status: {:?}",
                r.id, r.reporter, r.reason, r.status
            );
        }
        Ok(())
    })
    .await
}

/// `apex plugin resolve-abuse <id> <report-id> [--delist] [--moderator <name>]` — a
/// moderator resolves an open abuse report as valid, optionally delisting the
/// listing (removing it from discovery/download).
pub async fn resolve_abuse_cmd(
    id: &str,
    report_id: u64,
    delist: bool,
    moderator: Option<String>,
) -> Result<()> {
    let id = id.to_string();
    blocking(move || {
        let moderator = moderator.unwrap_or_else(|| "operator".to_string());
        marketplace_registry()?.resolve_abuse_report(&id, report_id, &moderator, delist)?;
        if delist {
            println!("Resolved report #{report_id} against {id}: listing delisted.");
        } else {
            println!("Resolved report #{report_id} against {id} (not delisted).");
        }
        Ok(())
    })
    .await
}

/// `apex plugin dismiss-abuse <id> <report-id> <reason> [--moderator <name>]` — a
/// moderator dismisses an open abuse report as not actionable.
pub async fn dismiss_abuse_cmd(
    id: &str,
    report_id: u64,
    reason: &str,
    moderator: Option<String>,
) -> Result<()> {
    let id = id.to_string();
    let reason = reason.to_string();
    blocking(move || {
        let moderator = moderator.unwrap_or_else(|| "operator".to_string());
        marketplace_registry()?.dismiss_abuse_report(&id, report_id, &moderator, &reason)?;
        println!("Dismissed report #{report_id} against {id}.");
        Ok(())
    })
    .await
}

/// Restrict a private-key file to owner-only access where supported.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}
