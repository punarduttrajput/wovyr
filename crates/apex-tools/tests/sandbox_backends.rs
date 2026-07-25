//! Integration tests for the container/gVisor sandbox backends.
//!
//! These run real `docker`/`runsc` and so are **capability-gated**: each test
//! probes the host with [`SandboxManager::detect`] and returns early (logging a
//! skip) when the backend is unavailable, so the suite still passes on offline CI
//! nodes without a container runtime. The deterministic argv/config construction is
//! covered by the unit tests in `src/sandbox.rs`; this file verifies that the
//! constructed commands actually isolate and enforce.
//!
//! **Only compiled with `--features sandbox-integration-tests`** (RM-AR-P1
//! QA-401). Runtime capability-gating alone isn't enough: a plain `cargo test
//! --workspace` on a machine that genuinely *has* Docker (any contributor's
//! machine, or the ordinary `rust` CI job on a stock `ubuntu-latest` runner)
//! still exercises real container networking/cgroups/iptables behavior this
//! workspace doesn't control there — 3 of these tests failed for real in exactly
//! that job, unrelated to any change in this crate, silently turning the whole
//! job red despite fmt/clippy/build all passing. CI's dedicated
//! `sandbox-integration` job (which installs a real gVisor runtime) is the one
//! place this file is built and run.

#![cfg(feature = "sandbox-integration-tests")]

use apex_tools::{
    CommandOutcome, ContainerSandbox, FirecrackerConfig, FirecrackerSandbox, NetworkPolicy,
    ResourceLimits, Sandbox, SandboxBackend, SandboxCommand, SandboxManager, SandboxPool,
};
use std::time::Duration;

const IMAGE: &str = "alpine:latest";

fn cmd(program: &str, args: &[&str]) -> SandboxCommand {
    SandboxCommand {
        program: program.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        workdir: ".".into(),
        env: vec![],
        limits: ResourceLimits {
            timeout: Duration::from_secs(60),
            ..ResourceLimits::default()
        },
    }
}

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

async fn run(sb: &impl Sandbox, c: &SandboxCommand) -> CommandOutcome {
    sb.execute(c).await.expect("sandbox execution failed")
}

#[tokio::test]
async fn container_runs_command_and_captures_output() {
    if !has(SandboxBackend::Container).await {
        return;
    }
    let sb = ContainerSandbox::docker(IMAGE);
    let out = run(&sb, &cmd("echo", &["apex_container_ok"])).await;
    assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("apex_container_ok"));
}

#[tokio::test]
async fn container_denies_egress_by_default() {
    if !has(SandboxBackend::Container).await {
        return;
    }
    // Deny-all policy → `--network none`; an outbound connection must fail.
    let sb = ContainerSandbox::docker(IMAGE);
    let out = run(
        &sb,
        &cmd(
            "sh",
            &["-c", "wget -T 3 -q -O- http://example.com; echo EXIT=$?"],
        ),
    )
    .await;
    assert!(
        !out.stdout.contains("EXIT=0"),
        "egress should be blocked by --network none, got stdout: {:?} stderr: {:?}",
        out.stdout,
        out.stderr
    );
}

#[tokio::test]
async fn container_allows_egress_when_policy_permits() {
    if !has(SandboxBackend::Container).await {
        return;
    }
    // A non-deny policy → bridged networking; loopback/DNS within the namespace
    // works even if outbound internet is unavailable on the CI node, so we only
    // assert the interface is up rather than reaching the internet.
    let sb = ContainerSandbox::docker(IMAGE).with_network(NetworkPolicy {
        default_deny: false,
        outbound_allow: vec![],
    });
    let out = run(&sb, &cmd("sh", &["-c", "ip link show lo"])).await;
    assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("lo"));
}

#[tokio::test]
async fn container_enforces_memory_limit() {
    if !has(SandboxBackend::Container).await {
        return;
    }
    // A 16 MiB cgroup memory cap OOM-kills a process that grows without bound.
    let mut c = cmd(
        "sh",
        &["-c", "a=0; while :; do a=\"$a$a$a$a$a$a$a$a$a\"; done"],
    );
    c.limits.memory_bytes = Some(16 * 1024 * 1024);
    c.limits.timeout = Duration::from_secs(30);
    let sb = ContainerSandbox::docker(IMAGE);
    let out = run(&sb, &c).await;
    assert_ne!(
        out.exit_code,
        Some(0),
        "process should be OOM-killed under the memory cap, stderr: {}",
        out.stderr
    );
}

#[tokio::test]
async fn container_pids_limit_contains_a_fork_bomb() {
    if !has(SandboxBackend::Container).await {
        return;
    }
    // A `--pids-limit` cap (cgroup `pids.max`) must survive an adversarial attempt
    // to grab far more processes than granted: of 40 attempted background jobs,
    // only a handful may actually hold a live pid inside the container.
    let mut c = cmd(
        "sh",
        &[
            "-c",
            "for i in $(seq 1 40); do sleep 10 & done 2>/dev/null; sleep 1; ls /proc | grep -Ec '^[0-9]+$'",
        ],
    );
    c.limits.max_pids = Some(8);
    c.limits.timeout = Duration::from_secs(15);
    let sb = ContainerSandbox::docker(IMAGE);
    let out = run(&sb, &c).await;

    assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
    let alive: u32 = out.stdout.trim().parse().unwrap_or(u32::MAX);
    assert!(
        alive < 40,
        "a pids limit of 8 must keep the fork bomb far below the 40 attempted forks, got {alive} alive; stdout: {:?} stderr: {:?}",
        out.stdout,
        out.stderr
    );
}

#[tokio::test]
async fn gvisor_runs_under_runsc_kernel() {
    if !has(SandboxBackend::Gvisor).await {
        return;
    }
    let sb = ContainerSandbox::gvisor(IMAGE);
    // gVisor's sentry reports itself in the guest dmesg ring buffer.
    let out = run(&sb, &cmd("dmesg", &[])).await;
    assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("gVisor"),
        "expected gVisor sentry banner in dmesg, got: {}",
        out.stdout
    );
}

#[tokio::test]
async fn gvisor_denies_privileged_mount_syscall() {
    if !has(SandboxBackend::Gvisor).await {
        return;
    }
    // A compromised guest process attempting to mount a filesystem is a classic
    // container-escape primitive (remounting `/` writable, staging a bind-mount
    // pivot, etc.). gVisor's sentry intercepts `mount` in its own user-space
    // kernel rather than passing it to the host, and denies it by default.
    let sb = ContainerSandbox::gvisor(IMAGE);
    let out = run(
        &sb,
        &cmd("sh", &["-c", "mount -t tmpfs tmpfs /mnt; echo RC=$?"]),
    )
    .await;
    assert!(
        out.stdout.contains("RC=") && !out.stdout.contains("RC=0"),
        "an in-guest mount attempt must be denied under gVisor, got stdout: {:?} stderr: {:?}",
        out.stdout,
        out.stderr
    );
}

#[tokio::test]
async fn gvisor_denies_reading_host_physical_memory_via_proc_kcore() {
    if !has(SandboxBackend::Gvisor).await {
        return;
    }
    // `/proc/kcore` exposes a process's view of physical memory — a known
    // container-escape / info-leak vector if a strong backend's synthetic procfs
    // ever forwarded it to the real host. gVisor's own procfs implementation
    // must not expose it (or must expose empty/inaccessible content).
    let sb = ContainerSandbox::gvisor(IMAGE);
    let out = run(
        &sb,
        &cmd(
            "sh",
            &[
                "-c",
                "dd if=/proc/kcore of=/dev/null bs=1 count=64 2>&1; echo RC=$?",
            ],
        ),
    )
    .await;
    assert!(
        !out.stdout.contains("RC=0"),
        "reading /proc/kcore must not succeed under gVisor, got stdout: {:?} stderr: {:?}",
        out.stdout,
        out.stderr
    );
}

#[tokio::test]
async fn egress_proxy_denies_non_allowlisted_host_from_container() {
    if !has(SandboxBackend::Container).await {
        return;
    }
    // Default-deny with a single allow-listed host → the sandbox starts an egress
    // proxy and routes the container through it via HTTPS_PROXY.
    let policy = NetworkPolicy {
        default_deny: true,
        outbound_allow: vec!["example.com".to_string()],
    };
    let sb = ContainerSandbox::docker(IMAGE).with_network(policy);

    // Inside the container: extract the proxy host:port from HTTPS_PROXY (a literal
    // gateway IP under the egress-lockdown flow, not a hostname) and CONNECT to a
    // host that is NOT allow-listed; the proxy must refuse it with 403 (no TLS needed).
    let script = r#"p=${HTTPS_PROXY#http://}; h=${p%%:*}; port=${p##*:}; printf 'CONNECT 10.255.255.1:443 HTTP/1.1\r\n\r\n' | nc -w 3 "$h" "$port" | head -1"#;
    let out = run(&sb, &cmd("sh", &["-c", script])).await;
    assert!(
        out.stdout.contains("403"),
        "proxy should deny a non-allow-listed host; stdout: {:?} stderr: {:?}",
        out.stdout,
        out.stderr
    );
}

#[tokio::test]
async fn container_egress_lockdown_blocks_direct_bypass_of_the_proxy() {
    if !has(SandboxBackend::Container).await {
        return;
    }
    // The core L3-bypass fix: a workload that simply ignores `HTTPS_PROXY` and
    // dials out directly must still be blocked. Previously this only had
    // `--network bridge` underneath it — fully open — so a direct connection
    // reached the internet just fine. Now the host applies an `iptables`
    // default-deny to the container's own `OUTPUT` chain (via `nsenter`, before
    // the real command ever runs), so nothing but loopback and the proxy's
    // address is reachable at all, regardless of whether the workload cooperates
    // with the proxy env vars.
    let policy = NetworkPolicy {
        default_deny: true,
        outbound_allow: vec!["example.com".to_string()],
    };
    let sb = ContainerSandbox::docker(IMAGE).with_network(policy);

    let out = run(
        &sb,
        &cmd("sh", &["-c", "nc -w 3 -z 1.1.1.1 443; echo EXIT=$?"]),
    )
    .await;
    assert!(
        !out.stdout.contains("EXIT=0"),
        "a direct connection bypassing HTTPS_PROXY must be blocked by the host-side \
         egress lockdown, got stdout: {:?} stderr: {:?}",
        out.stdout,
        out.stderr
    );
}

#[tokio::test]
async fn firecracker_microvm_runs_command_and_returns_output() {
    // Needs KVM + the firecracker binary (capability) and a guest kernel + rootfs
    // (carrying the apex guest agent as /init), supplied via env.
    if !has(SandboxBackend::Firecracker).await {
        return;
    }
    let (Ok(kernel), Ok(rootfs)) = (
        std::env::var("APEX_FC_KERNEL"),
        std::env::var("APEX_FC_ROOTFS"),
    ) else {
        eprintln!("skipping: APEX_FC_KERNEL / APEX_FC_ROOTFS not set");
        return;
    };

    let limits = ResourceLimits {
        timeout: Duration::from_secs(30),
        ..ResourceLimits::default()
    };
    let config = FirecrackerConfig::from_limits(&kernel, &rootfs, &limits);
    let sb = FirecrackerSandbox::with_config(config);

    let mut c = cmd(
        "sh",
        &["-c", "echo apex_microvm_ok; cat /etc/alpine-release"],
    );
    c.limits.timeout = Duration::from_secs(30);
    let out = sb.execute(&c).await.expect("microVM execution");

    assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("apex_microvm_ok"),
        "expected command output from inside the guest, got: {:?}",
        out.stdout
    );
}

#[tokio::test]
async fn firecracker_memory_ceiling_contains_a_guest_oom() {
    // Needs KVM + the firecracker binary (capability) and a guest kernel + rootfs,
    // supplied via env — see `firecracker_microvm_runs_command_and_returns_output`.
    if !has(SandboxBackend::Firecracker).await {
        return;
    }
    let (Ok(kernel), Ok(rootfs)) = (
        std::env::var("APEX_FC_KERNEL"),
        std::env::var("APEX_FC_ROOTFS"),
    ) else {
        eprintln!("skipping: APEX_FC_KERNEL / APEX_FC_ROOTFS not set");
        return;
    };

    // A microVM's memory ceiling (`mem_size_mib`, hardware-virtualized — the guest
    // simply has no more RAM to allocate) must contain a runaway process the same
    // way the container backend's cgroup does: the guest's own OOM killer should
    // intervene well before the wall-clock timeout, rather than the VM hanging or
    // (worse) the host feeling any memory pressure from it.
    let limits = ResourceLimits {
        timeout: Duration::from_secs(30),
        memory_bytes: Some(128 * 1024 * 1024),
        ..ResourceLimits::default()
    };
    let config = FirecrackerConfig::from_limits(&kernel, &rootfs, &limits);
    let sb = FirecrackerSandbox::with_config(config);

    let mut c = cmd(
        "sh",
        &["-c", "a=x; while :; do a=\"$a$a$a$a$a$a$a$a$a\"; done"],
    );
    c.limits.timeout = Duration::from_secs(30);
    let out = sb.execute(&c).await.expect("microVM execution");

    assert!(
        !out.timed_out,
        "an unbounded memory grab in a 128 MiB microVM should be OOM-killed well before the wall-clock timeout"
    );
    assert_ne!(
        out.exit_code,
        Some(0),
        "the OOM-killed process must not report success, stderr: {}",
        out.stderr
    );
}

#[tokio::test]
async fn warm_pool_executes_and_reuses_container_sandboxes() {
    if !has(SandboxBackend::Container).await {
        return;
    }
    // A pool of pre-warmed container sandboxes; checkouts reuse warm instances.
    let pool = SandboxPool::new(
        2,
        2,
        Box::new(|| Box::new(ContainerSandbox::docker(IMAGE)) as Box<dyn Sandbox>),
    );
    assert_eq!(pool.idle(), 2, "two container sandboxes pre-warmed");

    {
        let sb = pool.acquire().await.expect("acquire");
        assert_eq!(sb.backend(), SandboxBackend::Container);
        let out = sb
            .execute(&cmd("echo", &["pooled_ok"]))
            .await
            .expect("exec");
        assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
        assert!(out.stdout.contains("pooled_ok"));
    }
    // Returned to the pool and reused on the next checkout (no fresh construction).
    assert_eq!(pool.idle(), 2);
    let _sb = pool.acquire().await.expect("reacquire");
    assert_eq!(pool.reused(), 2, "both checkouts reused warm instances");
    assert_eq!(pool.created(), 2, "no extra sandboxes built");
}

// --- SBX-101: the shell tool selects a strong backend on the run path -------

#[tokio::test]
async fn shell_tool_runs_a_verified_run_in_a_container_not_native() {
    use apex_tools::{ShellTool, Tool, ToolContext, ToolRequest, TrustClass};

    if !has(SandboxBackend::Container).await {
        return;
    }
    // A node that has probed its capabilities (Container present here). The shell tool
    // resolves the backend from the run's trust class: `Verified` floors to Container,
    // so a verified run must execute inside the container, never on the native host.
    let manager = SandboxManager::detect().await;
    let shell = ShellTool::with_manager(manager).with_image(IMAGE);
    let ctx = ToolContext {
        trust_class: TrustClass::Verified,
        ..ToolContext::default()
    };
    // `/etc/alpine-release` exists only inside the alpine image, not on the host CI
    // node — so a zero exit proves the command ran in the container, not natively.
    let resp = shell
        .execute(
            &ctx,
            ToolRequest::new(serde_json::json!({ "command": "cat /etc/alpine-release" })),
        )
        .await
        .expect("shell execute");
    assert!(
        resp.success,
        "a verified shell run must succeed inside the alpine container (proving it ran \
         there, not on the host): {:?}",
        resp.payload
    );
    assert!(
        resp.payload["stdout"]
            .as_str()
            .is_some_and(|s| s.contains('.')),
        "expected an alpine version string from inside the container: {:?}",
        resp.payload
    );
}

#[tokio::test]
async fn shell_tool_first_party_run_stays_native_even_when_containers_exist() {
    use apex_tools::{ShellTool, Tool, ToolContext, ToolRequest, TrustClass};

    if !has(SandboxBackend::Container).await {
        return;
    }
    // Even on a container-capable node, a first-party run uses the native host shell
    // (the strongest requirement is only Native), so a host-only command succeeds.
    // `with_unsandboxed_native_ack(true)` (SEC-404): this test is about *backend
    // selection* (native vs. container), not about the native confinement floor —
    // that's covered by its own dedicated tests in `builtin.rs`/`native.rs`. Without
    // the acknowledgement, a `with_manager()`-constructed `ShellTool` now fails
    // closed on a host with no netns egress floor, which would make this
    // selection test fail for an unrelated reason on such a host.
    let manager = SandboxManager::detect().await;
    let shell = ShellTool::with_manager(manager)
        .with_image(IMAGE)
        .with_unsandboxed_native_ack(true);
    let ctx = ToolContext {
        trust_class: TrustClass::FirstParty,
        ..ToolContext::default()
    };
    // `/etc/alpine-release` does NOT exist on the (non-alpine) host, so a first-party
    // run of this command fails — the opposite of the verified/container case above,
    // confirming it did not run in the alpine container.
    let resp = shell
        .execute(
            &ctx,
            ToolRequest::new(serde_json::json!({ "command": "cat /etc/alpine-release" })),
        )
        .await
        .expect("shell execute");
    assert!(
        !resp.success,
        "a first-party run executes on the host (no /etc/alpine-release), not in the \
         alpine container: {:?}",
        resp.payload
    );
}

// --- Adversarial filesystem-escape tests -----------------------------------
//
// The container backend bind-mounts exactly one host directory (`cmd.workdir`) at
// `/workspace` and makes the whole rootfs `--read-only` otherwise. These tests
// attempt the two obvious escapes an untrusted tool might try: writing somewhere
// outside the mount, and reading a sibling host directory that was never granted.

/// A fresh host scratch directory under the OS temp dir, unique per test run.
fn scratch_dir(name: &str) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "apex-fs-escape-{name}-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn cmd_in(workdir: &std::path::Path, program: &str, args: &[&str]) -> SandboxCommand {
    let mut c = cmd(program, args);
    c.workdir = workdir.to_string_lossy().into_owned();
    c
}

#[tokio::test]
async fn container_read_only_rootfs_denies_writes_outside_workspace() {
    if !has(SandboxBackend::Container).await {
        return;
    }
    let base = scratch_dir("readonly");
    let workdir = base.join("workdir");
    std::fs::create_dir_all(&workdir).expect("create workdir");

    let sb = ContainerSandbox::docker(IMAGE);
    // The rootfs outside the bind mount is `--read-only`; only `/workspace` and the
    // `tmpfs` `/tmp` are writable. A tool trying to tamper with the image itself
    // (e.g. planting a backdoor in `/etc` or `/bin`) must be denied.
    let out = run(
        &sb,
        &cmd_in(
            &workdir,
            "sh",
            &["-c", "echo pwned > /etc/apex_pwn_test; echo RC=$?"],
        ),
    )
    .await;

    assert!(
        out.stdout.contains("RC=") && !out.stdout.contains("RC=0"),
        "write outside /workspace must fail on the read-only rootfs, got stdout: {:?} stderr: {:?}",
        out.stdout,
        out.stderr
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn container_workspace_mount_does_not_expose_host_sibling_directory() {
    if !has(SandboxBackend::Container).await {
        return;
    }
    let base = scratch_dir("sibling");
    let workdir = base.join("workdir");
    let secret_dir = base.join("secret");
    std::fs::create_dir_all(&workdir).expect("create workdir");
    std::fs::create_dir_all(&secret_dir).expect("create secret dir");
    let marker = "host-sibling-marker-2b6f9a";
    std::fs::write(secret_dir.join("marker.txt"), marker).expect("write marker");

    let sb = ContainerSandbox::docker(IMAGE);
    // Only `workdir` is bind-mounted at `/workspace`; its host sibling `secret/` was
    // never granted. Traversing `..` from `/workspace` must land inside the
    // container's own (empty) rootfs, never back out onto the host.
    let out = run(
        &sb,
        &cmd_in(
            &workdir,
            "sh",
            &["-c", "cat ../secret/marker.txt 2>&1; echo RC=$?"],
        ),
    )
    .await;

    assert!(
        !out.stdout.contains(marker),
        "a sibling host directory outside the bind mount must not be readable via `..`, got: {:?}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("RC=0"),
        "the traversal read must fail, got: {:?}",
        out.stdout
    );

    let _ = std::fs::remove_dir_all(&base);
}
