//! The WASM capability loader — executes a plugin's `tool` capability in the WASI
//! sandbox ([overview §5.5 Isolation](../../docs/08-plugin-sdk/overview.md#55-isolation),
//! [sandbox §loading model](../../docs/08-plugin-sdk/sandbox.md)).
//!
//! This is the concrete [`CapabilityRuntime`](crate::CapabilityRuntime) behind the
//! engine's [`NotLoadedRuntime`](crate::NotLoadedRuntime) placeholder: it loads the
//! capability's staged `wasm32-wasi` module and runs it in an in-process Wasmtime VM
//! (memory/fuel/epoch-limited, no ambient network or filesystem).
//!
//! **Capability ABI.** A WASM tool capability is a `wasm32-wasi` command whose
//! [`entry`](crate::CapabilityCall::entry) names its module (relative to the plugin's
//! staged [`artifact_dir`](crate::CapabilityCall::artifact_dir)). On invocation the
//! loader writes the request parameters as JSON to the guest's **stdin** and reads the
//! response as JSON from its **stdout**. A clean exit (`0`) with parseable JSON is a
//! success; a non-zero exit, a resource-budget breach, a timeout, or non-JSON output
//! is a [`ToolError`].
//!
//! Requires the `wasi` cargo feature (pulls Wasmtime).

use crate::engine::{CapabilityCall, CapabilityRuntime};
use apex_tools::{
    ResourceLimits, SandboxCommand, ToolError, ToolRequest, ToolResponse, WasiSandbox,
};
use async_trait::async_trait;
use serde_json::Value;

/// A [`CapabilityRuntime`] that runs WASM tool capabilities in the WASI sandbox.
#[derive(Clone)]
pub struct WasiCapabilityRuntime {
    sandbox: WasiSandbox,
    limits: ResourceLimits,
    secrets: Option<apex_secrets::Vault>,
}

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
        let dir = call.artifact_dir.ok_or_else(|| {
            ToolError::Internal(format!(
                "capability `{}` has no staged artifacts (configure the engine with a staging dir)",
                call.capability_id
            ))
        })?;
        if call.entry.is_empty() {
            return Err(ToolError::Validation(format!(
                "WASM capability `{}` declares no `entry` module",
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

        if outcome.timed_out {
            return Err(ToolError::Internal(format!(
                "capability `{}` timed out",
                call.capability_id
            )));
        }
        if outcome.resource_exceeded {
            return Err(ToolError::Internal(format!(
                "capability `{}` exceeded its resource budget",
                call.capability_id
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
                            "capability `{}` returned non-JSON output: {e}",
                            call.capability_id
                        ))
                    })?
                };
                Ok(ToolResponse::success(payload))
            }
            other => Err(ToolError::Internal(format!(
                "capability `{}` failed (exit {:?}): {}",
                call.capability_id,
                other,
                outcome.stderr.trim()
            ))),
        }
    }
}

#[cfg(test)]
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
