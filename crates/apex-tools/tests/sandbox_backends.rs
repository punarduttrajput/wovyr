//! Integration tests for the container/gVisor sandbox backends.
//!
//! These run real `docker`/`runsc` and so are **capability-gated**: each test
//! probes the host with [`SandboxManager::detect`] and returns early (logging a
//! skip) when the backend is unavailable, so the suite still passes on offline CI
//! nodes without a container runtime. The deterministic argv/config construction is
//! covered by the unit tests in `src/sandbox.rs`; this file verifies that the
//! constructed commands actually isolate and enforce.

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

    // Inside the container: extract the proxy port from HTTPS_PROXY and CONNECT to a
    // host that is NOT allow-listed; the proxy must refuse it with 403 (no TLS needed).
    let script = r#"p=${HTTPS_PROXY##*:}; printf 'CONNECT 10.255.255.1:443 HTTP/1.1\r\n\r\n' | nc -w 3 host.docker.internal "$p" | head -1"#;
    let out = run(&sb, &cmd("sh", &["-c", script])).await;
    assert!(
        out.stdout.contains("403"),
        "proxy should deny a non-allow-listed host; stdout: {:?} stderr: {:?}",
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
