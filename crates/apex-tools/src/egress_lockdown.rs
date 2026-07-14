//! Host-side network-namespace egress lockdown — closes the "L3 egress
//! bypass-blocking" gap: [`crate::egress::EgressProxy`] enforces a per-host
//! allow-list, but a container on `--network bridge` has always had full outbound
//! connectivity underneath it. A workload that simply ignores `HTTPS_PROXY` and
//! dials out directly was never actually stopped — the proxy is cooperative, not
//! enforced. This module makes it enforced: after the container starts (running an
//! inert placeholder, not the real command), the *host* attaches to the
//! container's network namespace via `nsenter` and applies an `iptables`
//! default-deny to its `OUTPUT` chain, allowing only loopback and the egress
//! proxy's address. Only then does the real command run (via `docker exec`).
//!
//! Running `iptables` as a host process inside the container's netns (rather than
//! granting the container `CAP_NET_ADMIN` to lock itself down) is the point: the
//! sandboxed workload never has the capability to see or undo these rules.
//!
//! Linux/Docker-specific (needs `nsenter` + `iptables` on the host, and a Docker
//! daemon whose `network inspect` reports an IPAM gateway — Podman's output shape
//! may differ and is not handled here). Fails closed: if any step fails, the
//! caller must not proceed to run the untrusted command.
//!
//! ## Platform matrix (SBX-304)
//!
//! | Host OS | `default_deny` + empty allow-list (deny-all) | `default_deny: false` (allow-all) | `default_deny` + non-empty allow-list (needs the proxy + lockdown) |
//! |---|---|---|---|
//! | Linux + Docker | `--network none` (airtight, no lockdown needed) | plain `bridge` (no lockdown needed) | **enforced** via this module |
//! | Windows / macOS (incl. Docker Desktop) | `--network none` (airtight, no lockdown needed) | plain `bridge` (no lockdown needed) | **refused** ([`lockdown_supported`] is `false`) |
//! | Any host, Podman runtime | `--network none` (airtight) | plain `bridge` | **refused** — this module only speaks Docker's `network inspect` output shape |
//!
//! Only the last column needs this module at all: a full deny (`--network none`)
//! or a fully-open bridge need no per-host lockdown regardless of platform,
//! since there's nothing partial to enforce. [`lockdown_supported`] is the single
//! gate [`crate::sandbox::ContainerSandbox::execute`] checks **before** starting
//! any container for that case — refusing up front (`SandboxError::Internal`,
//! naming the reason) rather than starting a container and only then discovering,
//! deep inside the lockdown sequence, that `nsenter` doesn't exist on this host.
//! That earlier failure mode was still fail-closed in effect (the untrusted
//! command's `docker exec` is sequenced after the lockdown call, which errors
//! first), but relied on a missing-binary spawn error as an accidental side
//! effect rather than a deliberate, documented platform check — and gave a less
//! specific error, and wasted a container start attempting the impossible.

use crate::sandbox::SandboxError;
use std::process::Stdio;
use tokio::process::Command;

/// Whether this host can actually enforce the L3 egress lockdown this module
/// implements (SBX-304) — checked from the compile target, not by probing for
/// `nsenter`/`iptables` at runtime: those tools operate on Linux network
/// namespaces specifically, so even a hypothetical non-Linux host that happened
/// to have same-named binaries on `PATH` (e.g. under some compatibility layer)
/// would not be attaching to the same kind of namespace this module was built
/// and proven against. Linux is the only supported host for the lockdown path;
/// see the platform matrix above for what happens on every other platform (or
/// with a non-Docker container runtime).
pub(crate) fn lockdown_supported() -> bool {
    cfg!(target_os = "linux")
}

/// The gateway IP of a docker `network` (default `"bridge"`) — the address a
/// container reaches the host at. Resolved once per run and used both as the
/// literal `HTTPS_PROXY` target (so the sandboxed workload needs no DNS lookup)
/// and as the lockdown's sole allow-listed destination.
pub(crate) async fn docker_network_gateway(
    runtime: &str,
    network: &str,
) -> Result<String, SandboxError> {
    let out = Command::new(runtime)
        .args([
            "network",
            "inspect",
            network,
            "--format",
            "{{ (index .IPAM.Config 0).Gateway }}",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .map_err(|e| SandboxError::Internal(format!("docker network inspect: {e}")))?;
    if !out.status.success() {
        return Err(SandboxError::Internal(format!(
            "docker network inspect `{network}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let gateway = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if gateway.is_empty() {
        return Err(SandboxError::Internal(format!(
            "docker network `{network}` reported no gateway"
        )));
    }
    Ok(gateway)
}

/// The host PID of a running container's main process — the target `nsenter`
/// attaches to in order to reach its network namespace.
pub(crate) async fn container_pid(runtime: &str, container_id: &str) -> Result<u32, SandboxError> {
    let out = Command::new(runtime)
        .args(["inspect", "--format", "{{.State.Pid}}", container_id])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .map_err(|e| SandboxError::Internal(format!("docker inspect: {e}")))?;
    if !out.status.success() {
        return Err(SandboxError::Internal(format!(
            "docker inspect `{container_id}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    raw.parse().map_err(|_| {
        SandboxError::Internal(format!(
            "docker inspect returned a non-numeric pid: {raw:?}"
        ))
    })
}

/// Apply the lockdown: default-deny `OUTPUT`, then allow only loopback and
/// `gateway:proxy_port` (the egress proxy). Each `iptables` invocation runs as a
/// host process attached to `pid`'s network namespace via `nsenter --net`, so the
/// container process itself is never granted the capability to modify these
/// rules. Fails closed on the first rule that can't be applied (e.g. `nsenter`/
/// `iptables` missing on the host, or insufficient privilege).
pub(crate) async fn lock_down_egress(
    pid: u32,
    gateway: &str,
    proxy_port: u16,
) -> Result<(), SandboxError> {
    let pid = pid.to_string();
    let port = proxy_port.to_string();
    // Order is immaterial to security here — the container is still running its
    // inert placeholder at this point, not the untrusted command, so there is no
    // window to exploit regardless of rule sequencing. Punch the ACCEPT holes
    // first purely for conventional clarity, then flip the default policy.
    let rules: Vec<Vec<&str>> = vec![
        vec!["-A", "OUTPUT", "-o", "lo", "-j", "ACCEPT"],
        vec![
            "-A", "OUTPUT", "-d", gateway, "-p", "tcp", "--dport", &port, "-j", "ACCEPT",
        ],
        // Default-deny last: everything not already matched above is dropped.
        vec!["-P", "OUTPUT", "DROP"],
    ];
    for rule in &rules {
        let mut args: Vec<&str> = vec!["--target", &pid, "--net", "--", "iptables"];
        args.extend(rule.iter().copied());
        let out = Command::new("nsenter")
            .args(&args)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| SandboxError::Internal(format!("nsenter/iptables: {e}")))?;
        if !out.status.success() {
            return Err(SandboxError::Internal(format!(
                "egress lockdown rule {:?} failed: {}",
                rule,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
    }
    Ok(())
}
