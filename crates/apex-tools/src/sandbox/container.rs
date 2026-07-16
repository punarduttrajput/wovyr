//! [`ContainerSandbox`] — a Docker/Podman (and gVisor) sandbox.

use super::types::{
    CommandOutcome, NetworkPolicy, ResourceLimits, SandboxBackend, SandboxCommand, SandboxError,
};
use super::{Sandbox, cap};
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
            false,
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
        self.execute_inner(cmd, None).await
    }
}

impl ContainerSandbox {
    /// Like [`Sandbox::execute`], but launches the container interactive (`-i`),
    /// feeds `stdin` to the wrapped command, and closes the pipe (EOF) once written —
    /// the stdin/stdout half of the plugin capability ABI
    /// ([sandbox §loading model](../../../docs/08-plugin-sdk/sandbox.md)).
    pub async fn execute_with_stdin(
        &self,
        cmd: &SandboxCommand,
        stdin: Vec<u8>,
    ) -> Result<CommandOutcome, SandboxError> {
        self.execute_inner(cmd, Some(stdin)).await
    }

    async fn execute_inner(
        &self,
        cmd: &SandboxCommand,
        stdin: Option<Vec<u8>>,
    ) -> Result<CommandOutcome, SandboxError> {
        if self.network.needs_proxy() {
            // Fail closed (SBX-304) *before* ever starting a container: a non-empty
            // egress allow-list is only meaningfully enforced by the host-side
            // `nsenter`/`iptables` lockdown, which needs a Linux host and a Docker
            // runtime (see the platform matrix on `egress_lockdown`'s module docs).
            // Refusing here, rather than letting `execute_with_egress_lockdown` run
            // and only then fail deep inside the lockdown sequence, turns an
            // accidental (if still fail-closed) missing-binary spawn error into a
            // deliberate, clearly-worded refusal — and never starts a container
            // whose egress this platform/runtime combination can't actually confine.
            if !crate::egress_lockdown::lockdown_supported() {
                return Err(SandboxError::Internal(
                    "a non-empty egress allow-list needs the host-side network-namespace \
                     lockdown (nsenter + iptables), which is only supported on a Linux \
                     host; refusing to run with an unenforceable network policy rather \
                     than silently allowing full container egress"
                        .to_string(),
                ));
            }
            if self.runtime != "docker" {
                return Err(SandboxError::Internal(format!(
                    "a non-empty egress allow-list needs the host-side lockdown, which \
                     only understands Docker's `network inspect` output shape; refusing \
                     to run under the `{}` runtime rather than silently allowing full \
                     container egress",
                    self.runtime
                )));
            }
            return self.execute_with_egress_lockdown(cmd, stdin).await;
        }

        // Deny-all (`--network none`, already airtight) or a fully open bridge
        // (the policy explicitly permits all egress): no proxy, no lockdown, a
        // single foreground `run` is sufficient.
        let argv = container_argv(
            &self.runtime,
            self.runtime_class.as_deref(),
            &self.image,
            cmd,
            &self.network,
            None,
            stdin.is_some(),
        );
        run_cli(&argv, &cmd.env, stdin, &cmd.limits).await
    }
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
        stdin: Option<Vec<u8>>,
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
            .run_locked_down(cmd, stdin, &container_id, &gateway, proxy.port())
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
        stdin: Option<Vec<u8>>,
        container_id: &str,
        gateway: &str,
        proxy_port: u16,
    ) -> Result<CommandOutcome, SandboxError> {
        let pid = crate::egress_lockdown::container_pid(&self.runtime, container_id).await?;
        crate::egress_lockdown::lock_down_egress(pid, gateway, proxy_port).await?;

        let mut exec_argv = vec![self.runtime.clone(), "exec".to_string()];
        if stdin.is_some() {
            exec_argv.push("-i".to_string());
        }
        // Env variable *names* only — the values travel via the CLI's own process
        // environment (see `run_cli`), never the argv.
        for (name, _) in &cmd.env {
            exec_argv.push("-e".to_string());
            exec_argv.push(name.clone());
        }
        exec_argv.push(container_id.to_string());
        exec_argv.push(cmd.program.clone());
        exec_argv.extend(cmd.args.iter().cloned());

        // The container's own cgroup limits (already applied at creation) enforce
        // memory/CPU/pids; this outer run only arms the wall-clock timeout and
        // output cap, exactly like the non-lockdown path.
        run_cli(&exec_argv, &cmd.env, stdin, &cmd.limits).await
    }
}

/// Run a container-CLI invocation (`docker run`/`docker exec`), arming only the
/// wall-clock timeout and output cap — resource enforcement is the container's
/// cgroups, applied via the argv's own flags. `client_env` is set on the CLI
/// *process* so `-e NAME` argv entries resolve to their values daemon-side without
/// the values ever appearing in a host process listing; `stdin` (when present) is
/// written to the CLI's piped stdin and closed (EOF) once delivered.
async fn run_cli(
    argv: &[String],
    client_env: &[(String, String)],
    stdin: Option<Vec<u8>>,
    limits: &ResourceLimits,
) -> Result<CommandOutcome, SandboxError> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in client_env {
        command.env(name, value);
    }
    let mut child = command
        .spawn()
        .map_err(|e| SandboxError::Spawn(format!("`{}`: {e}", argv[0])))?;
    if let Some(bytes) = stdin {
        use tokio::io::AsyncWriteExt;
        let mut pipe = child
            .stdin
            .take()
            .ok_or_else(|| SandboxError::Internal("container stdin pipe unavailable".into()))?;
        // Write concurrently with the output drain: a guest that emits output
        // before consuming its input must not deadlock against a full pipe.
        tokio::spawn(async move {
            let _ = pipe.write_all(&bytes).await;
            let _ = pipe.shutdown().await;
        });
    }
    match tokio::time::timeout(limits.timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let (stdout, t1) = cap(&output.stdout, limits.max_output_bytes);
            let (stderr, t2) = cap(&output.stderr, limits.max_output_bytes);
            let (signal, resource_exceeded) = super::native::terminating_signal(&output.status);
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
            stderr: format!("execution exceeded timeout of {:?}", limits.timeout),
            timed_out: true,
            truncated: false,
            signal: None,
            resource_exceeded: false,
        }),
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

/// Build the container launch argv. See [`ContainerSandbox::argv`]. With `stdin`,
/// the run is interactive (`-i`) so piped input reaches the wrapped command.
/// [`SandboxCommand::env`] contributes variable *names* only (`-e NAME`) — the
/// values are supplied through the container CLI's process environment (`run_cli`),
/// keeping secrets out of host process listings.
fn container_argv(
    runtime: &str,
    runtime_class: Option<&str>,
    image: &str,
    cmd: &SandboxCommand,
    network: &NetworkPolicy,
    proxy_port: Option<u16>,
    stdin: bool,
) -> Vec<String> {
    let mut a: Vec<String> = vec![runtime.to_string(), "run".into(), "--rm".into()];
    if stdin {
        a.push("-i".into());
    }
    for (name, _) in &cmd.env {
        a.push("-e".into());
        a.push(name.clone());
    }
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
    fn stdin_run_is_interactive_and_env_argv_carries_names_only() {
        let mut c = sample_cmd();
        c.env = vec![("APEX_SECRET_TOKEN".into(), "t0p-secret".into())];
        let argv = container_argv(
            "docker",
            None,
            "alpine:3.20",
            &c,
            &NetworkPolicy::default(),
            None,
            true,
        );
        // Interactive, so piped stdin reaches the wrapped command.
        assert!(argv.iter().any(|s| s == "-i"), "{argv:?}");
        // The env var travels by *name*; its value must never hit the argv
        // (it would be visible in a host `ps` otherwise).
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "APEX_SECRET_TOKEN"),
            "{argv:?}"
        );
        assert!(!argv.join(" ").contains("t0p-secret"), "{argv:?}");
        // A stdin-less, env-free run keeps its exact pre-stdin argv shape.
        let plain = ContainerSandbox::docker("alpine:3.20").argv(&sample_cmd());
        assert!(!plain.iter().any(|s| s == "-i" || s == "-e"), "{plain:?}");
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

    // --- SBX-304: fail closed when the L3 egress lockdown isn't available -----------

    fn allowlist_policy() -> NetworkPolicy {
        NetworkPolicy {
            default_deny: true,
            outbound_allow: vec!["api.example.com".to_string()],
        }
    }

    /// A non-empty egress allow-list run is refused outright on a host that can't
    /// enforce the lockdown — proven directly on whichever platform this test suite
    /// actually runs on: the check runs *before* any container starts, so this needs
    /// neither a real Docker daemon nor a Linux host to observe the refusal (it's
    /// exactly the platforms this is meant to protect that make the test meaningful).
    #[tokio::test]
    async fn network_policy_run_is_refused_when_lockdown_is_unsupported_on_this_host() {
        if crate::egress_lockdown::lockdown_supported() {
            eprintln!(
                "skipping: this test exercises the refusal path on a host where the \
                 lockdown is *not* supported (this one supports it)"
            );
            return;
        }
        let sandbox = ContainerSandbox::docker("alpine:3.20").with_network(allowlist_policy());
        let err = sandbox.execute(&sample_cmd()).await.unwrap_err();
        assert!(matches!(err, SandboxError::Internal(_)), "{err:?}");
        // The refusal names the actual reason (host-side lockdown unavailability),
        // not a generic/unrelated failure.
        assert!(err.to_string().contains("Linux"), "{err}");
    }

    /// The gVisor backend goes through the identical `execute` gate (it's the same
    /// `ContainerSandbox` type, just with `runtime_class: Some("runsc")`) — the
    /// refusal isn't accidentally scoped to the plain Docker constructor only.
    #[tokio::test]
    async fn gvisor_network_policy_run_is_also_refused_when_lockdown_is_unsupported() {
        if crate::egress_lockdown::lockdown_supported() {
            eprintln!("skipping: this test exercises the refusal path on a non-Linux host");
            return;
        }
        let sandbox = ContainerSandbox::gvisor("alpine:3.20").with_network(allowlist_policy());
        let err = sandbox.execute(&sample_cmd()).await.unwrap_err();
        assert!(matches!(err, SandboxError::Internal(_)), "{err:?}");
    }

    /// A Podman runtime is refused too, even on a host that *does* support the Linux
    /// lockdown mechanism — this module only speaks Docker's `network inspect`
    /// output shape. The platform check runs first, so on a non-Linux host this
    /// sandbox is already refused for that reason (covered by the two tests above);
    /// here we only additionally check the Podman-specific message when the
    /// platform check alone would otherwise have let it through.
    #[tokio::test]
    async fn podman_network_policy_run_is_refused_regardless_of_platform_support() {
        let sandbox = ContainerSandbox::podman("alpine:3.20").with_network(allowlist_policy());
        let err = sandbox.execute(&sample_cmd()).await.unwrap_err();
        assert!(matches!(err, SandboxError::Internal(_)), "{err:?}");
        if crate::egress_lockdown::lockdown_supported() {
            assert!(err.to_string().contains("Docker"), "{err}");
        }
    }

    /// A deny-all or fully-open policy needs no lockdown at all, on any platform —
    /// the refusal above must not become an accidental blanket ban on every
    /// container run, only the specific case that actually needs the lockdown.
    #[tokio::test]
    async fn deny_all_and_allow_all_policies_are_unaffected_by_the_lockdown_gate() {
        // Neither of these calls `execute_with_egress_lockdown` at all (`needs_proxy()`
        // is false for both), so neither is refused by the SBX-304 gate — whatever
        // happens next depends only on whether a real container runtime is present,
        // which this test doesn't require: it just asserts the error (if any) is
        // never the lockdown-unsupported message.
        let deny_all = ContainerSandbox::docker("alpine:3.20");
        if let Err(e) = deny_all.execute(&sample_cmd()).await {
            assert!(!e.to_string().contains("egress allow-list"), "{e}");
        }
        let allow_all = ContainerSandbox::docker("alpine:3.20").with_network(NetworkPolicy {
            default_deny: false,
            outbound_allow: vec![],
        });
        if let Err(e) = allow_all.execute(&sample_cmd()).await {
            assert!(!e.to_string().contains("egress allow-list"), "{e}");
        }
    }
}
