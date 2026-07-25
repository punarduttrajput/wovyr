//! [`WasiSandbox`] — an in-process Wasmtime VM running `wasm32-wasi` modules.
//!
//! This whole module is gated by the crate's `wasi` cargo feature (see the
//! `#[cfg(feature = "wasi")] mod wasi;` declaration in `sandbox/mod.rs`), so
//! nothing in this file needs its own per-item `#[cfg(feature = "wasi")]`.

use super::Sandbox;
use super::cap;
use super::types::{CommandOutcome, SandboxBackend, SandboxCommand, SandboxError};
use async_trait::async_trait;
use std::time::Duration;

/// Fuel units granted per millisecond of CPU budget. Wasmtime fuel meters executed
/// instructions, not wall-clock time, so this is an approximate compute budget
/// rather than a precise CPU-time limit (which the [`super::NativeSandbox`] enforces).
const WASI_FUEL_PER_MILLI: u64 = 1_000_000;

/// A WASI/WASM sandbox: runs a `wasm32-wasi` module in an in-process Wasmtime VM
/// with capability-based isolation ([sandbox runtime §2](../../../docs/07-tool-runtime/sandbox-runtime.md)).
///
/// Unlike the process backends, [`SandboxCommand::program`] is the path to a
/// `.wasm` module and [`SandboxCommand::args`] are its WASI argv. Isolation is
/// capability-based: the guest gets no network and no filesystem beyond the
/// bind-mounted `workdir` (preopened at `.`), so a deny-all [`super::NetworkPolicy`]
/// is the default and needs no enforcement. Limits map to Wasmtime primitives:
/// memory → `StoreLimits`, CPU → fuel, wall-clock → epoch interruption.
///
/// Enabled by the `wasi` cargo feature.
#[derive(Clone)]
pub struct WasiSandbox {
    engine: wasmtime::Engine,
}

struct WasiState {
    wasi: wasmtime_wasi::WasiCtx,
    limits: wasmtime::StoreLimits,
}

impl WasiSandbox {
    /// Construct a sandbox with a fuel- and epoch-metered engine.
    pub fn new() -> Result<Self, SandboxError> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = wasmtime::Engine::new(&config)
            .map_err(|e| SandboxError::Internal(format!("wasmtime engine: {e}")))?;
        Ok(Self { engine })
    }

    /// Run a module to completion on the current (blocking) thread, feeding `stdin`
    /// to the guest's standard input.
    fn run_module(
        &self,
        cmd: &SandboxCommand,
        stdin: &[u8],
    ) -> Result<CommandOutcome, SandboxError> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use wasi_common::pipe::{ReadPipe, WritePipe};
        use wasmtime::{Linker, Module, Store, StoreLimitsBuilder, Trap};
        use wasmtime_wasi::{Dir, WasiCtxBuilder, ambient_authority};

        let module = Module::from_file(&self.engine, &cmd.program)
            .map_err(|e| SandboxError::Spawn(format!("load wasm `{}`: {e}", cmd.program)))?;

        // Capture stdout/stderr into in-memory pipes (cloned handles share the buffer).
        let stdout = WritePipe::new_in_memory();
        let stderr = WritePipe::new_in_memory();

        let mut builder = WasiCtxBuilder::new();
        let argv: Vec<String> = std::iter::once(cmd.program.clone())
            .chain(cmd.args.iter().cloned())
            .collect();
        builder
            .args(&argv)
            .map_err(|e| SandboxError::Internal(format!("wasi args: {e}")))?;
        builder.stdout(Box::new(stdout.clone()));
        builder.stderr(Box::new(stderr.clone()));
        // Feed the request bytes to the guest's stdin (empty → no input available).
        builder.stdin(Box::new(ReadPipe::from(stdin.to_vec())));
        if !cmd.workdir.is_empty() {
            // Preopen the workdir as the guest's sole filesystem capability. A
            // missing dir is non-fatal — the module simply gets no preopens.
            if let Ok(dir) = Dir::open_ambient_dir(&cmd.workdir, ambient_authority()) {
                builder
                    .preopened_dir(dir, ".")
                    .map_err(|e| SandboxError::Internal(format!("wasi preopen: {e}")))?;
            }
        }
        // Inject environment variables (e.g. resolved secrets) into the guest. They
        // live only for this in-memory execution and are dropped with the command.
        for (key, value) in &cmd.env {
            builder
                .env(key, value)
                .map_err(|e| SandboxError::Internal(format!("wasi env: {e}")))?;
        }
        let wasi = builder.build();

        let mut limits = StoreLimitsBuilder::new();
        if let Some(bytes) = cmd.limits.memory_bytes {
            limits = limits.memory_size(bytes as usize);
        }
        let mut store = Store::new(
            &self.engine,
            WasiState {
                wasi,
                limits: limits.build(),
            },
        );
        store.limiter(|s| &mut s.limits);

        // CPU budget via fuel; unbounded when no cpu limit is requested.
        let fuel = cmd
            .limits
            .cpu_millis
            .map(|m| (m as u64).saturating_mul(WASI_FUEL_PER_MILLI))
            .unwrap_or(u64::MAX);
        store
            .add_fuel(fuel)
            .map_err(|e| SandboxError::Internal(format!("wasi fuel: {e}")))?;

        // Wall-clock budget via epoch interruption: a watchdog ticks the engine's
        // epoch once the timeout elapses, trapping a still-running guest.
        store.set_epoch_deadline(1);
        let finished = Arc::new(AtomicBool::new(false));
        let watchdog = {
            let engine = self.engine.clone();
            let finished = finished.clone();
            let timeout = cmd.limits.timeout;
            std::thread::spawn(move || {
                let step = Duration::from_millis(20);
                let mut waited = Duration::ZERO;
                while waited < timeout {
                    if finished.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(step);
                    waited += step;
                }
                engine.increment_epoch();
            })
        };

        let mut linker = Linker::new(&self.engine);
        wasmtime_wasi::add_to_linker(&mut linker, |s: &mut WasiState| &mut s.wasi)
            .map_err(|e| SandboxError::Internal(format!("wasi linker: {e}")))?;
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| SandboxError::Internal(format!("instantiate: {e}")))?;
        let start = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|e| SandboxError::Spawn(format!("module has no WASI `_start`: {e}")))?;

        let result = start.call(&mut store, ());
        finished.store(true, Ordering::Relaxed);
        let _ = watchdog.join();

        let mut timed_out = false;
        let mut resource_exceeded = false;
        let exit_code = match result {
            Ok(()) => Some(0),
            Err(err) => {
                if let Some(exit) = err.downcast_ref::<wasmtime_wasi::I32Exit>() {
                    // A WASI `proc_exit(code)` is a normal, non-zero return.
                    Some(exit.0)
                } else {
                    match err.downcast_ref::<Trap>() {
                        Some(Trap::OutOfFuel) => resource_exceeded = true,
                        Some(Trap::Interrupt) => timed_out = true,
                        _ => {}
                    }
                    None
                }
            }
        };

        // Drop the store so the sandbox's stdout/stderr clones are the sole holders
        // before we reclaim the captured bytes.
        drop(store);
        let out = pipe_bytes(stdout);
        let err = pipe_bytes(stderr);
        let (stdout_s, t1) = cap(&out, cmd.limits.max_output_bytes);
        let (stderr_s, t2) = cap(&err, cmd.limits.max_output_bytes);

        Ok(CommandOutcome {
            exit_code,
            stdout: stdout_s,
            stderr: stderr_s,
            timed_out,
            truncated: t1 || t2,
            signal: None,
            resource_exceeded,
        })
    }

    /// Execute the module at [`SandboxCommand::program`], feeding `stdin` to the
    /// guest and capturing its stdout/stderr. The async variant of [`run_module`]:
    /// Wasmtime execution is synchronous and CPU-bound, so it runs on a blocking
    /// thread to avoid stalling the async runtime.
    pub async fn execute_with_stdin(
        &self,
        cmd: &SandboxCommand,
        stdin: Vec<u8>,
    ) -> Result<CommandOutcome, SandboxError> {
        let this = self.clone();
        let cmd = cmd.clone();
        tokio::task::spawn_blocking(move || this.run_module(&cmd, &stdin))
            .await
            .map_err(|e| SandboxError::Internal(format!("wasi join: {e}")))?
    }
}

#[async_trait]
impl Sandbox for WasiSandbox {
    fn backend(&self) -> SandboxBackend {
        SandboxBackend::Wasi
    }

    async fn execute(&self, cmd: &SandboxCommand) -> Result<CommandOutcome, SandboxError> {
        self.execute_with_stdin(cmd, Vec::new()).await
    }
}

/// Reclaim the bytes written to an in-memory WASI pipe (sole-owner after the store
/// is dropped).
fn pipe_bytes(pipe: wasi_common::pipe::WritePipe<std::io::Cursor<Vec<u8>>>) -> Vec<u8> {
    pipe.try_into_inner()
        .map(std::io::Cursor::into_inner)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::types::ResourceLimits;
    use super::*;

    /// A WASI module that writes "wovyr_wasi_ok\n" to stdout via `fd_write`.
    const PRINT_WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 8) "wovyr_wasi_ok\0a")
          (func (export "_start")
            (i32.store (i32.const 0) (i32.const 8))   ;; iov.buf  = 8
            (i32.store (i32.const 4) (i32.const 13))  ;; iov.len  = 13
            (drop (call $fd_write
              (i32.const 1)    ;; fd = stdout
              (i32.const 0)    ;; iovs ptr
              (i32.const 1)    ;; iovs len
              (i32.const 20))))) ;; nwritten ptr
    "#;

    /// A WASI module that loops forever (to exercise fuel/epoch limits).
    const LOOP_WAT: &str = r#"(module (func (export "_start") (loop (br 0))))"#;

    /// A WASI module that dumps its environment block to stdout: reads `environ_sizes_get`
    /// then `environ_get` and writes the raw `KEY=VALUE\0…` buffer to fd 1. Used to prove
    /// that `SandboxCommand.env` reaches the guest.
    const ENVIRON_WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "environ_sizes_get"
            (func $sizes (param i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "environ_get"
            (func $get (param i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "_start")
            ;; environ_sizes_get(count=@0, bufsize=@4)
            (drop (call $sizes (i32.const 0) (i32.const 4)))
            ;; environ_get(environ_ptrs=@8, environ_buf=@100)
            (drop (call $get (i32.const 8) (i32.const 100)))
            ;; iovec @200 -> { buf=100, len=mem[4] (total env buffer size) }
            (i32.store (i32.const 200) (i32.const 100))
            (i32.store (i32.const 204) (i32.load (i32.const 4)))
            ;; fd_write(stdout, iovs=@200, 1, nwritten=@208)
            (drop (call $fd_write (i32.const 1) (i32.const 200) (i32.const 1) (i32.const 208)))))
    "#;

    fn wasm_temp(wat_src: &str, tag: &str) -> std::path::PathBuf {
        let bytes = wat::parse_str(wat_src).expect("assemble wat");
        let mut path = std::env::temp_dir();
        path.push(format!("wovyr_wasi_{tag}_{}.wasm", std::process::id()));
        std::fs::write(&path, bytes).expect("write wasm fixture");
        path
    }

    fn wasi_cmd(path: &std::path::Path, limits: ResourceLimits) -> SandboxCommand {
        SandboxCommand {
            program: path.to_string_lossy().into_owned(),
            args: vec![],
            workdir: ".".into(),
            env: vec![],
            limits,
        }
    }

    #[tokio::test]
    async fn wasi_runs_module_and_captures_stdout() {
        let path = wasm_temp(PRINT_WAT, "print");
        let sb = WasiSandbox::new().unwrap();
        let out = sb
            .execute(&wasi_cmd(&path, ResourceLimits::default()))
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
        assert!(
            out.stdout.contains("wovyr_wasi_ok"),
            "stdout: {:?}",
            out.stdout
        );
    }

    #[tokio::test]
    async fn wasi_injects_env_into_guest() {
        let path = wasm_temp(ENVIRON_WAT, "environ");
        let mut cmd = wasi_cmd(&path, ResourceLimits::default());
        cmd.env = vec![("WOVYR_SECRET_DB_TOKEN".into(), "hunter2".into())];
        let sb = WasiSandbox::new().unwrap();
        let out = sb.execute(&cmd).await.unwrap();
        assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
        assert!(
            out.stdout.contains("WOVYR_SECRET_DB_TOKEN=hunter2"),
            "injected env var should reach the guest; stdout: {:?}",
            out.stdout
        );
    }

    #[tokio::test]
    async fn wasi_fuel_budget_exhaustion_is_resource_exceeded() {
        // A 1 ms CPU budget is far less than an infinite loop needs.
        let path = wasm_temp(LOOP_WAT, "fuel");
        let limits = ResourceLimits {
            cpu_millis: Some(1),
            timeout: Duration::from_secs(30),
            ..ResourceLimits::default()
        };
        let sb = WasiSandbox::new().unwrap();
        let out = sb.execute(&wasi_cmd(&path, limits)).await.unwrap();
        assert!(out.resource_exceeded, "expected fuel exhaustion");
        assert!(!out.timed_out);
        assert_eq!(out.exit_code, None);
    }

    #[tokio::test]
    async fn wasi_wall_clock_timeout_interrupts() {
        // Unbounded fuel (no cpu limit) → the epoch watchdog must stop the loop.
        let path = wasm_temp(LOOP_WAT, "timeout");
        let limits = ResourceLimits {
            cpu_millis: None,
            timeout: Duration::from_millis(150),
            ..ResourceLimits::default()
        };
        let sb = WasiSandbox::new().unwrap();
        let out = sb.execute(&wasi_cmd(&path, limits)).await.unwrap();
        assert!(out.timed_out, "expected epoch interruption");
        assert!(!out.resource_exceeded);
    }

    #[tokio::test]
    async fn wasi_missing_module_is_spawn_error() {
        let sb = WasiSandbox::new().unwrap();
        let cmd = wasi_cmd(
            std::path::Path::new("/nonexistent/wovyr.wasm"),
            ResourceLimits::default(),
        );
        let err = sb.execute(&cmd).await.unwrap_err();
        assert!(matches!(err, SandboxError::Spawn(_)));
    }

    #[tokio::test]
    async fn wasi_backend_is_detected() {
        let mgr = super::super::SandboxManager::detect().await;
        assert!(mgr.capabilities().contains(&SandboxBackend::Wasi));
    }
}
