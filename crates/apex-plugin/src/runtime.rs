//! The capability loaders — execute a plugin's `tool` capability in an isolation
//! sandbox ([overview §5.5 Isolation](../../docs/08-plugin-sdk/overview.md#55-isolation),
//! [sandbox §loading model](../../docs/08-plugin-sdk/sandbox.md)).
//!
//! These are the concrete [`CapabilityRuntime`](crate::CapabilityRuntime)s behind the
//! engine's [`NotLoadedRuntime`](crate::NotLoadedRuntime) placeholder:
//!
//! - [`WasiCapabilityRuntime`] (feature `wasi` — pulls Wasmtime) loads the
//!   capability's staged `wasm32-wasi` module and runs it in an in-process Wasmtime
//!   VM (memory/fuel/epoch-limited, no ambient network or filesystem).
//! - [`ContainerCapabilityRuntime`] (ECO-303, always compiled) runs the capability's
//!   staged entry as a native executable in an OCI container
//!   ([`ContainerSandbox`]: cgroup limits, read-only rootfs, deny-all egress by
//!   default; gVisor via [`ContainerCapabilityRuntime::gvisor`]), with the plugin's
//!   artifact dir bind-mounted at `/workspace`.
//!
//! **Capability ABI** (identical across loaders). A tool capability is a command
//! whose [`entry`](crate::CapabilityCall::entry) names its artifact (relative to the
//! plugin's staged [`artifact_dir`](crate::CapabilityCall::artifact_dir)). On
//! invocation the loader writes the request parameters as JSON to the guest's
//! **stdin** and reads the response as JSON from its **stdout**. A clean exit (`0`)
//! with parseable JSON is a success; a non-zero exit, a resource-budget breach, a
//! timeout, or non-JSON output is a [`ToolError`]. Declared `secret:read:<name>`
//! permissions resolve through an attached vault into `APEX_SECRET_*` env vars.
//!
//! A capability selects its loader through the manifest's `sandbox` field: `wasm`
//! (or empty) routes to the WASM loader, `container`/`gvisor` to the container
//! loader — each loader refuses the other's capabilities fail-closed, and a
//! `gvisor` capability is refused by a plain-Docker runtime rather than silently
//! demoted to weaker isolation.

use crate::engine::{CapabilityCall, CapabilityRuntime};
#[cfg(feature = "wasi")]
use apex_tools::WasiSandbox;
use apex_tools::{
    CommandOutcome, ContainerSandbox, NetworkPolicy, ResourceLimits, Sandbox, SandboxBackend,
    SandboxCommand, ToolError, ToolRequest, ToolResponse,
};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;

/// Resolve a capability's staged entry to `(artifact_dir, module_path)`, failing
/// closed on the mispackaging cases every loader shares: no staging dir configured,
/// no declared `entry`, or an entry missing from the staged package.
fn staged_entry(call: &CapabilityCall<'_>) -> Result<(PathBuf, PathBuf), ToolError> {
    let dir = call.artifact_dir.ok_or_else(|| {
        ToolError::Internal(format!(
            "capability `{}` has no staged artifacts (configure the engine with a staging dir)",
            call.capability_id
        ))
    })?;
    if call.entry.is_empty() {
        return Err(ToolError::Validation(format!(
            "capability `{}` declares no `entry` artifact",
            call.capability_id
        )));
    }
    let module = dir.join(call.entry);
    if !module.is_file() {
        return Err(ToolError::Internal(format!(
            "capability `{}` module `{}` is missing from the staged package",
            call.capability_id,
            module.display()
        )));
    }
    Ok((dir.to_path_buf(), module))
}

/// Map a sandbox outcome to the capability ABI's response: a clean exit with JSON
/// (or empty = null) stdout succeeds; everything else is a clear [`ToolError`].
fn capability_response(
    capability_id: &str,
    outcome: CommandOutcome,
) -> Result<ToolResponse, ToolError> {
    if outcome.timed_out {
        return Err(ToolError::Internal(format!(
            "capability `{capability_id}` timed out"
        )));
    }
    if outcome.resource_exceeded {
        return Err(ToolError::Internal(format!(
            "capability `{capability_id}` exceeded its resource budget"
        )));
    }
    match outcome.exit_code {
        Some(0) => {
            // Empty output is a valid null result; otherwise parse stdout as JSON.
            let payload: Value = if outcome.stdout.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str(outcome.stdout.trim()).map_err(|e| {
                    ToolError::Internal(format!(
                        "capability `{capability_id}` returned non-JSON output: {e}"
                    ))
                })?
            };
            Ok(ToolResponse::success(payload))
        }
        other => Err(ToolError::Internal(format!(
            "capability `{capability_id}` failed (exit {:?}): {}",
            other,
            outcome.stderr.trim()
        ))),
    }
}

/// A [`CapabilityRuntime`] that runs native tool capabilities in an OCI container
/// (ECO-303). The staged artifact dir is bind-mounted at `/workspace` and the entry
/// executes inside `image` under the container's cgroup limits, read-only rootfs,
/// and (by default) deny-all egress. Same stdin/stdout JSON ABI as the WASM loader;
/// the entry must be runnable in `image` (a static binary, or a script whose
/// interpreter the image ships).
#[derive(Clone)]
pub struct ContainerCapabilityRuntime {
    sandbox: ContainerSandbox,
    limits: ResourceLimits,
    secrets: Option<apex_secrets::Vault>,
}

impl ContainerCapabilityRuntime {
    /// A loader running capabilities in Docker containers of `image`.
    pub fn docker(image: impl Into<String>) -> Self {
        Self::with_sandbox(ContainerSandbox::docker(image))
    }

    /// A loader running capabilities in Podman containers of `image`.
    pub fn podman(image: impl Into<String>) -> Self {
        Self::with_sandbox(ContainerSandbox::podman(image))
    }

    /// A loader running capabilities under gVisor (`--runtime=runsc`) in `image` —
    /// the only construction that accepts capabilities declaring `sandbox: gvisor`.
    pub fn gvisor(image: impl Into<String>) -> Self {
        Self::with_sandbox(ContainerSandbox::gvisor(image))
    }

    fn with_sandbox(sandbox: ContainerSandbox) -> Self {
        Self {
            sandbox,
            limits: ResourceLimits::default(),
            secrets: None,
        }
    }

    /// Set the per-invocation resource limits (timeout, memory, CPU, pids).
    pub fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Override the egress policy (default: deny-all → `--network none`).
    pub fn with_network(mut self, network: NetworkPolicy) -> Self {
        self.sandbox = self.sandbox.with_network(network);
        self
    }

    /// Resolve a capability's declared `secret:read:<name>` permissions from `vault`
    /// and inject the values as `APEX_SECRET_<NAME>` env vars at invocation
    /// ([secret-management §5](../../docs/13-security/secret-management.md#5-injection-into-tools--plugins)).
    /// Values travel via the container CLI's process environment, never its argv.
    pub fn with_secrets(mut self, vault: apex_secrets::Vault) -> Self {
        self.secrets = Some(vault);
        self
    }
}

#[async_trait]
impl CapabilityRuntime for ContainerCapabilityRuntime {
    async fn invoke(
        &self,
        call: &CapabilityCall<'_>,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        // This loader only runs container-family capabilities; and a capability that
        // asked for gVisor must never silently run with weaker (plain-container)
        // isolation.
        match call.sandbox {
            "container" => {}
            "gvisor" if self.sandbox.backend() == SandboxBackend::Gvisor => {}
            "gvisor" => {
                return Err(ToolError::Internal(format!(
                    "capability `{}` requires gVisor isolation but this runtime drives \
                     plain containers — construct `ContainerCapabilityRuntime::gvisor`",
                    call.capability_id
                )));
            }
            other => {
                return Err(ToolError::Internal(format!(
                    "ContainerCapabilityRuntime cannot run capability `{}`: sandbox `{}` \
                     is not a container backend",
                    call.capability_id, other
                )));
            }
        }
        let (dir, module) = staged_entry(call)?;
        // Docker needs an absolute host path for the bind mount.
        let dir = dir.canonicalize().map_err(|e| {
            ToolError::Internal(format!(
                "capability `{}`: cannot resolve staging dir: {e}",
                call.capability_id
            ))
        })?;

        // Artifact staging writes content bytes only — no mode bits survive
        // packaging — so mark the entry executable for the container to run it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&module)
                .map_err(|e| {
                    ToolError::Internal(format!(
                        "capability `{}`: stat entry: {e}",
                        call.capability_id
                    ))
                })?
                .permissions();
            if perms.mode() & 0o111 == 0 {
                perms.set_mode(perms.mode() | 0o555);
                std::fs::set_permissions(&module, perms).map_err(|e| {
                    ToolError::Internal(format!(
                        "capability `{}`: mark entry executable: {e}",
                        call.capability_id
                    ))
                })?;
            }
        }
        #[cfg(not(unix))]
        let _ = &module;

        // Request parameters → guest stdin (JSON).
        let stdin = serde_json::to_vec(&request.parameters)
            .map_err(|e| ToolError::Internal(format!("encode request: {e}")))?;

        // Resolve the secrets this capability is entitled to (within the caller's
        // tenant); injected per-invocation, dropped with the command on teardown.
        let env = match &self.secrets {
            Some(vault) => crate::engine::resolve_secret_env(
                call.declared_permissions,
                &call.ctx.tenant,
                vault,
            )?,
            None => Vec::new(),
        };

        let cmd = SandboxCommand {
            program: format!("/workspace/{}", call.entry),
            args: Vec::new(),
            workdir: dir.to_string_lossy().into_owned(),
            env,
            limits: self.limits.clone(),
        };

        let outcome = self
            .sandbox
            .execute_with_stdin(&cmd, stdin)
            .await
            .map_err(|e| ToolError::Internal(format!("container execution failed: {e}")))?;
        capability_response(call.capability_id, outcome)
    }
}

/// A [`CapabilityRuntime`] that runs WASM tool capabilities in the WASI sandbox.
#[cfg(feature = "wasi")]
#[derive(Clone)]
pub struct WasiCapabilityRuntime {
    sandbox: WasiSandbox,
    limits: ResourceLimits,
    secrets: Option<apex_secrets::Vault>,
}

#[cfg(feature = "wasi")]
impl WasiCapabilityRuntime {
    /// A loader with a fresh WASI engine and default resource limits.
    pub fn new() -> Result<Self, ToolError> {
        let sandbox = WasiSandbox::new()
            .map_err(|e| ToolError::Internal(format!("init wasi sandbox: {e}")))?;
        Ok(Self {
            sandbox,
            limits: ResourceLimits::default(),
            secrets: None,
        })
    }

    /// Set the per-invocation resource limits (timeout, memory, CPU/fuel).
    pub fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Resolve a capability's declared `secret:read:<name>` permissions from `vault` and
    /// inject the values into the sandbox as `APEX_SECRET_<NAME>` env vars at invocation
    /// ([secret-management §5](../../docs/13-security/secret-management.md#5-injection-into-tools--plugins)).
    pub fn with_secrets(mut self, vault: apex_secrets::Vault) -> Self {
        self.secrets = Some(vault);
        self
    }
}

#[cfg(feature = "wasi")]
#[async_trait]
impl CapabilityRuntime for WasiCapabilityRuntime {
    async fn invoke(
        &self,
        call: &CapabilityCall<'_>,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        // This loader only runs WASM; route anything else back as a clear error.
        if !matches!(call.sandbox, "wasm" | "wasi" | "") {
            return Err(ToolError::Internal(format!(
                "WasiCapabilityRuntime cannot run capability `{}`: sandbox `{}` is not WASM",
                call.capability_id, call.sandbox
            )));
        }
        let (dir, module) = staged_entry(call)?;

        // Request parameters → guest stdin (JSON).
        let stdin = serde_json::to_vec(&request.parameters)
            .map_err(|e| ToolError::Internal(format!("encode request: {e}")))?;

        // Resolve the secrets this capability is entitled to (within the caller's tenant)
        // and inject them as env vars — in memory, dropped with the command on teardown.
        let env = match &self.secrets {
            Some(vault) => crate::engine::resolve_secret_env(
                call.declared_permissions,
                &call.ctx.tenant,
                vault,
            )?,
            None => Vec::new(),
        };

        let cmd = SandboxCommand {
            program: module.to_string_lossy().into_owned(),
            args: Vec::new(),
            workdir: dir.to_string_lossy().into_owned(),
            env,
            limits: self.limits.clone(),
        };

        let outcome = self
            .sandbox
            .execute_with_stdin(&cmd, stdin)
            .await
            .map_err(|e| ToolError::Internal(format!("wasm execution failed: {e}")))?;
        capability_response(call.capability_id, outcome)
    }
}

#[cfg(all(test, feature = "wasi"))]
mod tests {
    use super::*;
    use crate::engine::{Package, PluginEngine};
    use crate::verify::TrustStore;
    use crate::verify::testing::{generate_keypair, sign};
    use apex_tools::{ToolContext, ToolRegistry};
    use serde_json::json;

    /// A `wasm32-wasi` module that echoes stdin → stdout: it reads up to 900 bytes of
    /// the request from fd 0 and writes exactly `nread` bytes back to fd 1. Used to
    /// prove the request-in / response-out round trip end to end.
    const ECHO_WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_read"
            (func $fd_read (param i32 i32 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "_start")
            ;; read iovec @0 → {buf=100, len=900}
            (i32.store (i32.const 0) (i32.const 100))
            (i32.store (i32.const 4) (i32.const 900))
            ;; fd_read(fd=0, iovs=0, iovs_len=1, nread=8)
            (drop (call $fd_read (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 8)))
            ;; write iovec @16 → {buf=100, len=mem[8] (bytes read)}
            (i32.store (i32.const 16) (i32.const 100))
            (i32.store (i32.const 20) (i32.load (i32.const 8)))
            ;; fd_write(fd=1, iovs=16, iovs_len=1, nwritten=24)
            (drop (call $fd_write (i32.const 1) (i32.const 16) (i32.const 1) (i32.const 24)))))
    "#;

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bytes);
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Build a signed package whose `tool` capability's entry is a staged WASM module,
    /// and an engine (trusting the publisher) staging into `staging`.
    fn wasm_plugin(staging: &std::path::Path) -> (Package, PluginEngine) {
        let wasm = wat::parse_str(ECHO_WAT).expect("assemble echo wat");
        let manifest = format!(
            r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata:
  name: echo
  version: 0.1.0
  publisher: acme
permissions: []
capabilities:
  - kind: tool
    id: echo.run
    entry: echo.wasm
    sandbox: wasm
artifacts:
  - path: echo.wasm
    digest: "sha256:{}"
"#,
            sha256_hex(&wasm)
        );
        let (kp, public) = generate_keypair();
        let mut trust = TrustStore::new();
        trust.trust("acme", public);
        let sig = sign(&kp, manifest.as_bytes());
        let package = Package::new(manifest, sig).with_artifact("echo.wasm", wasm);
        let engine = PluginEngine::new(semver::Version::new(1, 0, 0), trust)
            .with_runtime(std::sync::Arc::new(WasiCapabilityRuntime::new().unwrap()))
            .with_staging_dir(staging);
        (package, engine)
    }

    #[tokio::test]
    async fn wasm_capability_round_trips_request_to_response() {
        let staging = std::env::temp_dir().join(format!("apex_plugin_wasi_{}", std::process::id()));
        let (package, mut engine) = wasm_plugin(&staging);
        let mut registry = ToolRegistry::new();

        engine.install(&package, &[]).unwrap();
        engine.enable("acme/echo", &mut registry).unwrap();

        // The echo module returns whatever JSON it was sent on stdin.
        let resp = registry
            .execute(
                "echo.run",
                &ToolContext::default(),
                ToolRequest::new(json!({"hello": "world", "n": 42})),
            )
            .await
            .unwrap();
        assert!(resp.success);
        assert_eq!(resp.payload, json!({"hello": "world", "n": 42}));

        let _ = std::fs::remove_dir_all(&staging);
    }

    #[tokio::test]
    async fn missing_staging_dir_fails_closed() {
        // An engine with the wasi runtime but no staging dir → nothing to load.
        let wasm = wat::parse_str(ECHO_WAT).unwrap();
        let manifest = format!(
            r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata:
  name: echo
  version: 0.1.0
  publisher: acme
capabilities:
  - kind: tool
    id: echo.run
    entry: echo.wasm
    sandbox: wasm
artifacts:
  - path: echo.wasm
    digest: "sha256:{}"
"#,
            sha256_hex(&wasm)
        );
        let (kp, public) = generate_keypair();
        let mut trust = TrustStore::new();
        trust.trust("acme", public);
        let sig = sign(&kp, manifest.as_bytes());
        let package = Package::new(manifest, sig).with_artifact("echo.wasm", wasm);
        let mut engine = PluginEngine::new(semver::Version::new(1, 0, 0), trust)
            .with_runtime(std::sync::Arc::new(WasiCapabilityRuntime::new().unwrap()));
        let mut registry = ToolRegistry::new();
        engine.install(&package, &[]).unwrap();
        engine.enable("acme/echo", &mut registry).unwrap();

        let err = registry
            .execute(
                "echo.run",
                &ToolContext::default(),
                ToolRequest::new(json!({})),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Internal(m) if m.contains("no staged artifacts")),
            "got {err:?}"
        );
    }
}

/// The container loader's tests: fail-closed checks that need no container runtime
/// (every one refuses *before* anything could spawn), plus the ECO-303 end-to-end
/// runs, capability-gated on a real Docker daemon exactly like
/// `apex-tools/tests/sandbox_backends.rs` (probe, log a skip, return early).
#[cfg(test)]
mod container_tests {
    use super::*;
    use crate::engine::{Package, PluginEngine};
    use crate::verify::TrustStore;
    use crate::verify::testing::{generate_keypair, sign};
    use apex_tools::{SandboxManager, ToolContext, ToolRegistry, ToolRequest};
    use serde_json::json;

    /// Whether this host can run `backend`, logging a skip when it can't.
    async fn has(backend: SandboxBackend) -> bool {
        let available = SandboxManager::detect()
            .await
            .capabilities()
            .contains(&backend);
        if !available {
            eprintln!("skipping: {backend} backend not available on this host");
        }
        available
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bytes);
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A capability entry the default sandbox image can run: a `/bin/sh` script that
    /// echoes the stdin request back under `echo` and proves secret injection by
    /// including its `APEX_SECRET_API_TOKEN` env var.
    const ECHO_SH: &str = "#!/bin/sh\n\
        IN=$(cat)\n\
        printf '{\"echo\":%s,\"token\":\"%s\"}' \"$IN\" \"$APEX_SECRET_API_TOKEN\"\n";

    /// Build a signed package whose `tool` capability is the shell script above, and
    /// an engine (trusting the publisher, staging into `staging`) driving `runtime`.
    fn container_plugin(
        staging: &std::path::Path,
        sandbox: &str,
        runtime: ContainerCapabilityRuntime,
    ) -> (Package, PluginEngine) {
        let manifest = format!(
            r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata:
  name: cecho
  version: 0.1.0
  publisher: acme
permissions:
  - secret:read:api-token
capabilities:
  - kind: tool
    id: cecho.run
    entry: cecho.sh
    sandbox: {sandbox}
artifacts:
  - path: cecho.sh
    digest: "sha256:{}"
"#,
            sha256_hex(ECHO_SH.as_bytes())
        );
        let (kp, public) = generate_keypair();
        let mut trust = TrustStore::new();
        trust.trust("acme", public);
        let sig = sign(&kp, manifest.as_bytes());
        let package =
            Package::new(manifest, sig).with_artifact("cecho.sh", ECHO_SH.as_bytes().to_vec());
        let engine = PluginEngine::new(semver::Version::new(1, 0, 0), trust)
            .with_runtime(std::sync::Arc::new(runtime))
            .with_staging_dir(staging);
        (package, engine)
    }

    fn vault_with_token() -> apex_secrets::Vault {
        let store = std::sync::Arc::new(apex_secrets::InMemorySecretStore::new());
        let vault = apex_secrets::Vault::new(store);
        vault.create("acme", "api-token", "s3cr3t-t0ken").unwrap();
        vault
    }

    /// ECO-303 acceptance: a container-backed capability runs end to end — install →
    /// enable → execute through the registry, with the request round-tripping over
    /// stdin/stdout and the declared secret injected as an env var.
    #[tokio::test]
    async fn container_capability_runs_end_to_end() {
        if !has(SandboxBackend::Container).await {
            return;
        }
        let staging =
            std::env::temp_dir().join(format!("apex_plugin_container_{}", std::process::id()));
        let runtime =
            ContainerCapabilityRuntime::docker("alpine:3.20").with_secrets(vault_with_token());
        let (package, mut engine) = container_plugin(&staging, "container", runtime);
        let mut registry = ToolRegistry::new();
        engine
            .install(&package, &["secret:read:api-token".to_string()])
            .unwrap();
        engine.enable("acme/cecho", &mut registry).unwrap();

        let ctx = ToolContext {
            tenant: "acme".to_string(),
            ..ToolContext::default()
        };
        let resp = registry
            .execute(
                "cecho.run",
                &ctx,
                ToolRequest::new(json!({"hello": "world", "n": 42})),
            )
            .await
            .unwrap();
        assert!(resp.success);
        assert_eq!(
            resp.payload,
            json!({"echo": {"hello": "world", "n": 42}, "token": "s3cr3t-t0ken"})
        );

        let _ = std::fs::remove_dir_all(&staging);
    }

    /// The same capability declared `sandbox: gvisor` runs under the gVisor-backed
    /// runtime (which the docker-backed loader would have refused — see the
    /// fail-closed test above).
    #[tokio::test]
    async fn gvisor_capability_runs_end_to_end() {
        if !has(SandboxBackend::Gvisor).await {
            return;
        }
        let staging =
            std::env::temp_dir().join(format!("apex_plugin_gvisor_{}", std::process::id()));
        let runtime =
            ContainerCapabilityRuntime::gvisor("alpine:3.20").with_secrets(vault_with_token());
        let (package, mut engine) = container_plugin(&staging, "gvisor", runtime);
        let mut registry = ToolRegistry::new();
        engine
            .install(&package, &["secret:read:api-token".to_string()])
            .unwrap();
        engine.enable("acme/cecho", &mut registry).unwrap();

        let ctx = ToolContext {
            tenant: "acme".to_string(),
            ..ToolContext::default()
        };
        let resp = registry
            .execute("cecho.run", &ctx, ToolRequest::new(json!({"k": "v"})))
            .await
            .unwrap();
        assert!(resp.success);
        assert_eq!(
            resp.payload,
            json!({"echo": {"k": "v"}, "token": "s3cr3t-t0ken"})
        );

        let _ = std::fs::remove_dir_all(&staging);
    }

    fn call<'a>(
        sandbox: &'a str,
        entry: &'a str,
        dir: Option<&'a std::path::Path>,
        ctx: &'a ToolContext,
    ) -> CapabilityCall<'a> {
        CapabilityCall {
            plugin: "acme/echo@0.1.0",
            capability_id: "echo.run",
            entry,
            sandbox,
            artifact_dir: dir,
            declared_permissions: &[],
            ctx,
        }
    }

    #[tokio::test]
    async fn non_container_sandbox_is_refused() {
        let ctx = ToolContext::default();
        let rt = ContainerCapabilityRuntime::docker("alpine:3.20");
        let err = rt
            .invoke(
                &call("wasm", "echo.wasm", None, &ctx),
                ToolRequest::new(serde_json::json!({})),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Internal(m) if m.contains("not a container backend")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn gvisor_capability_is_refused_by_a_plain_docker_runtime() {
        // A capability that declared gVisor isolation must never silently run in a
        // weaker plain container.
        let ctx = ToolContext::default();
        let rt = ContainerCapabilityRuntime::docker("alpine:3.20");
        let err = rt
            .invoke(
                &call("gvisor", "tool.sh", None, &ctx),
                ToolRequest::new(serde_json::json!({})),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Internal(m) if m.contains("gVisor")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn missing_staging_dir_fails_closed() {
        let ctx = ToolContext::default();
        let rt = ContainerCapabilityRuntime::docker("alpine:3.20");
        let err = rt
            .invoke(
                &call("container", "tool.sh", None, &ctx),
                ToolRequest::new(serde_json::json!({})),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Internal(m) if m.contains("no staged artifacts")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn empty_entry_and_missing_module_fail_closed() {
        let ctx = ToolContext::default();
        let rt = ContainerCapabilityRuntime::docker("alpine:3.20");
        let dir = std::env::temp_dir();

        let err = rt
            .invoke(
                &call("container", "", Some(&dir), &ctx),
                ToolRequest::new(serde_json::json!({})),
            )
            .await
            .unwrap_err();
        assert!(matches!(&err, ToolError::Validation(_)), "got {err:?}");

        let err = rt
            .invoke(
                &call(
                    "container",
                    "definitely_not_staged_here.sh",
                    Some(&dir),
                    &ctx,
                ),
                ToolRequest::new(serde_json::json!({})),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Internal(m) if m.contains("missing from the staged package")),
            "got {err:?}"
        );
    }
}
