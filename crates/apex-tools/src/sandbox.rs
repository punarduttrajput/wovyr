//! Sandbox abstraction and the isolation backends.
//!
//! Models the isolation backends, trust classes, and backend-selection rules from
//! the [Sandbox Runtime](../../docs/07-tool-runtime/sandbox-runtime.md) and
//! [Security & Isolation](../../docs/07-tool-runtime/security-isolation.md) specs:
//! the runtime picks the **strongest** of (tool preference, tenant policy floor,
//! trust-class minimum), then checks node capability.
//!
//! Implemented backends:
//! - [`NativeSandbox`] — OS process with **real resource enforcement** on Unix via
//!   `setrlimit` (memory `RLIMIT_AS`, CPU time `RLIMIT_CPU`) plus a wall-clock
//!   timeout and an output cap.
//! - [`ContainerSandbox`] — OCI container via the `docker`/`podman` CLI, enforcing
//!   memory/CPU/PID limits through cgroups, a read-only rootfs, a bind-mounted
//!   workspace, and a [`NetworkPolicy`] (`--network none` on deny). The same type
//!   drives the **gVisor** backend via `--runtime=runsc`
//!   ([`ContainerSandbox::gvisor`]).
//! - [`FirecrackerSandbox`] — a microVM backend that runs the command inside a
//!   Firecracker guest via a one-shot block-device protocol (input/output drives + an
//!   `/init` guest agent; see `deployment/firecracker/`). Capability-gated on
//!   `firecracker` + `/dev/kvm`; needs a guest kernel + rootfs.
//!
//! Backend *selection* ([`select_backend`]) is pure and deterministic. Node
//! *capability detection* ([`SandboxManager::detect`]) probes the environment
//! (docker daemon, `runsc` runtime, `firecracker` + `/dev/kvm`) and is the only
//! part of this module that does ambient I/O.

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

/// Declarative egress policy ([security §5](../../docs/07-tool-runtime/security-isolation.md)).
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

    /// Probe the host for available backends ([sandbox runtime §3, step 5](../../docs/07-tool-runtime/sandbox-runtime.md)).
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
    /// memory). Honored by the WASI backend; zeroed with the command on teardown.
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
    /// (`resource_exceeded` in [Execution API §10](../../docs/07-tool-runtime/execution-api.md)).
    pub resource_exceeded: bool,
}

/// An isolated execution environment.
#[async_trait]
pub trait Sandbox: Send + Sync {
    /// The backend this sandbox implements.
    fn backend(&self) -> SandboxBackend;

    /// Execute a command, capturing its output.
    async fn execute(&self, cmd: &SandboxCommand) -> Result<CommandOutcome, SandboxError>;
}

/// A native-process sandbox: timeout-enforced, output-capped child process with
/// per-execution `setrlimit` memory/CPU caps on Unix.
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

    /// Build the child process, applying `setrlimit` caps in a `pre_exec` hook on
    /// Unix (memory `RLIMIT_AS`, CPU `RLIMIT_CPU`).
    fn build_command(&self, program: &str, args: &[&str], workdir: &str) -> Command {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let mut std_cmd = std::process::Command::new(program);
            std_cmd
                .args(args)
                .current_dir(workdir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let limits = self.limits.clone();
            // SAFETY: `apply_rlimits` only issues `setrlimit` syscalls and touches
            // stack memory, so it is async-signal-safe to run in the forked child
            // before exec.
            unsafe {
                std_cmd.pre_exec(move || apply_rlimits(&limits));
            }
            let mut cmd = Command::from(std_cmd);
            cmd.kill_on_drop(true);
            cmd
        }
        #[cfg(not(unix))]
        {
            let mut command = Command::new(program);
            command
                .args(args)
                .current_dir(workdir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            command
        }
    }

    /// Run `program` with `args` in `workdir`, capturing (capped) output.
    ///
    /// On timeout the child is killed and a [`CommandOutcome`] with
    /// `timed_out = true` is returned rather than an error. A process killed by a
    /// CPU/file-size rlimit returns `resource_exceeded = true`.
    pub async fn run(
        &self,
        program: &str,
        args: &[&str],
        workdir: &str,
    ) -> Result<CommandOutcome, SandboxError> {
        let mut command = self.build_command(program, args, workdir);

        let child = command
            .spawn()
            .map_err(|e| SandboxError::Spawn(format!("`{program}`: {e}")))?;

        match tokio::time::timeout(self.limits.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                let (stdout, t1) = cap(&output.stdout, self.limits.max_output_bytes);
                let (stderr, t2) = cap(&output.stderr, self.limits.max_output_bytes);
                let (signal, resource_exceeded) = terminating_signal(&output.status);
                Ok(CommandOutcome {
                    exit_code: output.status.code(),
                    stdout,
                    stderr,
                    timed_out: false,
                    truncated: t1 || t2,
                    signal,
                    resource_exceeded,
                })
            }
            Ok(Err(e)) => Err(SandboxError::Internal(format!("process error: {e}"))),
            Err(_elapsed) => Ok(CommandOutcome {
                exit_code: None,
                stdout: String::new(),
                stderr: format!("execution exceeded timeout of {:?}", self.limits.timeout),
                timed_out: true,
                truncated: false,
                signal: None,
                resource_exceeded: false,
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

/// A container sandbox driven by the `docker`/`podman` CLI. The same type backs the
/// gVisor backend by setting the container runtime to `runsc`
/// ([`ContainerSandbox::gvisor`]).
///
/// Resource limits become cgroup flags (`--memory`, `--cpus`, `--pids-limit`); the
/// rootfs is read-only with a `tmpfs` `/tmp`; the working directory is bind-mounted
/// at `/workspace`; and a deny-all [`NetworkPolicy`] becomes `--network none`.
#[derive(Clone, Debug)]
pub struct ContainerSandbox {
    backend: SandboxBackend,
    runtime: String,
    runtime_class: Option<String>,
    image: String,
    network: NetworkPolicy,
}

impl ContainerSandbox {
    /// A Docker container backend running `image`.
    pub fn docker(image: impl Into<String>) -> Self {
        Self {
            backend: SandboxBackend::Container,
            runtime: "docker".to_string(),
            runtime_class: None,
            image: image.into(),
            network: NetworkPolicy::default(),
        }
    }

    /// A Podman container backend running `image`.
    pub fn podman(image: impl Into<String>) -> Self {
        Self {
            backend: SandboxBackend::Container,
            runtime: "podman".to_string(),
            runtime_class: None,
            image: image.into(),
            network: NetworkPolicy::default(),
        }
    }

    /// A gVisor backend: a Docker container with the `runsc` runtime.
    pub fn gvisor(image: impl Into<String>) -> Self {
        Self {
            backend: SandboxBackend::Gvisor,
            runtime: "docker".to_string(),
            runtime_class: Some("runsc".to_string()),
            image: image.into(),
            network: NetworkPolicy::default(),
        }
    }

    /// Override the egress policy (default: deny-all → `--network none`).
    pub fn with_network(mut self, network: NetworkPolicy) -> Self {
        self.network = network;
        self
    }

    /// The full argv used to launch the container, including the wrapped command.
    /// Pure and deterministic — the basis for the backend's unit tests. When the
    /// policy needs an egress proxy, `proxy_port` injects the `HTTPS_PROXY` wiring.
    pub fn argv_with_proxy(&self, cmd: &SandboxCommand, proxy_port: Option<u16>) -> Vec<String> {
        container_argv(
            &self.runtime,
            self.runtime_class.as_deref(),
            &self.image,
            cmd,
            &self.network,
            proxy_port,
        )
    }

    /// The launch argv without an egress proxy (full deny or full bridge).
    pub fn argv(&self, cmd: &SandboxCommand) -> Vec<String> {
        self.argv_with_proxy(cmd, None)
    }
}

#[async_trait]
impl Sandbox for ContainerSandbox {
    fn backend(&self) -> SandboxBackend {
        self.backend
    }

    async fn execute(&self, cmd: &SandboxCommand) -> Result<CommandOutcome, SandboxError> {
        if self.network.needs_proxy() {
            return self.execute_with_egress_lockdown(cmd).await;
        }

        // Deny-all (`--network none`, already airtight) or a fully open bridge
        // (the policy explicitly permits all egress): no proxy, no lockdown, the
        // existing single foreground `run` is sufficient.
        let argv = self.argv(cmd);
        let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        let outer = ResourceLimits {
            timeout: cmd.limits.timeout,
            max_output_bytes: cmd.limits.max_output_bytes,
            ..ResourceLimits::default()
        };
        NativeSandbox::with_limits(outer)
            .run(&argv[0], &args, ".")
            .await
    }
}

impl ContainerSandbox {
    /// Run `cmd` under a host-enforced egress lockdown (closes the "L3 egress
    /// bypass" gap — see `egress_lockdown` module docs): start the container
    /// detached running an inert placeholder (never the untrusted command),
    /// resolve the bridge network's gateway (used as the literal `HTTPS_PROXY`
    /// target, needing no DNS lookup from inside the container), apply the host-side
    /// `iptables` lockdown into the container's network namespace, and only then
    /// run the real command via `docker exec`. The container is removed
    /// unconditionally afterward, on every path.
    async fn execute_with_egress_lockdown(
        &self,
        cmd: &SandboxCommand,
    ) -> Result<CommandOutcome, SandboxError> {
        const NETWORK: &str = "bridge";
        let gateway = crate::egress_lockdown::docker_network_gateway(&self.runtime, NETWORK)
            .await
            .map_err(|e| SandboxError::Internal(format!("resolve gateway: {e}")))?;

        let proxy = crate::egress::EgressProxy::start(self.network.clone())
            .await
            .map_err(|e| SandboxError::Internal(format!("egress proxy: {e}")))?;

        let create_argv = container_lockdown_create_argv(
            &self.runtime,
            self.runtime_class.as_deref(),
            &self.image,
            cmd,
            &gateway,
            proxy.port(),
        );
        let create_args: Vec<&str> = create_argv[1..].iter().map(String::as_str).collect();
        let create_out = Command::new(&create_argv[0])
            .args(&create_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| SandboxError::Spawn(format!("`{}`: {e}", create_argv[0])))?;
        if !create_out.status.success() {
            return Err(SandboxError::Internal(format!(
                "container start failed: {}",
                String::from_utf8_lossy(&create_out.stderr)
            )));
        }
        let container_id = String::from_utf8_lossy(&create_out.stdout)
            .trim()
            .to_string();

        // From here on, every path must remove the container before returning.
        let result = self
            .run_locked_down(cmd, &container_id, &gateway, proxy.port())
            .await;
        let _ = Command::new(&self.runtime)
            .args(["rm", "-f", &container_id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        drop(proxy);
        result
    }

    /// The lockdown-then-exec sequence over an already-started (placeholder)
    /// container. Split out from [`execute_with_egress_lockdown`] purely so that
    /// method can guarantee container cleanup on every return path via one call
    /// site, without duplicating the `docker rm -f` at each early return here.
    async fn run_locked_down(
        &self,
        cmd: &SandboxCommand,
        container_id: &str,
        gateway: &str,
        proxy_port: u16,
    ) -> Result<CommandOutcome, SandboxError> {
        let pid = crate::egress_lockdown::container_pid(&self.runtime, container_id).await?;
        crate::egress_lockdown::lock_down_egress(pid, gateway, proxy_port).await?;

        let mut exec_argv = vec![
            self.runtime.clone(),
            "exec".to_string(),
            container_id.to_string(),
        ];
        exec_argv.push(cmd.program.clone());
        exec_argv.extend(cmd.args.iter().cloned());
        let exec_args: Vec<&str> = exec_argv[1..].iter().map(String::as_str).collect();

        // The container's own cgroup limits (already applied at creation) enforce
        // memory/CPU/pids; this outer run only arms the wall-clock timeout and
        // output cap, exactly like the non-lockdown path.
        let outer = ResourceLimits {
            timeout: cmd.limits.timeout,
            max_output_bytes: cmd.limits.max_output_bytes,
            ..ResourceLimits::default()
        };
        NativeSandbox::with_limits(outer)
            .run(&exec_argv[0], &exec_args, ".")
            .await
    }
}

/// Build the launch argv for the lockdown flow's detached placeholder container:
/// like [`container_argv`], but always `bridge` networking with a literal gateway
/// `HTTPS_PROXY` (no DNS needed), detached (`-d`), and an inert `sh` loop instead
/// of the real command — which arrives later via `docker exec` once the host-side
/// lockdown is in place. Pure and deterministic, mirroring `container_argv`.
fn container_lockdown_create_argv(
    runtime: &str,
    runtime_class: Option<&str>,
    image: &str,
    cmd: &SandboxCommand,
    gateway: &str,
    proxy_port: u16,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        runtime.to_string(),
        "run".into(),
        "-d".into(),
        "--rm".into(),
    ];
    if let Some(rc) = runtime_class {
        a.push("--runtime".into());
        a.push(rc.to_string());
    }
    a.push("--network".into());
    a.push("bridge".into());
    for var in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
        a.push("-e".into());
        a.push(format!("{var}=http://{gateway}:{proxy_port}"));
    }
    if let Some(bytes) = cmd.limits.memory_bytes {
        a.push("--memory".into());
        a.push(bytes.to_string());
    }
    if let Some(millis) = cmd.limits.cpu_millis {
        a.push("--cpus".into());
        a.push(format!("{:.3}", millis as f64 / 1000.0));
    }
    if let Some(pids) = cmd.limits.max_pids {
        a.push("--pids-limit".into());
        a.push(pids.to_string());
    }
    a.push("--read-only".into());
    a.push("--tmpfs".into());
    a.push("/tmp".into());
    a.push("--workdir".into());
    a.push("/workspace".into());
    a.push("--volume".into());
    a.push(format!("{}:/workspace", cmd.workdir));
    a.push(image.to_string());
    a.push("sh".into());
    a.push("-c".into());
    a.push("while :; do sleep 3600; done".into());
    a
}

/// Build the container launch argv. See [`ContainerSandbox::argv`].
fn container_argv(
    runtime: &str,
    runtime_class: Option<&str>,
    image: &str,
    cmd: &SandboxCommand,
    network: &NetworkPolicy,
    proxy_port: Option<u16>,
) -> Vec<String> {
    let mut a: Vec<String> = vec![runtime.to_string(), "run".into(), "--rm".into()];
    if let Some(rc) = runtime_class {
        a.push("--runtime".into());
        a.push(rc.to_string());
    }
    a.push("--network".into());
    a.push(
        if network.denies_all() {
            "none"
        } else {
            "bridge"
        }
        .into(),
    );
    // Route egress through the host proxy: reach the host via its gateway and point
    // the standard proxy env vars at it. Only allow-listed hosts will be tunneled.
    if let Some(port) = proxy_port {
        a.push("--add-host".into());
        a.push("host.docker.internal:host-gateway".into());
        for var in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
            a.push("-e".into());
            a.push(format!("{var}=http://host.docker.internal:{port}"));
        }
    }
    if let Some(bytes) = cmd.limits.memory_bytes {
        a.push("--memory".into());
        a.push(bytes.to_string());
    }
    if let Some(millis) = cmd.limits.cpu_millis {
        a.push("--cpus".into());
        a.push(format!("{:.3}", millis as f64 / 1000.0));
    }
    if let Some(pids) = cmd.limits.max_pids {
        a.push("--pids-limit".into());
        a.push(pids.to_string());
    }
    a.push("--read-only".into());
    a.push("--tmpfs".into());
    a.push("/tmp".into());
    a.push("--workdir".into());
    a.push("/workspace".into());
    a.push("--volume".into());
    a.push(format!("{}:/workspace", cmd.workdir));
    a.push(image.to_string());
    a.push(cmd.program.clone());
    a.extend(cmd.args.iter().cloned());
    a
}

/// A Firecracker microVM machine configuration
/// ([sandbox runtime §2](../../docs/07-tool-runtime/sandbox-runtime.md)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirecrackerConfig {
    /// Guest kernel image (uncompressed `vmlinux`).
    pub kernel_image_path: String,
    /// Root filesystem image (ext4) containing the guest agent.
    pub rootfs_path: String,
    /// Kernel boot arguments.
    pub boot_args: String,
    /// Virtual CPU count.
    pub vcpu_count: u32,
    /// Guest memory in MiB.
    pub mem_size_mib: u32,
}

impl FirecrackerConfig {
    /// Derive a config from resource limits: `cpu_millis`→`vcpu_count` (≥1),
    /// `memory_bytes`→`mem_size_mib` (≥128).
    pub fn from_limits(
        kernel_image_path: impl Into<String>,
        rootfs_path: impl Into<String>,
        limits: &ResourceLimits,
    ) -> Self {
        let vcpu_count = limits
            .cpu_millis
            .map(|m| (m as f64 / 1000.0).ceil() as u32)
            .unwrap_or(1)
            .max(1);
        let mem_size_mib = limits
            .memory_bytes
            .map(|b| (b / (1024 * 1024)) as u32)
            .unwrap_or(256)
            .max(128);
        Self {
            kernel_image_path: kernel_image_path.into(),
            rootfs_path: rootfs_path.into(),
            boot_args: "console=ttyS0 reboot=k panic=1 pci=off".into(),
            vcpu_count,
            mem_size_mib,
        }
    }

    /// Render the Firecracker machine configuration JSON (the `--config-file` body).
    /// Pure and deterministic — the basis for the backend's unit tests.
    pub fn to_json(&self) -> String {
        format!(
            concat!(
                "{{\n",
                "  \"boot-source\": {{\n",
                "    \"kernel_image_path\": \"{kernel}\",\n",
                "    \"boot_args\": \"{boot_args}\"\n",
                "  }},\n",
                "  \"drives\": [\n",
                "    {{\n",
                "      \"drive_id\": \"rootfs\",\n",
                "      \"path_on_host\": \"{rootfs}\",\n",
                "      \"is_root_device\": true,\n",
                "      \"is_read_only\": false\n",
                "    }}\n",
                "  ],\n",
                "  \"machine-config\": {{\n",
                "    \"vcpu_count\": {vcpu},\n",
                "    \"mem_size_mib\": {mem}\n",
                "  }}\n",
                "}}"
            ),
            kernel = self.kernel_image_path,
            boot_args = self.boot_args,
            rootfs = self.rootfs_path,
            vcpu = self.vcpu_count,
            mem = self.mem_size_mib,
        )
    }

    /// Render the execution config: the boot source plus three drives — a read-only
    /// rootfs (`/dev/vda`), a read-only **input** device (`/dev/vdb`, the command),
    /// and a writable **output** device (`/dev/vdc`, the guest agent's result). Boot
    /// args run the agent as init.
    pub fn to_exec_json(&self, input_path: &str, output_path: &str) -> String {
        format!(
            concat!(
                "{{\n",
                "  \"boot-source\": {{\n",
                "    \"kernel_image_path\": \"{kernel}\",\n",
                "    \"boot_args\": \"{boot_args} root=/dev/vda ro init=/init\"\n",
                "  }},\n",
                "  \"drives\": [\n",
                "    {{ \"drive_id\": \"rootfs\", \"path_on_host\": \"{rootfs}\", \"is_root_device\": true, \"is_read_only\": true }},\n",
                "    {{ \"drive_id\": \"input\", \"path_on_host\": \"{input}\", \"is_root_device\": false, \"is_read_only\": true }},\n",
                "    {{ \"drive_id\": \"output\", \"path_on_host\": \"{output}\", \"is_root_device\": false, \"is_read_only\": false }}\n",
                "  ],\n",
                "  \"machine-config\": {{ \"vcpu_count\": {vcpu}, \"mem_size_mib\": {mem} }}\n",
                "}}"
            ),
            kernel = self.kernel_image_path,
            boot_args = self.boot_args,
            rootfs = self.rootfs_path,
            input = input_path,
            output = output_path,
            vcpu = self.vcpu_count,
            mem = self.mem_size_mib,
        )
    }
}

/// A Firecracker microVM sandbox.
///
/// Runs a command inside a microVM via a one-shot protocol over block devices: the
/// host writes the command to a read-only **input** drive, boots the kernel + rootfs
/// (whose `/init` is the apex guest agent), and the agent executes the command,
/// writes a structured result to the writable **output** drive, and reboots — which
/// makes Firecracker exit. The host then reads the result back. Needs a guest kernel
/// and a rootfs carrying the agent (see `FirecrackerConfig`); without one (or KVM),
/// `execute` returns [`SandboxError::Internal`].
#[derive(Clone, Debug)]
pub struct FirecrackerSandbox {
    config: Option<FirecrackerConfig>,
    firecracker_bin: String,
}

impl Default for FirecrackerSandbox {
    fn default() -> Self {
        Self::new()
    }
}

/// Monotonic run sequence for unique per-execution working directories.
static FC_RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl FirecrackerSandbox {
    /// An unconfigured microVM sandbox (no guest image).
    pub fn new() -> Self {
        Self {
            config: None,
            firecracker_bin: "firecracker".to_string(),
        }
    }

    /// Attach a guest VM configuration (kernel + rootfs).
    pub fn with_config(config: FirecrackerConfig) -> Self {
        Self {
            config: Some(config),
            firecracker_bin: "firecracker".to_string(),
        }
    }

    /// Override the `firecracker` binary path.
    pub fn with_binary(mut self, bin: impl Into<String>) -> Self {
        self.firecracker_bin = bin.into();
        self
    }

    /// The attached guest configuration, if any.
    pub fn config(&self) -> Option<&FirecrackerConfig> {
        self.config.as_ref()
    }

    /// Boot the microVM for `cmd` inside `dir`, returning the guest's outcome.
    async fn run_vm(
        &self,
        config: &FirecrackerConfig,
        cmd: &SandboxCommand,
        dir: &std::path::Path,
    ) -> Result<CommandOutcome, SandboxError> {
        let input_path = dir.join("input.img");
        let output_path = dir.join("output.img");
        let config_path = dir.join("config.json");

        // Input drive: the raw shell command, zero-padded to a block-sized device.
        let shell_cmd = build_guest_command(cmd);
        write_block_device(&input_path, shell_cmd.as_bytes(), 64 * 1024).await?;
        // Output drive: zeroed, for the agent's result.
        write_block_device(&output_path, &[], 1024 * 1024).await?;

        let cfg = config.to_exec_json(&path_str(&input_path)?, &path_str(&output_path)?);
        tokio::fs::write(&config_path, cfg)
            .await
            .map_err(|e| SandboxError::Internal(format!("write firecracker config: {e}")))?;

        let mut child = Command::new(&self.firecracker_bin)
            .arg("--no-api")
            .arg("--config-file")
            .arg(&config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SandboxError::Spawn(format!("firecracker: {e}")))?;

        match tokio::time::timeout(cmd.limits.timeout, child.wait()).await {
            Ok(Ok(_status)) => {
                let raw = tokio::fs::read(&output_path)
                    .await
                    .map_err(|e| SandboxError::Internal(format!("read guest result: {e}")))?;
                parse_guest_result(&raw, cmd.limits.max_output_bytes)
            }
            Ok(Err(e)) => Err(SandboxError::Internal(format!("firecracker wait: {e}"))),
            Err(_elapsed) => {
                let _ = child.start_kill();
                Ok(CommandOutcome {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("microVM exceeded timeout of {:?}", cmd.limits.timeout),
                    timed_out: true,
                    truncated: false,
                    signal: None,
                    resource_exceeded: false,
                })
            }
        }
    }
}

#[async_trait]
impl Sandbox for FirecrackerSandbox {
    fn backend(&self) -> SandboxBackend {
        SandboxBackend::Firecracker
    }

    async fn execute(&self, cmd: &SandboxCommand) -> Result<CommandOutcome, SandboxError> {
        let config = self.config.as_ref().ok_or_else(|| {
            SandboxError::Internal("firecracker sandbox has no guest config".to_string())
        })?;

        // Per-execution scratch dir for the config + input/output drives.
        let seq = FC_RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("apex-fc-{}-{seq}", std::process::id()));
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| SandboxError::Internal(format!("create scratch dir: {e}")))?;

        let result = self.run_vm(config, cmd, &dir).await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        result
    }
}

/// Build the shell command line the guest agent will `eval`, quoting safely.
fn build_guest_command(cmd: &SandboxCommand) -> String {
    let mut line = String::new();
    if !cmd.workdir.is_empty() && cmd.workdir != "." {
        line.push_str(&format!("cd {} 2>/dev/null; ", shell_quote(&cmd.workdir)));
    }
    line.push_str(&shell_quote(&cmd.program));
    for arg in &cmd.args {
        line.push(' ');
        line.push_str(&shell_quote(arg));
    }
    line
}

/// Single-quote a string for POSIX `sh`, escaping embedded single quotes.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Absolute-path string, or an internal error if the path isn't valid UTF-8.
fn path_str(path: &std::path::Path) -> Result<String, SandboxError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| SandboxError::Internal("non-UTF-8 path".to_string()))
}

/// Write `data` to `path` as a block-aligned device file, zero-padded to at least
/// `min_size` (rounded up to a 512-byte multiple).
async fn write_block_device(
    path: &std::path::Path,
    data: &[u8],
    min_size: usize,
) -> Result<(), SandboxError> {
    let size = data.len().max(min_size).div_ceil(512) * 512;
    let mut buf = vec![0u8; size];
    buf[..data.len()].copy_from_slice(data);
    tokio::fs::write(path, buf)
        .await
        .map_err(|e| SandboxError::Internal(format!("write device {}: {e}", path.display())))
}

/// Parse the guest agent's result blob: `APEXR1 / <rc> / b64(stdout) / b64(stderr) /
/// APEXEOF`, each on its own line, padded with NULs.
fn parse_guest_result(raw: &[u8], max_output: usize) -> Result<CommandOutcome, SandboxError> {
    let mut lines = raw.split(|&b| b == b'\n');
    let bad = || SandboxError::Internal("malformed guest result".to_string());

    if lines.next() != Some(b"APEXR1".as_ref()) {
        return Err(bad());
    }
    let rc_line = lines.next().ok_or_else(bad)?;
    let exit_code: i32 = std::str::from_utf8(rc_line)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(bad)?;
    let stdout_b64 = lines.next().ok_or_else(bad)?;
    let stderr_b64 = lines.next().ok_or_else(bad)?;

    let stdout_bytes = base64_decode(stdout_b64).ok_or_else(bad)?;
    let stderr_bytes = base64_decode(stderr_b64).ok_or_else(bad)?;
    let (stdout, t1) = cap(&stdout_bytes, max_output);
    let (stderr, t2) = cap(&stderr_bytes, max_output);

    Ok(CommandOutcome {
        exit_code: Some(exit_code),
        stdout,
        stderr,
        timed_out: false,
        truncated: t1 || t2,
        signal: None,
        resource_exceeded: false,
    })
}

/// Decode standard base64 (ignoring `=` padding and newlines). `None` on a bad byte.
fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &c in input {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        buf = (buf << 6) | val(c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Fuel units granted per millisecond of CPU budget. Wasmtime fuel meters executed
/// instructions, not wall-clock time, so this is an approximate compute budget
/// rather than a precise CPU-time limit (which the [`NativeSandbox`] enforces).
#[cfg(feature = "wasi")]
const WASI_FUEL_PER_MILLI: u64 = 1_000_000;

/// A WASI/WASM sandbox: runs a `wasm32-wasi` module in an in-process Wasmtime VM
/// with capability-based isolation ([sandbox runtime §2](../../docs/07-tool-runtime/sandbox-runtime.md)).
///
/// Unlike the process backends, [`SandboxCommand::program`] is the path to a
/// `.wasm` module and [`SandboxCommand::args`] are its WASI argv. Isolation is
/// capability-based: the guest gets no network and no filesystem beyond the
/// bind-mounted `workdir` (preopened at `.`), so a deny-all [`NetworkPolicy`] is the
/// default and needs no enforcement. Limits map to Wasmtime primitives: memory →
/// `StoreLimits`, CPU → fuel, wall-clock → epoch interruption.
///
/// Enabled by the `wasi` cargo feature.
#[cfg(feature = "wasi")]
#[derive(Clone)]
pub struct WasiSandbox {
    engine: wasmtime::Engine,
}

#[cfg(feature = "wasi")]
struct WasiState {
    wasi: wasmtime_wasi::WasiCtx,
    limits: wasmtime::StoreLimits,
}

#[cfg(feature = "wasi")]
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

#[cfg(feature = "wasi")]
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
#[cfg(feature = "wasi")]
fn pipe_bytes(pipe: wasi_common::pipe::WritePipe<std::io::Cursor<Vec<u8>>>) -> Vec<u8> {
    pipe.try_into_inner()
        .map(std::io::Cursor::into_inner)
        .unwrap_or_default()
}

/// Map a process exit status to its terminating Unix signal and whether that signal
/// indicates a resource-limit breach (`SIGXCPU`/`SIGXFSZ`).
#[cfg(unix)]
fn terminating_signal(status: &std::process::ExitStatus) -> (Option<i32>, bool) {
    use std::os::unix::process::ExitStatusExt;
    let signal = status.signal();
    let exceeded = matches!(signal, Some(libc::SIGXCPU) | Some(libc::SIGXFSZ));
    (signal, exceeded)
}

#[cfg(not(unix))]
fn terminating_signal(_status: &std::process::ExitStatus) -> (Option<i32>, bool) {
    (None, false)
}

/// Apply `setrlimit` caps in the forked child before `exec`. Async-signal-safe:
/// only stack memory and `setrlimit` syscalls.
#[cfg(unix)]
fn apply_rlimits(limits: &ResourceLimits) -> std::io::Result<()> {
    fn set(resource: libc::__rlimit_resource_t, soft: u64, hard: u64) -> std::io::Result<()> {
        let lim = libc::rlimit {
            rlim_cur: soft as libc::rlim_t,
            rlim_max: hard as libc::rlim_t,
        };
        // SAFETY: `lim` is a valid, fully-initialized rlimit for the duration of the call.
        if unsafe { libc::setrlimit(resource, &lim) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    if let Some(bytes) = limits.memory_bytes {
        set(libc::RLIMIT_AS, bytes, bytes)?;
    }
    if let Some(millis) = limits.cpu_millis {
        // RLIMIT_CPU is whole seconds: SIGXCPU at the soft limit, SIGKILL one
        // second later at the hard limit.
        let secs = millis.div_ceil(1000).max(1) as u64;
        set(libc::RLIMIT_CPU, secs, secs + 1)?;
    }
    Ok(())
}

/// Truncate `bytes` to `max` (UTF-8 lossy); returns the string and whether it was cut.
fn cap(bytes: &[u8], max: usize) -> (String, bool) {
    if bytes.len() > max {
        (String::from_utf8_lossy(&bytes[..max]).into_owned(), true)
    } else {
        (String::from_utf8_lossy(bytes).into_owned(), false)
    }
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

    fn sample_cmd() -> SandboxCommand {
        SandboxCommand {
            program: "echo".into(),
            args: vec!["hi".into()],
            workdir: "/work".into(),
            env: vec![],
            limits: ResourceLimits {
                memory_bytes: Some(256 * 1024 * 1024),
                cpu_millis: Some(1500),
                max_pids: Some(64),
                ..ResourceLimits::default()
            },
        }
    }

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
        assert!(!out.resource_exceeded);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn enforces_timeout() {
        let sb = NativeSandbox::new(Duration::from_millis(150));
        let out = sb.run("sh", &["-c", "sleep 5"], ".").await.unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cpu_rlimit_terminates_with_resource_exceeded() {
        // A busy loop with a 1s CPU cap is killed by SIGXCPU well before the 30s
        // wall-clock timeout, so the breach is attributed to the resource limit.
        let limits = ResourceLimits {
            timeout: Duration::from_secs(30),
            cpu_millis: Some(1000),
            ..ResourceLimits::default()
        };
        let sb = NativeSandbox::with_limits(limits);
        let out = sb
            .run("sh", &["-c", "while :; do :; done"], ".")
            .await
            .unwrap();
        assert!(!out.timed_out, "should hit the CPU cap, not the wall clock");
        assert!(out.resource_exceeded);
        assert_eq!(out.signal, Some(libc::SIGXCPU));
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

    #[test]
    fn container_argv_wires_egress_proxy() {
        let sb = ContainerSandbox::docker("alpine:3.20").with_network(NetworkPolicy {
            default_deny: true,
            outbound_allow: vec!["api.example.com".to_string()],
        });
        let argv = sb.argv_with_proxy(&sample_cmd(), Some(8080)).join(" ");
        // Allow-list → bridge networking + host-gateway + proxy env pointing at the proxy.
        assert!(argv.contains("--network bridge"), "{argv}");
        assert!(
            argv.contains("--add-host host.docker.internal:host-gateway"),
            "{argv}"
        );
        assert!(
            argv.contains("HTTPS_PROXY=http://host.docker.internal:8080"),
            "{argv}"
        );
        // Without a proxy port, no proxy wiring is emitted.
        assert!(!sb.argv(&sample_cmd()).join(" ").contains("HTTPS_PROXY"));
    }

    #[test]
    fn lockdown_create_argv_uses_literal_gateway_and_a_placeholder_command() {
        let argv = container_lockdown_create_argv(
            "docker",
            None,
            "alpine:3.20",
            &sample_cmd(),
            "172.17.0.1",
            8080,
        )
        .join(" ");
        // Detached, so the host can lock down the network before anything real runs.
        assert!(argv.contains(" -d "), "{argv}");
        // The literal gateway IP, not a hostname the container would need to
        // resolve via DNS (the lockdown's allow-list has no DNS exception).
        assert!(
            argv.contains("HTTPS_PROXY=http://172.17.0.1:8080"),
            "{argv}"
        );
        assert!(!argv.contains("host.docker.internal"), "{argv}");
        // The real command never appears in the launch argv — it arrives later via
        // `docker exec`, once the lockdown is in place.
        assert!(!argv.contains(&sample_cmd().program), "{argv}");
        assert!(argv.trim_end().ends_with("while :; do sleep 3600; done"));
    }

    #[test]
    fn container_argv_denies_network_and_maps_limits() {
        let argv = ContainerSandbox::docker("alpine:3.20").argv(&sample_cmd());
        // Deny-all egress isolates the network namespace.
        let net = argv.windows(2).find(|w| w[0] == "--network").unwrap();
        assert_eq!(net[1], "none");
        // cgroup limit flags are derived from ResourceLimits.
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--memory" && w[1] == "268435456")
        );
        assert!(argv.windows(2).any(|w| w[0] == "--cpus" && w[1] == "1.500"));
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--pids-limit" && w[1] == "64")
        );
        // Read-only rootfs + bind-mounted workspace.
        assert!(argv.iter().any(|s| s == "--read-only"));
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--volume" && w[1] == "/work:/workspace")
        );
        // The wrapped command comes last, after the image.
        let img = argv.iter().position(|s| s == "alpine:3.20").unwrap();
        assert_eq!(&argv[img + 1..], &["echo".to_string(), "hi".to_string()]);
        // No runtime override for the plain container backend.
        assert!(!argv.iter().any(|s| s == "--runtime"));
    }

    #[test]
    fn gvisor_argv_sets_runsc_runtime() {
        let argv = ContainerSandbox::gvisor("alpine:3.20").argv(&sample_cmd());
        let rt = argv.windows(2).find(|w| w[0] == "--runtime").unwrap();
        assert_eq!(rt[1], "runsc");
    }

    #[test]
    fn container_argv_allows_bridge_when_egress_permitted() {
        let policy = NetworkPolicy {
            default_deny: false,
            outbound_allow: vec![],
        };
        let argv = ContainerSandbox::docker("alpine:3.20")
            .with_network(policy)
            .argv(&sample_cmd());
        let net = argv.windows(2).find(|w| w[0] == "--network").unwrap();
        assert_eq!(net[1], "bridge");
    }

    #[test]
    fn firecracker_config_derives_from_limits_and_renders_json() {
        let limits = ResourceLimits {
            cpu_millis: Some(2000),
            memory_bytes: Some(512 * 1024 * 1024),
            ..ResourceLimits::default()
        };
        let cfg = FirecrackerConfig::from_limits("/vmlinux", "/rootfs.ext4", &limits);
        assert_eq!(cfg.vcpu_count, 2);
        assert_eq!(cfg.mem_size_mib, 512);
        let json = cfg.to_json();
        assert!(json.contains("\"kernel_image_path\": \"/vmlinux\""));
        assert!(json.contains("\"path_on_host\": \"/rootfs.ext4\""));
        assert!(json.contains("\"vcpu_count\": 2"));
        assert!(json.contains("\"mem_size_mib\": 512"));
    }

    #[tokio::test]
    async fn firecracker_without_guest_config_errors() {
        // Execution needs a kernel + rootfs; an unconfigured sandbox fails fast.
        let err = FirecrackerSandbox::new()
            .execute(&sample_cmd())
            .await
            .unwrap_err();
        assert!(
            matches!(err, SandboxError::Internal(m) if m.contains("no guest config")),
            "expected a missing-config error"
        );
    }

    #[test]
    fn firecracker_exec_json_has_three_drives_and_agent_init() {
        let cfg =
            FirecrackerConfig::from_limits("/k/vmlinux", "/r/rootfs.ext4", &sample_cmd().limits);
        let json = cfg.to_exec_json("/run/input.img", "/run/output.img");
        assert!(
            json.contains("init=/init"),
            "agent must run as init: {json}"
        );
        assert!(json.contains("\"drive_id\": \"input\""));
        assert!(json.contains("\"drive_id\": \"output\""));
        assert!(json.contains("\"path_on_host\": \"/run/output.img\""));
    }

    #[test]
    fn build_guest_command_quotes_args() {
        let mut c = sample_cmd();
        c.program = "echo".into();
        c.args = vec!["a b".into(), "it's".into()];
        let line = build_guest_command(&c);
        // Each arg is single-quoted; an embedded quote is escaped.
        assert!(line.contains(r#"'a b'"#), "got: {line}");
        assert!(line.contains(r#"'it'\''s'"#), "got: {line}");
    }

    #[test]
    fn parse_guest_result_decodes_stdout_and_exit() {
        // "ok\n" base64 = "b2sK"; empty stderr.
        let raw = b"APEXR1\n0\nb2sK\n\nAPEXEOF\n\0\0\0";
        let out = parse_guest_result(raw, 1 << 20).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout, "ok\n");
        assert_eq!(out.stderr, "");
    }

    // ---- WASI/WASM backend (feature = "wasi") ----------------------------------

    /// A WASI module that writes "apex_wasi_ok\n" to stdout via `fd_write`.
    #[cfg(feature = "wasi")]
    const PRINT_WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 8) "apex_wasi_ok\0a")
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
    #[cfg(feature = "wasi")]
    const LOOP_WAT: &str = r#"(module (func (export "_start") (loop (br 0))))"#;

    /// A WASI module that dumps its environment block to stdout: reads `environ_sizes_get`
    /// then `environ_get` and writes the raw `KEY=VALUE\0…` buffer to fd 1. Used to prove
    /// that `SandboxCommand.env` reaches the guest.
    #[cfg(feature = "wasi")]
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

    #[cfg(feature = "wasi")]
    fn wasm_temp(wat_src: &str, tag: &str) -> std::path::PathBuf {
        let bytes = wat::parse_str(wat_src).expect("assemble wat");
        let mut path = std::env::temp_dir();
        path.push(format!("apex_wasi_{tag}_{}.wasm", std::process::id()));
        std::fs::write(&path, bytes).expect("write wasm fixture");
        path
    }

    #[cfg(feature = "wasi")]
    fn wasi_cmd(path: &std::path::Path, limits: ResourceLimits) -> SandboxCommand {
        SandboxCommand {
            program: path.to_string_lossy().into_owned(),
            args: vec![],
            workdir: ".".into(),
            env: vec![],
            limits,
        }
    }

    #[cfg(feature = "wasi")]
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
            out.stdout.contains("apex_wasi_ok"),
            "stdout: {:?}",
            out.stdout
        );
    }

    #[cfg(feature = "wasi")]
    #[tokio::test]
    async fn wasi_injects_env_into_guest() {
        let path = wasm_temp(ENVIRON_WAT, "environ");
        let mut cmd = wasi_cmd(&path, ResourceLimits::default());
        cmd.env = vec![("APEX_SECRET_DB_TOKEN".into(), "hunter2".into())];
        let sb = WasiSandbox::new().unwrap();
        let out = sb.execute(&cmd).await.unwrap();
        assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
        assert!(
            out.stdout.contains("APEX_SECRET_DB_TOKEN=hunter2"),
            "injected env var should reach the guest; stdout: {:?}",
            out.stdout
        );
    }

    #[cfg(feature = "wasi")]
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

    #[cfg(feature = "wasi")]
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

    #[cfg(feature = "wasi")]
    #[tokio::test]
    async fn wasi_missing_module_is_spawn_error() {
        let sb = WasiSandbox::new().unwrap();
        let cmd = wasi_cmd(
            std::path::Path::new("/nonexistent/apex.wasm"),
            ResourceLimits::default(),
        );
        let err = sb.execute(&cmd).await.unwrap_err();
        assert!(matches!(err, SandboxError::Spawn(_)));
    }

    #[cfg(feature = "wasi")]
    #[tokio::test]
    async fn wasi_backend_is_detected() {
        let mgr = SandboxManager::detect().await;
        assert!(mgr.capabilities().contains(&SandboxBackend::Wasi));
    }
}
