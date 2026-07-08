//! [`FirecrackerSandbox`] — a Firecracker microVM sandbox.

use super::Sandbox;
use super::cap;
use super::types::{CommandOutcome, ResourceLimits, SandboxBackend, SandboxCommand, SandboxError};
use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

/// A Firecracker microVM machine configuration
/// ([sandbox runtime §2](../../../docs/07-tool-runtime/sandbox-runtime.md)).
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
}
