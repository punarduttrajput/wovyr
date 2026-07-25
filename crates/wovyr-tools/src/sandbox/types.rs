//! Shared sandbox types: backend/trust enums, resource and network policy,
//! command/outcome types, the error type, and the capability-aware
//! [`SandboxManager`].

use std::fmt;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Isolation backends, ordered weakest→strongest by [`Self::isolation_level`]
/// ([sandbox runtime §2](../../../docs/07-tool-runtime/sandbox-runtime.md)).
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
/// ([security §3](../../../docs/07-tool-runtime/security-isolation.md)).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TrustClass {
    /// Built by the platform team.
    #[default]
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
/// ([sandbox runtime §5](../../../docs/07-tool-runtime/sandbox-runtime.md)).
///
/// The native backend enforces `timeout`, `max_output_bytes`, and (on Unix) the
/// memory and CPU caps via `setrlimit`. The container/gVisor backends additionally
/// enforce CPU/memory/PID limits through cgroups.
#[derive(Clone, Debug)]
pub struct ResourceLimits {
    /// Wall-clock execution timeout.
    pub timeout: Duration,
    /// Max captured stdout/stderr bytes (output beyond this is truncated).
    pub max_output_bytes: usize,
    /// CPU quota in millicores. Native maps this to `RLIMIT_CPU` seconds;
    /// containers map it to `--cpus`.
    pub cpu_millis: Option<u32>,
    /// Memory cap in bytes. Native maps this to `RLIMIT_AS`; containers map it to
    /// `--memory` (cgroup `memory.max`).
    pub memory_bytes: Option<u64>,
    /// Max process count (cgroup `pids.max`; enforced by container/VM backends).
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

/// Declarative egress policy ([security §5](../../../docs/07-tool-runtime/security-isolation.md)).
/// Default is deny-all; the container/gVisor backends enforce a full deny with
/// `--network none`. A non-empty allow-list is enforced through an
/// [`EgressProxy`](crate::EgressProxy): the workload reaches only allow-listed hosts
/// over HTTPS (`CONNECT`), via `HTTPS_PROXY`.
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

    /// Whether the policy denies *all* egress (no host may be reached). This is the
    /// condition under which a backend uses a fully isolated network namespace.
    pub fn denies_all(&self) -> bool {
        self.default_deny && self.outbound_allow.is_empty()
    }

    /// Whether this policy requires an [`EgressProxy`](crate::EgressProxy): default-deny
    /// with a non-empty allow-list (specific hosts permitted, all others blocked).
    pub fn needs_proxy(&self) -> bool {
        self.default_deny && !self.outbound_allow.is_empty()
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
/// supports it ([sandbox runtime §3](../../../docs/07-tool-runtime/sandbox-runtime.md)).
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
    /// A manager for a node that supports only the native backend.
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

    /// Probe the host for available backends ([sandbox runtime §3, step 5](../../../docs/07-tool-runtime/sandbox-runtime.md)).
    ///
    /// Native is always present. Container is added when a `docker` daemon is
    /// reachable; gVisor when `runsc` is additionally registered as a docker
    /// runtime; Firecracker when the `firecracker` binary and `/dev/kvm` are
    /// present. This is the only ambient-I/O entry point in the module.
    pub async fn detect() -> Self {
        let mut capabilities = vec![SandboxBackend::Native];

        // The WASI backend is in-process: available whenever compiled in.
        #[cfg(feature = "wasi")]
        capabilities.push(SandboxBackend::Wasi);

        let docker = command_succeeds("docker", &["info"]).await;
        if docker {
            capabilities.push(SandboxBackend::Container);
            if binary_exists("runsc").await && docker_info_mentions("runsc").await {
                capabilities.push(SandboxBackend::Gvisor);
            }
        }
        if binary_exists("firecracker").await && std::path::Path::new("/dev/kvm").exists() {
            capabilities.push(SandboxBackend::Firecracker);
        }

        Self {
            capabilities,
            policy_floor: None,
        }
    }

    /// The backends this node can run.
    pub fn capabilities(&self) -> &[SandboxBackend] {
        &self.capabilities
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
#[derive(Clone, Debug, Default)]
pub struct SandboxCommand {
    /// Program to run.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Working directory (bind-mounted to `/workspace` for container backends).
    pub workdir: String,
    /// Environment variables injected into the sandbox (e.g. resolved secrets, in
    /// memory). Honored by the WASI and container backends (the latter passes names
    /// on the argv and values via the CLI process env, keeping secret values out of
    /// host process listings); zeroed with the command on teardown.
    pub env: Vec<(String, String)>,
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
    /// Terminating Unix signal number, if the process was killed by a signal.
    pub signal: Option<i32>,
    /// Whether the process was terminated for breaching a resource limit
    /// (`resource_exceeded` in [Execution API §10](../../../docs/07-tool-runtime/execution-api.md)).
    pub resource_exceeded: bool,
}

/// Whether `program args...` exits successfully (used for capability detection).
async fn command_succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether `name` resolves on `PATH`.
async fn binary_exists(name: &str) -> bool {
    command_succeeds("sh", &["-c", &format!("command -v {name}")]).await
}

/// Whether `docker info` reports a runtime/feature mentioning `needle` (e.g. `runsc`).
async fn docker_info_mentions(needle: &str) -> bool {
    let out = Command::new("docker")
        .args(["info"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await;
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains(needle),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(p.denies_all());
        p.outbound_allow.push("api.example.com".to_string());
        assert!(p.allows_host("api.example.com"));
        assert!(!p.allows_host("evil.example.com"));
        assert!(!p.denies_all());
        // A non-empty allow-list needs the egress proxy; deny-all and allow-all do not.
        assert!(p.needs_proxy());
        assert!(!NetworkPolicy::default().needs_proxy());
        let open = NetworkPolicy {
            default_deny: false,
            outbound_allow: vec![],
        };
        assert!(!open.needs_proxy());
    }
}
