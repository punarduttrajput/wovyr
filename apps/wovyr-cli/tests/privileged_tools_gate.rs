//! SBX-305: `--local` runs must not hand the privileged builtins (`shell`,
//! `fs_write`, `code_execute`) to a model without an explicit per-run opt-in.
//!
//! Before this gate, *choosing `--local`* was itself treated as the operator's
//! acknowledgement, so `wovyr agents run --local` on a manifest listing `shell`
//! executed arbitrary host commands with nothing but a WARN line in the way. The
//! 2026-07-27 internal red-team run demonstrated a real model reading an arbitrary
//! host file outside the run's workdir through exactly that path.
//!
//! Drives the **real `wovyr` binary** (via `CARGO_BIN_EXE_wovyr` — this is a
//! binary-only crate with no lib target to `use`) against a scratch `HOME`, the same
//! approach `cross_process_kms.rs` uses, so this tests the shipped CLI surface rather
//! than an in-process reimplementation of it. Runs unconditionally, offline: with no
//! API key set, the agent loop resolves the deterministic mock provider.

use std::process::Output;
use tokio::process::Command;

/// A scratch `HOME`/`USERPROFILE` unique to this test run, so a `--local` run never
/// touches the developer's real `~/.wovyr` (or a concurrent test's).
fn scratch_home(label: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "wovyr_cli_privileged_gate_{label}_{}_{}_{}",
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

/// The error text the gate must produce — asserted on rather than merely "it failed",
/// so an unrelated failure (a missing file, a provider error) can't pass this test.
const GATE_MESSAGE: &str = "privileged tools not enabled";

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `wovyr agents run --local` on a manifest declaring `shell`, with no opt-in.
#[tokio::test]
async fn agents_run_local_fails_closed_on_a_shell_manifest_without_the_opt_in() {
    let home = scratch_home("agents_denied");
    let out = Command::new(env!("CARGO_BIN_EXE_wovyr"))
        .args([
            "agents",
            "run",
            "--local",
            "-f",
            "../../examples/agents/shell-runner.yaml",
            "--input",
            "{\"message\":\"hi\"}",
        ])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        // Ensure a stray env var in the developer's shell can't silently enable the
        // very thing this test asserts is denied.
        .env_remove("WOVYR_LOCAL_PRIVILEGED")
        .output()
        .await
        .expect("spawn `wovyr agents run --local`");

    let text = combined(&out);
    assert!(
        !out.status.success(),
        "a manifest declaring `shell` must fail closed without --allow-privileged-tools, \
         but the run succeeded: {text}"
    );
    assert!(
        text.contains(GATE_MESSAGE),
        "the failure must name the gate (not read as a typo'd tool id): {text}"
    );
    assert!(
        text.contains("--allow-privileged-tools"),
        "the error must tell the operator which flag enables it: {text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The same manifest *with* the opt-in must get past the gate. Deliberately asserts
/// only "not blocked by SBX-305" rather than overall success: with the mock provider
/// the agent's answer is deterministic but says nothing about tool availability, and
/// pinning that would make this test fail for reasons unrelated to the gate.
#[tokio::test]
async fn agents_run_local_passes_the_gate_with_the_opt_in() {
    let home = scratch_home("agents_allowed");
    let out = Command::new(env!("CARGO_BIN_EXE_wovyr"))
        .args([
            "agents",
            "run",
            "--local",
            "--allow-privileged-tools",
            "-f",
            "../../examples/agents/shell-runner.yaml",
            "--input",
            "{\"message\":\"hi\"}",
        ])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .await
        .expect("spawn `wovyr agents run --local --allow-privileged-tools`");

    let text = combined(&out);
    assert!(
        !text.contains(GATE_MESSAGE),
        "--allow-privileged-tools must satisfy the gate: {text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// `WOVYR_LOCAL_PRIVILEGED=1` is the session-wide equivalent of the flag — it is what
/// the `workflows approve`/`signal`/`tick` resume paths honor, since they take no flag
/// of their own, so it must be proven to work and not just documented.
#[tokio::test]
async fn agents_run_local_passes_the_gate_with_the_env_var() {
    let home = scratch_home("agents_env");
    let out = Command::new(env!("CARGO_BIN_EXE_wovyr"))
        .args([
            "agents",
            "run",
            "--local",
            "-f",
            "../../examples/agents/shell-runner.yaml",
            "--input",
            "{\"message\":\"hi\"}",
        ])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("WOVYR_LOCAL_PRIVILEGED", "1")
        .output()
        .await
        .expect("spawn `wovyr agents run --local` with WOVYR_LOCAL_PRIVILEGED=1");

    let text = combined(&out);
    assert!(
        !text.contains(GATE_MESSAGE),
        "WOVYR_LOCAL_PRIVILEGED=1 must satisfy the gate: {text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A manifest using only the safe builtins must be entirely unaffected — the gate
/// must not become a blanket "no tools locally" regression.
#[tokio::test]
async fn agents_run_local_is_unaffected_for_a_non_privileged_manifest() {
    let home = scratch_home("agents_safe");
    let out = Command::new(env!("CARGO_BIN_EXE_wovyr"))
        .args([
            "agents",
            "run",
            "--local",
            "-f",
            "../../examples/agents/hello.yaml",
            "--input",
            "{\"message\":\"hi\"}",
        ])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env_remove("WOVYR_LOCAL_PRIVILEGED")
        .output()
        .await
        .expect("spawn `wovyr agents run --local` on hello.yaml");

    let text = combined(&out);
    assert!(
        out.status.success(),
        "a manifest with no privileged tools must still run: {text}"
    );
    assert!(
        !text.contains(GATE_MESSAGE),
        "the gate must not fire for a non-privileged manifest: {text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// `wovyr workflows run --local` on a definition whose `for_each` *body* names
/// `shell`. The parent activity's own type is `for_each`, so a gate that only looked
/// at top-level `type: tool` activities would miss this entirely — which is the shape
/// a fan-out abuse case would actually take.
#[tokio::test]
async fn workflows_run_local_fails_closed_on_a_shell_inside_a_for_each_body() {
    let home = scratch_home("wf_denied");
    let def = home.join("shell-fanout.yaml");
    std::fs::write(
        &def,
        r#"apiVersion: workflow.wovyr.io/v1
kind: Workflow
metadata:
  name: shell-fanout
  version: "1.0.0"
spec:
  activities:
    - id: fan
      type: for_each
      inputs:
        items: ["a", "b"]
        activity:
          type: tool
          name: shell
          inputs:
            command: echo hi
"#,
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_wovyr"))
        .args([
            "workflows",
            "run",
            "--local",
            "-f",
            &def.to_string_lossy(),
            "--input",
            "{}",
        ])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env_remove("WOVYR_LOCAL_PRIVILEGED")
        .output()
        .await
        .expect("spawn `wovyr workflows run --local`");

    let text = combined(&out);
    assert!(
        !out.status.success(),
        "a for_each body naming `shell` must fail closed without the opt-in: {text}"
    );
    assert!(
        text.contains(GATE_MESSAGE),
        "the failure must name the gate: {text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The workflow path's opt-in must work too (the mirror of the agents case above).
#[tokio::test]
async fn workflows_run_local_passes_the_gate_with_the_opt_in() {
    let home = scratch_home("wf_allowed");
    let def = home.join("shell-direct.yaml");
    std::fs::write(
        &def,
        r#"apiVersion: workflow.wovyr.io/v1
kind: Workflow
metadata:
  name: shell-direct
  version: "1.0.0"
spec:
  activities:
    - id: run
      type: tool
      name: shell
      inputs:
        command: echo hi
"#,
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_wovyr"))
        .args([
            "workflows",
            "run",
            "--local",
            "--allow-privileged-tools",
            "-f",
            &def.to_string_lossy(),
            "--input",
            "{}",
        ])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .await
        .expect("spawn `wovyr workflows run --local --allow-privileged-tools`");

    let text = combined(&out);
    assert!(
        !text.contains(GATE_MESSAGE),
        "--allow-privileged-tools must satisfy the gate on the workflow path: {text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
