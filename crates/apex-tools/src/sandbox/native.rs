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

        // Windows: enforce memory/process-count/CPU-time caps via a Job Object
        // (SBX-102) — the non-Unix analog of the `setrlimit` `pre_exec` hook applied
        // in `build_command` on Unix. The guard is held across the wait; on drop
        // (`KILL_ON_JOB_CLOSE`) any process still alive in the job is terminated, so a
        // timed-out or breaching child can't outlive the call.
        #[cfg(windows)]
        let _job = assign_job_object(&child, &self.limits);

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
/// indicates a resource-limit breach (`SIGXCPU`/`SIGXFSZ`). Shared with the container
/// backend, whose CLI runner maps outcomes identically.
#[cfg(unix)]
pub(super) fn terminating_signal(status: &std::process::ExitStatus) -> (Option<i32>, bool) {
    use std::os::unix::process::ExitStatusExt;
    let signal = status.signal();
    let exceeded = matches!(signal, Some(libc::SIGXCPU) | Some(libc::SIGXFSZ));
    (signal, exceeded)
}

#[cfg(not(unix))]
pub(super) fn terminating_signal(_status: &std::process::ExitStatus) -> (Option<i32>, bool) {
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

/// Create a Job Object enforcing `limits`, assign `child` to it, and return the guard
/// (SBX-102). `None` when no enforceable limit is set or job creation fails — the run
/// then proceeds with just the timeout + output cap, exactly as before. The returned
/// guard must be held for the child's lifetime; dropping it closes the job (killing
/// any survivors via `KILL_ON_JOB_CLOSE`).
#[cfg(windows)]
fn assign_job_object(child: &tokio::process::Child, limits: &ResourceLimits) -> Option<JobObject> {
    let job = JobObject::with_limits(limits)?;
    if let Some(handle) = child.raw_handle() {
        // SAFETY: `handle` is the live process handle owned by `child`, which outlives
        // this call (the guard is dropped after `wait_with_output` completes).
        unsafe { job.assign(handle) };
    }
    Some(job)
}

/// An owned Windows Job Object handle enforcing per-execution resource caps, mirroring
/// the Unix `setrlimit` path: `ProcessMemoryLimit` ↔ `RLIMIT_AS`, a per-job user-CPU
/// time limit ↔ `RLIMIT_CPU` (a total-time quota, not a rate — the closest analog),
/// plus an active-process cap (the container backend's `pids.max` analog, which Unix
/// native has no equivalent for). `KILL_ON_JOB_CLOSE` guarantees teardown.
#[cfg(windows)]
struct JobObject {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// SAFETY: a Windows Job Object HANDLE is a process-global kernel handle — valid and
// usable from any thread. The guard is held across an `.await` in `run()`, so the
// future must be `Send`; the raw pointer is the only reason it wouldn't be.
#[cfg(windows)]
unsafe impl Send for JobObject {}

#[cfg(windows)]
impl JobObject {
    /// Create and configure a job for `limits`, or `None` if nothing to enforce.
    fn with_limits(limits: &ResourceLimits) -> Option<Self> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_TIME,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        if limits.memory_bytes.is_none() && limits.max_pids.is_none() && limits.cpu_millis.is_none()
        {
            return None;
        }

        // SAFETY: null attributes + null name creates a new anonymous, unnamed job.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return None;
        }

        // SAFETY: the struct is plain-old-data; zeroing is a valid initial state.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Some(bytes) = limits.memory_bytes {
            info.ProcessMemoryLimit = bytes as usize;
            flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        }
        if let Some(pids) = limits.max_pids {
            info.BasicLimitInformation.ActiveProcessLimit = pids;
            flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        }
        if let Some(millis) = limits.cpu_millis {
            // Total user-mode CPU time across the job, in 100ns units.
            info.BasicLimitInformation.PerJobUserTimeLimit = (millis as i64) * 10_000;
            flags |= JOB_OBJECT_LIMIT_JOB_TIME;
        }
        info.BasicLimitInformation.LimitFlags = flags;

        // SAFETY: `info` is a fully-initialized struct of the matching class/size.
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            // SAFETY: `handle` is a valid job handle we just created.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return None;
        }
        Some(Self { handle })
    }

    /// Assign `process` to this job so the caps apply to it (and, since the job is
    /// inherited, to any child it spawns).
    ///
    /// # Safety
    /// `process` must be a valid, open process handle for the assignment's duration.
    unsafe fn assign(&self, process: std::os::windows::io::RawHandle) {
        // SAFETY: caller guarantees `process` is live; `self.handle` is a valid job.
        unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                self.handle,
                process as windows_sys::Win32::Foundation::HANDLE,
            );
        }
    }
}

#[cfg(windows)]
impl Drop for JobObject {
    fn drop(&mut self) {
        // Closing the last handle terminates any process still in the job
        // (`KILL_ON_JOB_CLOSE`), so a survivor can't outlive the run.
        // SAFETY: `self.handle` is a valid job handle owned by this guard.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
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

    // --- SBX-102: Windows Job Object resource enforcement ----------------------

    #[cfg(windows)]
    #[tokio::test]
    async fn job_object_active_process_limit_blocks_child_spawns() {
        // `max_pids = 1`: the assigned `cmd.exe` is the only process the job permits,
        // so a child it later tries to spawn is blocked — the process-count analog of
        // the container backend's `pids.max` cap, which native had no enforcement for
        // before SBX-102. The `ping` delay guarantees the job is assigned before the
        // child spawn is attempted.
        let limits = ResourceLimits {
            timeout: Duration::from_secs(20),
            max_pids: Some(1),
            ..ResourceLimits::default()
        };
        let sb = NativeSandbox::with_limits(limits);
        let out = sb
            .run(
                "cmd",
                &["/C", "ping -n 2 127.0.0.1 >nul & cmd /c echo SPAWNED_CHILD"],
                ".",
            )
            .await
            .unwrap();
        assert!(
            !out.stdout.contains("SPAWNED_CHILD"),
            "the active-process cap must block the child spawn; stdout: {:?}",
            out.stdout
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn job_object_memory_limit_fails_an_over_allocating_child() {
        // `ProcessMemoryLimit` ≈ 256 MiB (the `RLIMIT_AS` analog): a process trying to
        // commit ~1 GiB is denied and dies non-zero, rather than running with zero
        // memory isolation as the pre-SBX-102 non-Unix path did.
        let limits = ResourceLimits {
            timeout: Duration::from_secs(30),
            memory_bytes: Some(256 * 1024 * 1024),
            ..ResourceLimits::default()
        };
        let sb = NativeSandbox::with_limits(limits);
        // `ErrorActionPreference = Stop` makes the OutOfMemoryException terminating, so
        // the cap breach ends the script (non-zero, no `ALLOC_OK`) rather than being a
        // non-terminating error the next `;`-chained statement would run past.
        let out = sb
            .run(
                "powershell",
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "$ErrorActionPreference = 'Stop'; $a = New-Object byte[] 1073741824; \
                     $a[0] = 1; Write-Output ALLOC_OK",
                ],
                ".",
            )
            .await
            .unwrap();
        assert!(
            !out.stdout.contains("ALLOC_OK"),
            "the ~1 GiB allocation must be denied under the 256 MiB job memory cap; \
             stdout: {:?} stderr: {:?}",
            out.stdout,
            out.stderr
        );
        assert_ne!(
            out.exit_code,
            Some(0),
            "the memory-cap breach must terminate the process non-zero; stdout: {:?} stderr: {:?}",
            out.stdout,
            out.stderr
        );
        assert!(
            out.stderr.contains("OutOfMemoryException"),
            "the breach must surface as an OOM from the job memory cap; stderr: {:?}",
            out.stderr
        );
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
