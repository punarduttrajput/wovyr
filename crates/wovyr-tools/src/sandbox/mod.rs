//! Sandbox abstraction and the isolation backends.
//!
//! Models the isolation backends, trust classes, and backend-selection rules from
//! the [Sandbox Runtime](../../../docs/07-tool-runtime/sandbox-runtime.md) and
//! [Security & Isolation](../../../docs/07-tool-runtime/security-isolation.md) specs:
//! the runtime picks the **strongest** of (tool preference, tenant policy floor,
//! trust-class minimum), then checks node capability.
//!
//! Implemented backends, one file per module (RM-GA-P4 HLTH-904 — this used to be
//! a single 1,891-line file):
//! - [`native::NativeSandbox`] — OS process with **real resource enforcement** on
//!   Unix via `setrlimit` (memory `RLIMIT_AS`, CPU time `RLIMIT_CPU`) plus a
//!   wall-clock timeout and an output cap.
//! - [`container::ContainerSandbox`] — OCI container via the `docker`/`podman` CLI,
//!   enforcing memory/CPU/PID limits through cgroups, a read-only rootfs, a
//!   bind-mounted workspace, and a [`types::NetworkPolicy`] (`--network none` on
//!   deny). The same type drives the **gVisor** backend via `--runtime=runsc`
//!   ([`container::ContainerSandbox::gvisor`]).
//! - [`firecracker::FirecrackerSandbox`] — a microVM backend that runs the command
//!   inside a Firecracker guest via a one-shot block-device protocol (input/output
//!   drives + an `/init` guest agent; see `deployment/firecracker/`).
//!   Capability-gated on `firecracker` + `/dev/kvm`; needs a guest kernel + rootfs.
//! - [`wasi::WasiSandbox`] (behind the `wasi` cargo feature) — an in-process
//!   Wasmtime VM running `wasm32-wasi` modules with capability-based isolation.
//!
//! Backend *selection* ([`types::select_backend`]) is pure and deterministic. Node
//! *capability detection* ([`types::SandboxManager::detect`]) probes the environment
//! (docker daemon, `runsc` runtime, `firecracker` + `/dev/kvm`) and is the only
//! part of this module tree that does ambient I/O.

mod container;
mod firecracker;
mod native;
mod types;
#[cfg(feature = "wasi")]
mod wasi;

pub use container::ContainerSandbox;
pub use firecracker::{FirecrackerConfig, FirecrackerSandbox};
pub use native::NativeSandbox;
pub use types::{
    CommandOutcome, NetworkPolicy, ResourceLimits, SandboxBackend, SandboxCommand, SandboxError,
    SandboxManager, TrustClass, select_backend,
};
#[cfg(feature = "wasi")]
pub use wasi::WasiSandbox;

/// An isolated execution environment.
#[async_trait::async_trait]
pub trait Sandbox: Send + Sync {
    /// The backend this sandbox implements.
    fn backend(&self) -> SandboxBackend;

    /// Execute a command, capturing its output.
    async fn execute(&self, cmd: &SandboxCommand) -> Result<CommandOutcome, SandboxError>;
}

/// Truncate `bytes` to `max` (UTF-8 lossy); returns the string and whether it was cut.
/// Shared by the native, Firecracker, and WASI backends (each captures process/guest
/// output and needs to cap it identically).
fn cap(bytes: &[u8], max: usize) -> (String, bool) {
    if bytes.len() > max {
        (String::from_utf8_lossy(&bytes[..max]).into_owned(), true)
    } else {
        (String::from_utf8_lossy(bytes).into_owned(), false)
    }
}
