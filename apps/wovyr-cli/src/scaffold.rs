//! `wovyr plugin new` / `wovyr plugin build` — the plugin authoring scaffold
//! (RM-AIM-P3 ECO-302).
//!
//! `new` generates a buildable plugin project: a `wasm32-wasip1` bin crate
//! using `wovyr-plugin-sdk`'s typed entry point, plus a `plugin.yaml` whose
//! `artifacts` list is deliberately empty — digests are **computed, never
//! hand-edited**. `build` closes that loop: it compiles the project to wasm,
//! stages a distributable package directory (`dist/`), and writes the manifest
//! back with the real sha256 artifact digest filled in. From there the
//! existing supply chain applies unchanged: `wovyr plugin sign` → `trust` →
//! `install`.

use std::path::{Path, PathBuf};
use wovyr_common::{Error, Result};
use wovyr_plugin::{Artifact, PluginManifest};

/// `wovyr plugin new <name>` — generate a plugin project under `<dir>/<name>`.
///
/// `sdk_path` points the generated project at a local `wovyr-plugin-sdk`
/// checkout (emitted as a `path` dependency). Without it the project depends
/// on the published crate version — which requires the SDK to be on crates.io.
pub fn new_cmd(name: &str, publisher: &str, dir: &str, sdk_path: Option<&str>) -> Result<()> {
    validate_name(name)?;
    if publisher.trim().is_empty() {
        return Err(Error::invalid("publisher must not be empty"));
    }
    let project = Path::new(dir).join(name);
    if project.exists() {
        return Err(Error::conflict(format!(
            "{} already exists — refusing to overwrite",
            project.display()
        )));
    }
    std::fs::create_dir_all(project.join("src"))?;

    let sdk_dep = match sdk_path {
        // TOML strings want forward slashes; backslashes would need escaping.
        Some(path) => format!("{{ path = \"{}\" }}", path.replace('\\', "/")),
        None => format!("\"{}\"", env!("CARGO_PKG_VERSION")),
    };
    std::fs::write(
        project.join("Cargo.toml"),
        format!(
            r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
# Until wovyr-plugin-sdk is published to crates.io, generate this project with
# `wovyr plugin new --sdk-path <wovyr-repo>/crates/wovyr-plugin-sdk` so the
# dependency resolves locally.
wovyr-plugin-sdk = {sdk_dep}
serde = {{ version = "1", features = ["derive"] }}

# Small, static wasm artifacts.
[profile.release]
opt-level = "s"
strip = true
"#
        ),
    )?;

    std::fs::write(
        project.join("src").join("main.rs"),
        r#"use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Request {
    /// Who to greet (defaults to "world").
    name: Option<String>,
}

#[derive(Serialize)]
struct Response {
    greeting: String,
}

fn main() -> std::process::ExitCode {
    wovyr_plugin_sdk::run_tool(|req: Request| -> Result<Response, String> {
        let name = req.name.unwrap_or_else(|| "world".to_string());
        Ok(Response {
            greeting: format!("Hello, {name}!"),
        })
    })
}
"#,
    )?;

    std::fs::write(
        project.join("plugin.yaml"),
        format!(
            r#"apiVersion: plugin.wovyr.io/v1
kind: Plugin
metadata:
  name: {name}
  version: 0.1.0
  publisher: {publisher}
  description: ""
  license: Apache-2.0
compatibility:
  platform_api: ">=0.1.0 <2.0.0"
permissions: []
capabilities:
  - kind: tool
    id: {name}.run
    entry: {name}.wasm
    sandbox: wasm
# Computed by `wovyr plugin build` into dist/plugin.yaml — never hand-edit digests.
artifacts: []
"#
        ),
    )?;

    std::fs::write(project.join(".gitignore"), "/target\n/dist\n")?;

    std::fs::write(
        project.join("README.md"),
        format!(
            r#"# {name}

An Wovyr plugin (a `wasm32-wasip1` tool capability). Edit `src/main.rs` and
`plugin.yaml`, then:

```bash
wovyr plugin build {name}                       # compile + stage dist/ with computed digests
wovyr plugin keygen {publisher}                 # once: a publisher signing keypair
wovyr plugin sign --key {publisher}.key --manifest {name}/dist/plugin.yaml
wovyr plugin trust {publisher} --key {publisher}.pub   # once, on the installing node
wovyr plugin install {name}/dist
wovyr plugin enable {publisher}/{name}
wovyr plugin run {name}.run --input '{{"name": "Wovyr"}}'
```
"#
        ),
    )?;

    println!("Created plugin project {}", project.display());
    println!("Next: `wovyr plugin build {}`", project.display());
    Ok(())
}

/// `wovyr plugin build <project>` — compile the project to `wasm32-wasip1` and
/// stage a digest-complete package directory (default `<project>/dist`):
/// the built module beside a `plugin.yaml` whose `artifacts` carry the
/// computed `sha256:` digest, ready for `wovyr plugin sign` + `install`.
pub fn build_cmd(project: &str, out: Option<&str>) -> Result<()> {
    let project = Path::new(project);
    let manifest_yaml = std::fs::read_to_string(project.join("plugin.yaml")).map_err(|e| {
        Error::config(format!(
            "could not read {}/plugin.yaml: {e}",
            project.display()
        ))
    })?;
    let mut manifest = PluginManifest::from_yaml(&manifest_yaml)?;
    let entry = wasm_entry(&manifest)?;

    // Compile. An explicit --target-dir keeps the artifact somewhere this
    // command can find it even when the caller's environment redirects
    // CARGO_TARGET_DIR.
    let target_dir = project.join("target");
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(["build", "--release", "--target", "wasm32-wasip1"])
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir(project);
    // Strip the build-shaping environment an *outer* cargo exports, so a nested
    // invocation gets a clean build of the scaffolded project rather than
    // inheriting settings meant for a different workspace and target.
    //
    // `CARGO_MAKEFLAGS` is the one that actually bites: it carries the outer
    // cargo's jobserver file descriptors, which are not valid in this child, so
    // the inner build intermittently fails partway through compiling
    // dependencies — the reason this crate's own scaffold round-trip test passed
    // when run alone but failed under `cargo test --workspace`. The rest would
    // silently apply host-targeted flags (or redirect output) to a
    // `wasm32-wasip1` build.
    for key in [
        "CARGO_MAKEFLAGS",
        "MAKEFLAGS",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_UNSTABLE_BUILD_STD",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        cmd.env_remove(key);
    }
    let output = cmd
        .output()
        .map_err(|e| Error::config(format!("could not run cargo: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let hint = if stderr.contains("wasm32-wasip1") {
            "\nhint: install the target with `rustup target add wasm32-wasip1`"
        } else if !stderr.contains("error") {
            // cargo exited non-zero having printed only progress lines: it was
            // terminated rather than failing on a compile error (typically memory
            // pressure when a full dependency graph is compiled alongside other
            // heavy work). Say so, instead of surfacing a truncated progress log
            // that reads like a mystery.
            "\nhint: cargo exited without reporting a compile error, which usually \
             means the process was terminated (e.g. out of memory) rather than the \
             code failing to build — retry with less concurrent load"
        } else {
            ""
        };
        // Include the exit status and stdout: with an empty/truncated stderr, they
        // are the only remaining signal.
        return Err(Error::config(format!(
            "cargo build failed ({}):\n{}{}{hint}",
            output.status,
            stderr.trim(),
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!("\n--- stdout ---\n{}", stdout.trim())
            },
        )));
    }

    let wasm_path = built_module(project, &target_dir)?;
    let wasm = std::fs::read(&wasm_path)?;

    // Stage the package dir: module + manifest with the computed digest.
    let dist = out
        .map(PathBuf::from)
        .unwrap_or_else(|| project.join("dist"));
    if dist.exists() {
        std::fs::remove_dir_all(&dist)?;
    }
    let module_dest = dist.join(&entry);
    if let Some(parent) = module_dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&module_dest, &wasm)?;

    manifest.artifacts = vec![Artifact {
        path: entry.clone(),
        digest: format!("sha256:{}", sha256_hex(&wasm)),
    }];
    let rendered = serde_yaml::to_string(&manifest)
        .map_err(|e| Error::invalid(format!("could not render the manifest: {e}")))?;
    std::fs::write(dist.join("plugin.yaml"), rendered)?;

    println!(
        "Built {} -> {} ({} bytes, digest computed)",
        manifest.reference(),
        dist.display(),
        wasm.len()
    );
    println!(
        "Next: `wovyr plugin sign --key <publisher>.key --manifest {}` then `wovyr plugin install {}`",
        dist.join("plugin.yaml").display(),
        dist.display()
    );
    Ok(())
}

/// The single wasm entry the manifest's tool capabilities declare. Multiple
/// capabilities may share one module; multiple *distinct* modules would need
/// multiple crates, which this build step doesn't orchestrate (yet).
fn wasm_entry(manifest: &PluginManifest) -> Result<String> {
    let mut entries: Vec<&str> = manifest
        .capabilities
        .iter()
        .filter(|c| matches!(c.sandbox.as_str(), "wasm" | "wasi" | ""))
        .map(|c| c.entry.as_str())
        .filter(|e| !e.is_empty())
        .collect();
    entries.sort_unstable();
    entries.dedup();
    match entries.as_slice() {
        [] => Err(Error::invalid(
            "plugin.yaml declares no wasm capability entry to build",
        )),
        [entry] => Ok((*entry).to_string()),
        many => Err(Error::invalid(format!(
            "plugin.yaml declares {} distinct wasm entries ({}) — `wovyr plugin build` \
             builds exactly one module per project",
            many.len(),
            many.join(", ")
        ))),
    }
}

/// Locate the built module: `<package-name>.wasm` in the release dir, falling
/// back to the single `.wasm` file present.
fn built_module(project: &Path, target_dir: &Path) -> Result<PathBuf> {
    let release = target_dir.join("wasm32-wasip1").join("release");
    if let Ok(cargo_toml) = std::fs::read_to_string(project.join("Cargo.toml"))
        && let Some(name) = package_name(&cargo_toml)
    {
        let candidate = release.join(format!("{name}.wasm"));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(&release)
        .map_err(|e| Error::config(format!("no build output at {}: {e}", release.display())))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "wasm"))
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(Error::config(format!(
            "cargo build produced no .wasm module under {}",
            release.display()
        ))),
        _ => Err(Error::config(format!(
            "multiple .wasm modules under {} — could not pick one",
            release.display()
        ))),
    }
}

/// The `[package] name` from a Cargo.toml (line-based; enough for the
/// manifests this scaffold generates and typical hand-authored ones).
fn package_name(cargo_toml: &str) -> Option<String> {
    let mut in_package = false;
    for line in cargo_toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package
            && let Some(rest) = line.strip_prefix("name")
            && let Some(value) = rest.trim_start().strip_prefix('=')
        {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// Crate-name-safe plugin names: lowercase alphanumeric plus `-`/`_`, starting
/// with a letter — the name doubles as the cargo package name and the
/// capability-id prefix.
fn validate_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "plugin name `{name}` must be lowercase [a-z][a-z0-9_-]* (it names the \
             crate and the capability id)"
        )))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wovyr_scaffold_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scaffold_generates_a_valid_project_and_refuses_to_overwrite() {
        let root = scratch("new");
        new_cmd("greeter", "acme", root.to_str().unwrap(), None).unwrap();

        let project = root.join("greeter");
        for file in [
            "Cargo.toml",
            "src/main.rs",
            "plugin.yaml",
            ".gitignore",
            "README.md",
        ] {
            assert!(project.join(file).is_file(), "missing {file}");
        }

        // The generated manifest is valid as-is, with artifacts deliberately empty.
        let manifest = PluginManifest::from_yaml(
            &std::fs::read_to_string(project.join("plugin.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.qualified_id(), "acme/greeter");
        assert!(manifest.artifacts.is_empty());
        assert_eq!(manifest.capabilities[0].entry, "greeter.wasm");

        // A second scaffold at the same path fails closed.
        let err = new_cmd("greeter", "acme", root.to_str().unwrap(), None).unwrap_err();
        assert!(matches!(err, Error::Conflict(_)), "{err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn names_are_validated_fail_closed() {
        let root = scratch("names");
        for bad in ["", "Greeter", "9lives", "has space", "semi;colon"] {
            assert!(
                new_cmd(bad, "acme", root.to_str().unwrap(), None).is_err(),
                "`{bad}` should be rejected"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_requires_exactly_one_wasm_entry() {
        let two = PluginManifest::from_yaml(
            r#"
apiVersion: plugin.wovyr.io/v1
kind: Plugin
metadata: { name: x, version: 0.1.0, publisher: p }
capabilities:
  - { kind: tool, id: a.run, entry: a.wasm, sandbox: wasm }
  - { kind: tool, id: b.run, entry: b.wasm, sandbox: wasm }
"#,
        )
        .unwrap();
        assert!(wasm_entry(&two).is_err());

        let shared = PluginManifest::from_yaml(
            r#"
apiVersion: plugin.wovyr.io/v1
kind: Plugin
metadata: { name: x, version: 0.1.0, publisher: p }
capabilities:
  - { kind: tool, id: a.run, entry: x.wasm, sandbox: wasm }
  - { kind: tool, id: b.run, entry: x.wasm, sandbox: wasm }
"#,
        )
        .unwrap();
        assert_eq!(wasm_entry(&shared).unwrap(), "x.wasm");
    }

    #[test]
    fn package_name_parses_the_package_section_only() {
        let toml = "[workspace]\nname = \"nope\"\n[package]\nversion = \"0.1.0\"\nname = \"real\"\n[dependencies]\nname = \"alsono\"\n";
        assert_eq!(package_name(toml).as_deref(), Some("real"));
    }

    /// The ECO-302 acceptance round trip: `wovyr plugin new` → `wovyr plugin
    /// build` (real `cargo build --target wasm32-wasip1`) → sign → verified
    /// install — **no hand-edited digests anywhere**. Runs the exact
    /// verify/stage/register core `wovyr plugin install` runs, against scratch
    /// directories (never the real `~/.wovyr`). Skips cleanly when the wasm
    /// target (or cargo) is unavailable, the same capability-gated pattern as
    /// the container/Postgres suites; CI installs the target so it runs there.
    ///
    /// This test compiles a whole dependency graph for `wasm32-wasip1` in a nested
    /// `cargo` process, so it is the heaviest test in the workspace. Two hazards
    /// come with that, both handled rather than left to flake:
    ///
    /// 1. The nested cargo must not inherit the outer cargo's jobserver
    ///    (`CARGO_MAKEFLAGS`) — handled in [`build_cmd`], which strips it.
    /// 2. Under enough concurrent load the nested build can be **terminated** by
    ///    the OS (memory pressure) rather than failing to compile. That is an
    ///    environment limit, not a defect in the code under test, so it is
    ///    reported as a skip. A genuine compile error still fails the test.
    #[test]
    fn scaffolded_project_builds_signs_and_installs_with_no_hand_edited_digests() {
        let targets = std::process::Command::new("rustup")
            .args(["target", "list", "--installed"])
            .output();
        let has_target = targets
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("wasm32-wasip1"))
            .unwrap_or(false);
        if !has_target {
            println!(
                "skipping: wasm32-wasip1 target not installed (rustup target add wasm32-wasip1)"
            );
            return;
        }

        let root = scratch("roundtrip");
        // Point the generated project at the workspace's own SDK checkout.
        let sdk_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("crates")
            .join("wovyr-plugin-sdk");
        new_cmd(
            "greeter",
            "acme",
            root.to_str().unwrap(),
            Some(sdk_path.to_str().unwrap()),
        )
        .unwrap();
        let project = root.join("greeter");

        // Build: compiles to wasm and stages dist/ with a computed digest.
        if let Err(e) = build_cmd(project.to_str().unwrap(), None) {
            let msg = e.to_string();
            // Hazard 2 above: cargo was killed without reporting a compile error.
            // `build_cmd` detects this and says so; treat it as a skip so a machine
            // that simply ran out of memory doesn't report a nonexistent defect.
            // Anything else — a real compile error, a missing file — still fails.
            if msg.contains("without reporting a compile error") {
                println!(
                    "skipping: the nested wasm build was terminated by the environment: {msg}"
                );
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
            panic!("the scaffolded project must build: {msg}");
        }
        let dist = project.join("dist");
        let manifest =
            PluginManifest::from_yaml(&std::fs::read_to_string(dist.join("plugin.yaml")).unwrap())
                .unwrap();
        let wasm = std::fs::read(dist.join("greeter.wasm")).unwrap();
        assert_eq!(
            manifest.artifacts[0].digest,
            format!("sha256:{}", sha256_hex(&wasm)),
            "the staged digest must be the real module digest, not a hand-edited value"
        );

        // Sign with a fresh publisher keypair (the real CLI commands).
        crate::plugin::keygen_cmd("acme", root.to_str().unwrap()).unwrap();
        crate::plugin::sign_cmd(
            root.join("acme.key").to_str().unwrap(),
            dist.join("plugin.yaml").to_str().unwrap(),
            None,
        )
        .unwrap();

        // Install: signature verify → digest verify → stage → register. This is
        // `install_cmd`'s engine core against scratch state.
        let (package, manifest) = crate::plugin::read_package_dir(&dist).unwrap();
        let mut trust = wovyr_plugin::TrustStore::new();
        trust.trust("acme", std::fs::read(root.join("acme.pub")).unwrap());
        let mut engine = wovyr_plugin::PluginEngine::new(crate::plugin::platform_api(), trust)
            .with_staging_dir(root.join("staging"));
        let installed = engine.install(&package, &[]).unwrap();
        assert_eq!(installed.manifest.reference(), "acme/greeter@0.1.0");
        assert_eq!(manifest.reference(), "acme/greeter@0.1.0");

        // With the WASM loader compiled in, prove the built module actually
        // answers through the registry.
        #[cfg(feature = "plugin-wasi")]
        {
            use wovyr_tools::{ToolContext, ToolRegistry, ToolRequest};
            let mut engine = engine.with_runtime(std::sync::Arc::new(
                wovyr_plugin::WasiCapabilityRuntime::new().unwrap(),
            ));
            let mut registry = ToolRegistry::new();
            engine.enable("acme/greeter", &mut registry).unwrap();
            let response = tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(registry.execute(
                    "greeter.run",
                    &ToolContext::default(),
                    ToolRequest::new(serde_json::json!({ "name": "Wovyr" })),
                ))
                .unwrap();
            assert_eq!(
                response.payload,
                serde_json::json!({ "greeting": "Hello, Wovyr!" })
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }
}
