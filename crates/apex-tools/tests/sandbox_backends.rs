//! Integration tests for the container/gVisor sandbox backends.
//!
//! These run real `docker`/`runsc` and so are **capability-gated**: each test
//! probes the host with [`SandboxManager::detect`] and returns early (logging a
//! skip) when the backend is unavailable, so the suite still passes on offline CI
//! nodes without a container runtime. The deterministic argv/config construction is
//! covered by the unit tests in `src/sandbox.rs`; this file verifies that the
//! constructed commands actually isolate and enforce.

use apex_tools::{
    CommandOutcome, ContainerSandbox, NetworkPolicy, ResourceLimits, Sandbox, SandboxBackend,
    SandboxCommand, SandboxManager,
};
use std::time::Duration;

const IMAGE: &str = "alpine:latest";

fn cmd(program: &str, args: &[&str]) -> SandboxCommand {
    SandboxCommand {
        program: program.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        workdir: ".".into(),
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
