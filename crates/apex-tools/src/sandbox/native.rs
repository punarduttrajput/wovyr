//! [`NativeSandbox`] — an OS-process sandbox with real resource enforcement.

use super::Sandbox;
use super::cap;
use super::types::{CommandOutcome, ResourceLimits, SandboxBackend, SandboxCommand, SandboxError};
use async_trait::async_trait;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// A native-process sandbox: timeout-enforced, output-capped child process with
/// per-execution `setrlimit` memory/CPU caps on Unix.
#[derive(Clone, Debug)]
pub struct NativeSandbox {
    limits: ResourceLimits,
}

impl NativeSandbox {
    /// Construct with just a timeout (other limits default).
    pub fn new(timeout: Duration) -> Self {
        Self {
            limits: ResourceLimits {
                timeout,
                ..ResourceLimits::default()
            },
        }
    }

    /// Construct with full resource limits.
    pub fn with_limits(limits: ResourceLimits) -> Self {
        Self { limits }
    }

    /// Build the child process, applying `setrlimit` caps in a `pre_exec` hook on
    /// Unix (memory `RLIMIT_AS`, CPU `RLIMIT_CPU`).
    fn build_command(&self, program: &str, args: &[&str], workdir: &str) -> Command {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let mut std_cmd = std::process::Command::new(program);
            std_cmd
                .args(args)
                .current_dir(workdir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let limits = self.limits.clone();
            // SAFETY: `apply_rlimits` only issues `setrlimit` syscalls and touches
            // stack memory, so it is async-signal-safe to run in the forked child
            // before exec.
            unsafe {
                std_cmd.pre_exec(move || apply_rlimits(&limits));
            }
            let mut cmd = Command::from(std_cmd);
            cmd.kill_on_drop(true);
            cmd
        }
        #[cfg(not(unix))]
        {
            let mut command = Command::new(program);
            command
                .args(args)
                .current_dir(workdir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            command
        }
    }

    /// Run `program` with `args` in `workdir`, capturing (capped) output.
    ///
    /// On timeout the child is killed and a [`CommandOutcome`] with
    /// `timed_out = true` is returned rather than an error. A process killed by a
    /// CPU/file-size rlimit returns `resource_exceeded = true`.
    pub async fn run(
        &self,
        program: &str,
        args: &[&str],
        workdir: &str,
    ) -> Result<CommandOutcome, SandboxError> {
        let mut command = self.build_command(program, args, workdir);

        let child = command
            .spawn()
            .map_err(|e| SandboxError::Spawn(format!("`{program}`: {e}")))?;

        match tokio::time::timeout(self.limits.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                let (stdout, t1) = cap(&output.stdout, self.limits.max_output_bytes);
                let (stderr, t2) = cap(&output.stderr, self.limits.max_output_bytes);
                let (signal, resource_exceeded) = terminating_signal(&output.status);
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
                stderr: format!("execution exceeded timeout of {:?}", self.limits.timeout),
                timed_out: true,
                truncated: false,
                signal: None,
                resource_exceeded: false,
            }),
        }
    }
}

#[async_trait]
impl Sandbox for NativeSandbox {
    fn backend(&self) -> SandboxBackend {
        SandboxBackend::Native
    }

    async fn execute(&self, cmd: &SandboxCommand) -> Result<CommandOutcome, SandboxError> {
        let args: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
        NativeSandbox::with_limits(cmd.limits.clone())
            .run(&cmd.program, &args, &cmd.workdir)
            .await
    }
}

/// Map a process exit status to its terminating Unix signal and whether that signal
/// indicates a resource-limit breach (`SIGXCPU`/`SIGXFSZ`).
#[cfg(unix)]
fn terminating_signal(status: &std::process::ExitStatus) -> (Option<i32>, bool) {
    use std::os::unix::process::ExitStatusExt;
    let signal = status.signal();
    let exceeded = matches!(signal, Some(libc::SIGXCPU) | Some(libc::SIGXFSZ));
    (signal, exceeded)
}

#[cfg(not(unix))]
fn terminating_signal(_status: &std::process::ExitStatus) -> (Option<i32>, bool) {
    (None, false)
}

/// Apply `setrlimit` caps in the forked child before `exec`. Async-signal-safe:
/// only stack memory and `setrlimit` syscalls.
#[cfg(unix)]
fn apply_rlimits(limits: &ResourceLimits) -> std::io::Result<()> {
    fn set(resource: libc::__rlimit_resource_t, soft: u64, hard: u64) -> std::io::Result<()> {
        let lim = libc::rlimit {
            rlim_cur: soft as libc::rlim_t,
            rlim_max: hard as libc::rlim_t,
        };
        // SAFETY: `lim` is a valid, fully-initialized rlimit for the duration of the call.
        if unsafe { libc::setrlimit(resource, &lim) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    if let Some(bytes) = limits.memory_bytes {
        set(libc::RLIMIT_AS, bytes, bytes)?;
    }
    if let Some(millis) = limits.cpu_millis {
        // RLIMIT_CPU is whole seconds: SIGXCPU at the soft limit, SIGKILL one
        // second later at the hard limit.
        let secs = millis.div_ceil(1000).max(1) as u64;
        set(libc::RLIMIT_CPU, secs, secs + 1)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn echo(text: &str) -> CommandOutcome {
        let sb = NativeSandbox::new(Duration::from_secs(10));
        if cfg!(windows) {
            sb.run("cmd", &["/C", &format!("echo {text}")], ".")
                .await
                .unwrap()
        } else {
            sb.run("sh", &["-c", &format!("echo {text}")], ".")
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn captures_stdout_and_exit_code() {
        let out = echo("apex_sandbox_ok").await;
        assert!(out.stdout.contains("apex_sandbox_ok"));
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        assert!(!out.resource_exceeded);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn enforces_timeout() {
        let sb = NativeSandbox::new(Duration::from_millis(150));
        let out = sb.run("sh", &["-c", "sleep 5"], ".").await.unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cpu_rlimit_terminates_with_resource_exceeded() {
        // A busy loop with a 1s CPU cap is killed by SIGXCPU well before the 30s
        // wall-clock timeout, so the breach is attributed to the resource limit.
        let limits = ResourceLimits {
            timeout: Duration::from_secs(30),
            cpu_millis: Some(1000),
            ..ResourceLimits::default()
        };
        let sb = NativeSandbox::with_limits(limits);
        let out = sb
            .run("sh", &["-c", "while :; do :; done"], ".")
            .await
            .unwrap();
        assert!(!out.timed_out, "should hit the CPU cap, not the wall clock");
        assert!(out.resource_exceeded);
        assert_eq!(out.signal, Some(libc::SIGXCPU));
    }
}
