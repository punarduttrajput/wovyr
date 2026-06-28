//! Sandbox abstraction and the native-process backend.
//!
//! Models the isolation backends, trust classes, and backend-selection rules from
//! the [Sandbox Runtime](../../docs/07-tool-runtime/sandbox-runtime.md) and
//! [Security & Isolation](../../docs/07-tool-runtime/security-isolation.md) specs:
//! the runtime picks the **strongest** of (tool preference, tenant policy floor,
//! trust-class minimum), then checks node capability.
//!
//! v0.2 implements the **native** backend only (process + timeout + output cap).
//! Stronger backends (WASI, Container, gVisor, microVM, …) are represented in the
//! [`SandboxBackend`] spectrum and selected for correctly, but constructing them
//! returns [`SandboxError::Unsupported`] until their runtimes are integrated —
//! containers/microVMs cannot run in every environment. Per-execution CPU/memory/
//! network/filesystem confinement is likewise deferred to those backends.

use async_trait::async_trait;
use std::fmt;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Isolation backends, ordered weakest→strongest by [`Self::isolation_level`]
/// ([sandbox runtime §2](../../docs/07-tool-runtime/sandbox-runtime.md)).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SandboxBackend {
    /// OS process + namespaces. Trusted first-party tools.
    Native,
    /// Capability-based in-process WASM VM.
    Wasi,
    /// Network-isolated remote worker pool.
    RemoteWorker,
    /// OCI container (namespaces + cgroups).
    Container,
    /// User-space kernel (gVisor) for untrusted syscalls.
    Gvisor,
    /// Hardware-virtualized microVM (Firecracker).
    Firecracker,
    /// Kubernetes pod with policies.
    KubernetesPod,
}

impl SandboxBackend {
    /// Relative isolation strength (higher = stronger). Used to pick the strongest
    /// of competing requirements.
    pub fn isolation_level(self) -> u8 {
        match self {
            SandboxBackend::Native => 1,
            SandboxBackend::Wasi => 2,
            SandboxBackend::RemoteWorker => 3,
            SandboxBackend::Container => 4,
            SandboxBackend::Gvisor => 5,
            SandboxBackend::Firecracker => 6,
            SandboxBackend::KubernetesPod => 7,
        }
    }
}

impl fmt::Display for SandboxBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            SandboxBackend::Native => "native",
            SandboxBackend::Wasi => "wasi",
            SandboxBackend::RemoteWorker => "remote-worker",
            SandboxBackend::Container => "container",
            SandboxBackend::Gvisor => "gvisor",
            SandboxBackend::Firecracker => "firecracker",
            SandboxBackend::KubernetesPod => "kubernetes-pod",
        };
        f.write_str(s)
    }
}

/// A tool's trust classification, which sets the minimum isolation backend
/// ([security §3](../../docs/07-tool-runtime/security-isolation.md)).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrustClass {
    /// Built by the platform team.
    FirstParty,
    /// Third-party, reviewed and signed.
    Verified,
    /// Unreviewed / user-supplied.
    Untrusted,
}

impl TrustClass {
    /// The weakest backend permitted for this trust class.
    pub fn minimum_backend(self) -> SandboxBackend {
        match self {
            TrustClass::FirstParty => SandboxBackend::Native,
            TrustClass::Verified => SandboxBackend::Container,
            TrustClass::Untrusted => SandboxBackend::Gvisor,
        }
    }
}

/// Per-execution resource limits
/// ([sandbox runtime §5](../../docs/07-tool-runtime/sandbox-runtime.md)).
///
/// The native backend enforces `timeout` and `max_output_bytes`; CPU/memory/PID
/// caps require cgroups and are carried but enforced only by stronger backends.
#[derive(Clone, Debug)]
pub struct ResourceLimits {
    /// Wall-clock execution timeout.
    pub timeout: Duration,
    /// Max captured stdout/stderr bytes (output beyond this is truncated).
    pub max_output_bytes: usize,
    /// CPU quota in millicores (enforced by container/VM backends).
    pub cpu_millis: Option<u32>,
    /// Memory cap in bytes (enforced by container/VM backends).
    pub memory_bytes: Option<u64>,
    /// Max process count (enforced by cgroup `pids.max`).
    pub max_pids: Option<u32>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_output_bytes: 1024 * 1024,
            cpu_millis: None,
            memory_bytes: None,
            max_pids: None,
        }
    }
}

/// Declarative egress policy ([security §5](../../docs/07-tool-runtime/security-isolation.md)).
/// Default is deny-all; enforcement requires a network-isolating backend/proxy.
#[derive(Clone, Debug)]
pub struct NetworkPolicy {
    /// Deny all egress unless explicitly allowed.
    pub default_deny: bool,
    /// Allowed outbound hosts.
    pub outbound_allow: Vec<String>,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            default_deny: true,
            outbound_allow: Vec::new(),
        }
    }
}

impl NetworkPolicy {
    /// Whether egress to `host` is permitted under this policy.
    pub fn allows_host(&self, host: &str) -> bool {
        if !self.default_deny {
            return true;
        }
        self.outbound_allow.iter().any(|h| h == host)
    }
}

/// Errors from sandbox provisioning or execution.
#[derive(Clone, Debug)]
pub enum SandboxError {
    /// The selected backend is not available on this node.
    Unsupported(SandboxBackend),
    /// Failed to spawn the process.
    Spawn(String),
    /// An internal execution error.
    Internal(String),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SandboxError::Unsupported(b) => {
                write!(f, "sandbox backend `{b}` is not available on this node")
            }
            SandboxError::Spawn(m) => write!(f, "sandbox spawn failed: {m}"),
            SandboxError::Internal(m) => write!(f, "sandbox error: {m}"),
        }
    }
}

impl std::error::Error for SandboxError {}

/// Select the backend to run a tool in: the strongest of the tool's `preference`,
/// the trust-class minimum, and any tenant `policy_floor`; then verify the node
/// supports it ([sandbox runtime §3](../../docs/07-tool-runtime/sandbox-runtime.md)).
pub fn select_backend(
    preference: SandboxBackend,
    policy_floor: Option<SandboxBackend>,
    trust: TrustClass,
    capabilities: &[SandboxBackend],
) -> Result<SandboxBackend, SandboxError> {
    let mut chosen = preference;
    let raise_to = |chosen: &mut SandboxBackend, candidate: SandboxBackend| {
        if candidate.isolation_level() > chosen.isolation_level() {
            *chosen = candidate;
        }
    };
    raise_to(&mut chosen, trust.minimum_backend());
    if let Some(floor) = policy_floor {
        raise_to(&mut chosen, floor);
    }
    if !capabilities.contains(&chosen) {
        return Err(SandboxError::Unsupported(chosen));
    }
    Ok(chosen)
}

/// Selects backends against a node's capabilities and an optional policy floor.
#[derive(Clone, Debug)]
pub struct SandboxManager {
    capabilities: Vec<SandboxBackend>,
    policy_floor: Option<SandboxBackend>,
}

impl SandboxManager {
    /// A manager for a node that supports only the native backend (this build).
    pub fn native_only() -> Self {
        Self {
            capabilities: vec![SandboxBackend::Native],
            policy_floor: None,
        }
    }

    /// Construct with explicit node capabilities and an optional tenant floor.
    pub fn new(capabilities: Vec<SandboxBackend>, policy_floor: Option<SandboxBackend>) -> Self {
        Self {
            capabilities,
            policy_floor,
        }
    }

    /// Resolve the backend for a tool with the given preference and trust class.
    pub fn select(
        &self,
        preference: SandboxBackend,
        trust: TrustClass,
    ) -> Result<SandboxBackend, SandboxError> {
        select_backend(preference, self.policy_floor, trust, &self.capabilities)
    }
}

/// A command to execute inside a sandbox.
#[derive(Clone, Debug)]
pub struct SandboxCommand {
    /// Program to run.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Working directory.
    pub workdir: String,
    /// Resource limits.
    pub limits: ResourceLimits,
}

/// The outcome of running a command in a sandbox.
#[derive(Clone, Debug)]
pub struct CommandOutcome {
    /// Process exit code, if the process exited normally.
    pub exit_code: Option<i32>,
    /// Captured standard output (possibly truncated).
    pub stdout: String,
    /// Captured standard error (possibly truncated).
    pub stderr: String,
    /// Whether the process was killed for exceeding the timeout.
    pub timed_out: bool,
    /// Whether output was truncated at `max_output_bytes`.
    pub truncated: bool,
}

/// An isolated execution environment.
#[async_trait]
pub trait Sandbox: Send + Sync {
    /// The backend this sandbox implements.
    fn backend(&self) -> SandboxBackend;

    /// Execute a command, capturing its output.
    async fn execute(&self, cmd: &SandboxCommand) -> Result<CommandOutcome, SandboxError>;
}

/// A native-process sandbox: timeout-enforced, output-capped child process.
#[derive(Clone, Debug)]
pub struct NativeSandbox {
    limits: ResourceLimits,
}

impl NativeSandbox {
    /// Construct with just a timeout (other limits default).
    pub fn new(timeout: Duration) -> Self {
        Self {
            limits: ResourceLimits {
                timeout,
                ..ResourceLimits::default()
            },
        }
    }

    /// Construct with full resource limits.
    pub fn with_limits(limits: ResourceLimits) -> Self {
        Self { limits }
    }

    /// Run `program` with `args` in `workdir`, capturing (capped) output.
    ///
    /// On timeout the child is killed and a [`CommandOutcome`] with
    /// `timed_out = true` is returned rather than an error.
    pub async fn run(
        &self,
        program: &str,
        args: &[&str],
        workdir: &str,
    ) -> Result<CommandOutcome, SandboxError> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = command
            .spawn()
            .map_err(|e| SandboxError::Spawn(format!("`{program}`: {e}")))?;

        match tokio::time::timeout(self.limits.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                let (stdout, t1) = cap(&output.stdout, self.limits.max_output_bytes);
                let (stderr, t2) = cap(&output.stderr, self.limits.max_output_bytes);
                Ok(CommandOutcome {
                    exit_code: output.status.code(),
                    stdout,
                    stderr,
                    timed_out: false,
                    truncated: t1 || t2,
                })
            }
            Ok(Err(e)) => Err(SandboxError::Internal(format!("process error: {e}"))),
            Err(_elapsed) => Ok(CommandOutcome {
                exit_code: None,
                stdout: String::new(),
                stderr: format!("execution exceeded timeout of {:?}", self.limits.timeout),
                timed_out: true,
                truncated: false,
            }),
        }
    }
}

#[async_trait]
impl Sandbox for NativeSandbox {
    fn backend(&self) -> SandboxBackend {
        SandboxBackend::Native
    }

    async fn execute(&self, cmd: &SandboxCommand) -> Result<CommandOutcome, SandboxError> {
        let args: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
        NativeSandbox::with_limits(cmd.limits.clone())
            .run(&cmd.program, &args, &cmd.workdir)
            .await
    }
}

/// Truncate `bytes` to `max` (UTF-8 lossy); returns the string and whether it was cut.
fn cap(bytes: &[u8], max: usize) -> (String, bool) {
    if bytes.len() > max {
        (String::from_utf8_lossy(&bytes[..max]).into_owned(), true)
    } else {
        (String::from_utf8_lossy(bytes).into_owned(), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn echo(text: &str) -> CommandOutcome {
        let sb = NativeSandbox::new(Duration::from_secs(10));
        if cfg!(windows) {
            sb.run("cmd", &["/C", &format!("echo {text}")], ".")
                .await
                .unwrap()
        } else {
            sb.run("sh", &["-c", &format!("echo {text}")], ".")
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn captures_stdout_and_exit_code() {
        let out = echo("apex_sandbox_ok").await;
        assert!(out.stdout.contains("apex_sandbox_ok"));
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn enforces_timeout() {
        let sb = NativeSandbox::new(Duration::from_millis(150));
        let out = sb.run("sh", &["-c", "sleep 5"], ".").await.unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
    }

    #[test]
    fn first_party_selects_native() {
        let mgr = SandboxManager::native_only();
        assert_eq!(
            mgr.select(SandboxBackend::Native, TrustClass::FirstParty)
                .unwrap(),
            SandboxBackend::Native
        );
    }

    #[test]
    fn untrusted_is_floored_to_gvisor() {
        // Even though the tool prefers native, untrusted code must be floored up.
        let caps = vec![SandboxBackend::Native, SandboxBackend::Gvisor];
        let chosen =
            select_backend(SandboxBackend::Native, None, TrustClass::Untrusted, &caps).unwrap();
        assert_eq!(chosen, SandboxBackend::Gvisor);
    }

    #[test]
    fn untrusted_unavailable_backend_is_unsupported() {
        // A native-only node cannot run untrusted code (needs gVisor/microVM).
        let err = SandboxManager::native_only()
            .select(SandboxBackend::Native, TrustClass::Untrusted)
            .unwrap_err();
        assert!(matches!(
            err,
            SandboxError::Unsupported(SandboxBackend::Gvisor)
        ));
    }

    #[test]
    fn policy_floor_raises_weaker_preference() {
        let caps = vec![
            SandboxBackend::Native,
            SandboxBackend::Container,
            SandboxBackend::Gvisor,
        ];
        let chosen = select_backend(
            SandboxBackend::Native,
            Some(SandboxBackend::Container),
            TrustClass::FirstParty,
            &caps,
        )
        .unwrap();
        assert_eq!(chosen, SandboxBackend::Container);
    }

    #[test]
    fn network_policy_default_denies() {
        let mut p = NetworkPolicy::default();
        assert!(!p.allows_host("api.example.com"));
        p.outbound_allow.push("api.example.com".to_string());
        assert!(p.allows_host("api.example.com"));
        assert!(!p.allows_host("evil.example.com"));
    }
}
