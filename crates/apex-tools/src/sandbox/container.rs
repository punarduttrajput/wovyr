//! [`ContainerSandbox`] — a Docker/Podman (and gVisor) sandbox.

use super::types::{
    CommandOutcome, NetworkPolicy, ResourceLimits, SandboxBackend, SandboxCommand, SandboxError,
};
use super::{NativeSandbox, Sandbox};
use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

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
}
