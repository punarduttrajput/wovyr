//! QA-403: an automated, reproducible replacement for the one-time "verified
//! live" manual check (CLAUDE.md's wovyr-server bullet: "a CLI-sealed memory
//! record was read back through a separately-running server") that cross-process
//! CLI↔server KMS sharing actually works.
//!
//! Spawns the real `wovyr` binary twice against a shared scratch `HOME` (so both
//! processes resolve the identical `~/.wovyr/kms` root key + `~/.wovyr/memory`
//! store, exactly as two real processes on an operator's machine would): once to
//! `memory put --sensitive` (the CLI's local path, sealing content through a
//! freshly generate-on-first-use KMS root key), once as `wovyr dev` (the embedded
//! server, reading that same root key back). The record is then read back over
//! the server's HTTP API — a genuinely different process than the one that
//! sealed it — and the plaintext content must round-trip.
//!
//! **Not capability-gated on external infrastructure** — only the already-built
//! `wovyr` binary (via `CARGO_BIN_EXE_wovyr`, Cargo's standard mechanism for a
//! binary-only crate's own integration tests, which have no lib target to `use`)
//! and a free local TCP port — so this runs unconditionally, offline, in CI.

use std::net::TcpListener;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};

/// A scratch `HOME`/`USERPROFILE` directory unique to this test run, so
/// concurrent runs (and the developer's own real `~/.wovyr`) never collide.
fn scratch_home() -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "wovyr_cli_cross_process_kms_{}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An unused local TCP port (bind to port 0 and read it back) — avoids a fixed
/// port colliding with another test, CI job, or a developer's own running
/// `wovyr dev`.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Kills the spawned `wovyr dev` server on drop, so a test failure (panic before
/// reaching the explicit teardown) never leaves an orphaned server process.
struct ServerGuard(Child);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

#[tokio::test]
async fn cli_sealed_memory_record_is_readable_through_a_separately_started_server() {
    let home = scratch_home();
    let port = free_port();
    let bin = env!("CARGO_BIN_EXE_wovyr");
    const MARKER: &str = "cross-process-kms-roundtrip-marker";
    const NAMESPACE: &str = "cross-process-test";
    const ADMIN_PRINCIPAL: &str = "cross-process-test-admin";

    // 1. Seal a sensitive memory record via the CLI — a first-run KMS root key is
    // generated and persisted under the scratch HOME's ~/.wovyr/kms.
    let put = Command::new(bin)
        .args([
            "memory",
            "put",
            "--namespace",
            NAMESPACE,
            "--content",
            MARKER,
            "--sensitive",
        ])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .await
        .expect("spawn `wovyr memory put`");
    assert!(
        put.status.success(),
        "`wovyr memory put --sensitive` failed: stdout={} stderr={}",
        String::from_utf8_lossy(&put.stdout),
        String::from_utf8_lossy(&put.stderr)
    );

    // 2. Start a *separate* `wovyr dev` server against the SAME scratch HOME, so it
    // resolves the identical root key + memory store the CLI process just wrote.
    // `WOVYR_ALLOW_ANONYMOUS=1` (loopback-only bind, so this is safe) +
    // `WOVYR_PLATFORM_ADMINS` grants the test's principal the `memory:read` scope
    // the records route requires — the same pattern CI's `contract-gate` job uses.
    let mut server = ServerGuard(
        Command::new(bin)
            .args(["dev", "--addr", &format!("127.0.0.1:{port}")])
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("WOVYR_ALLOW_ANONYMOUS", "1")
            .env("WOVYR_PLATFORM_ADMINS", ADMIN_PRINCIPAL)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn `wovyr dev`"),
    );

    // 3. Wait for the server to become healthy.
    let client = reqwest::Client::new();
    let healthz = format!("http://127.0.0.1:{port}/healthz");
    let mut healthy = false;
    for _ in 0..60 {
        if client
            .get(&healthz)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            healthy = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        healthy,
        "wovyr dev server never became healthy on 127.0.0.1:{port}"
    );

    // 4. Read the record back through the server's HTTP API — a genuinely
    // different process than the one that sealed it — and assert the plaintext
    // content round-trips. `EncryptingMemoryStore` transparently unseals on every
    // read path, so a correct cross-process KMS share means this plaintext marker
    // comes back exactly as written.
    let records_url =
        format!("http://127.0.0.1:{port}/api/v1/memory/records?namespace={NAMESPACE}");
    let resp: serde_json::Value = client
        .get(&records_url)
        .header("X-Wovyr-Tenant", "default")
        .header("X-Wovyr-Principal", ADMIN_PRINCIPAL)
        .send()
        .await
        .expect("query memory records")
        .json()
        .await
        .expect("parse memory records response");

    let items = resp["data"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a `data` array in the response, got: {resp:#?}"));
    assert!(
        items.iter().any(|r| r["content"].as_str() == Some(MARKER)),
        "the CLI-sealed record must be readable, in plaintext, through the \
         separately-started server (cross-process KMS sharing): {resp:#?}"
    );

    // Explicit, ordered teardown before dropping the guard (avoids a panic on
    // an already-killed handle); the Drop impl above is the safety net for an
    // assertion failure above this point.
    let _ = server.0.start_kill();
    let _ = server.0.wait().await;
    let _ = std::fs::remove_dir_all(&home);
}
