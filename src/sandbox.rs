//! §15B — The sandboxed process runner behind the generic execution gate.
//!
//! The validation command comes from an LLM, so it is treated as hostile.
//! Protection is layered, and the layer that actually applied is reported in
//! every result ([`Isolation`]) — the Conductor never has to assume.
//!
//! | Layer | Platforms | Stops |
//! |---|---|---|
//! | Watchdog: wall-clock timeout, tree RSS limit, tree process cap, tree kill | all | hangs, memory bombs, runaway trees (not exponential fork bombs) |
//! | Scrubbed environment (allowlist) | all | API-key exfiltration via env |
//! | rlimits: NPROC, FSIZE, CPU, CORE | Unix | fast fork bombs, disk filling, CPU spin |
//! | Job Object: active-process limit, job memory limit, kill-on-close | Windows | fork bombs, memory bombs, stray processes |
//! | Landlock: writes only inside the sandbox dir, TCP denied | Linux ≥ 5.13 | `rm -rf ~`, writes outside the sandbox, network |
//!
//! No shell is ever involved (§9): the command is tokenised with POSIX word
//! splitting rules (`shlex`) and the first word is exec'd directly.

#[cfg(unix)]
use crate::config::GATE_MAX_FILE_BYTES;
use crate::config::{
    GATE_ENV_PASSTHROUGH, GATE_MAX_OUTPUT_BYTES, GATE_READ_PATHS_ENV, GATE_SAMPLE_INTERVAL_MS,
    REQUIRE_FS_ISOLATION_ENV,
};
#[cfg(unix)]
use crate::config::GATE_NPROC_HEADROOM;
use crate::error::{Error, Result};
use crate::events::{emit, ContainerExit, EngineEvent, EventSink, Isolation};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// Resource limits for one sandboxed run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SandboxLimits {
    pub timeout: Duration,
    /// Resident-set limit for the whole process tree.
    pub mem_bytes: u64,
    /// Maximum simultaneously live processes in the tree.
    pub max_procs: u32,
    /// Apply OS-specific hardening (Unix rlimits + process group, Linux
    /// Landlock) on top of the portable watchdog. Always true in production;
    /// tests turn it off to measure what the portable layer alone — i.e. a
    /// Windows or macOS-without-rlimits host — actually guarantees.
    pub native_hardening: bool,
}

/// What came back from a sandboxed run.
#[derive(Debug, Clone)]
pub struct SandboxOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit: ContainerExit,
    /// Process exit code, or -1 when killed / signalled.
    pub exit_code: i32,
    pub elapsed: Duration,
    pub peak_rss_bytes: u64,
    pub isolation: Isolation,
}

impl SandboxOutput {
    pub fn success(&self) -> bool {
        self.exit == ContainerExit::Exited(0)
    }
}

/// Monotonic container id so events from concurrent gates can be told apart.
static NEXT_CONTAINER_ID: AtomicU64 = AtomicU64::new(1);

/// Tokenise a validation command into argv without a shell. Quotes group
/// words; `;`, `|`, `&&`, `$()` and friends are just literal characters.
pub fn split_command(command: &str) -> Result<Vec<String>> {
    let argv = shlex::split(command)
        .ok_or_else(|| Error::SandboxError(format!("unbalanced quotes in command: {command}")))?;
    if argv.is_empty() {
        return Err(Error::SandboxError("empty validation_command".into()));
    }
    Ok(argv)
}

/// Run `argv` inside `root` under `limits`.
///
/// Returns `Ok` for every run that got as far as spawning — including
/// timeouts and limit kills, which are reported in [`SandboxOutput::exit`]
/// so the 3-strike loop can feed them back like any other gate failure.
///
/// # Errors
/// - [`Error::DependencyMissing`] — `argv[0]` is not on PATH.
/// - [`Error::SandboxError`] — the sandbox could not be set up
///   (including Landlock being unavailable while
///   `GATESTRAINER_REQUIRE_FS_ISOLATION=1`).
pub async fn run_sandboxed(
    root: &Path,
    argv: &[String],
    limits: SandboxLimits,
    sink: Option<&EventSink>,
) -> Result<SandboxOutput> {
    let (bin, args) = argv
        .split_first()
        .ok_or_else(|| Error::SandboxError("empty argv".into()))?;

    let tmp = root.join(".tmp");
    std::fs::create_dir_all(&tmp)
        .map_err(|e| Error::SandboxError(format!("mkdir {}: {e}", tmp.display())))?;

    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    for key in GATE_ENV_PASSTHROUGH {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    for key in ["TMPDIR", "TMP", "TEMP"] {
        cmd.env(key, &tmp);
    }

    // `guard` owns kernel handles for the whole run: the Landlock ruleset fd
    // (needed until the fork) and, on Windows, the Job Object that holds the
    // process tree (closing it kills every survivor).
    let (isolation, mut guard) = harden(&mut cmd, root, bin, limits)?;

    let id = NEXT_CONTAINER_ID.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::DependencyMissing(bin.clone())
        } else {
            Error::SandboxError(format!("spawn {bin}: {e}"))
        }
    })?;
    let root_pid = child
        .id()
        .ok_or_else(|| Error::SandboxError(format!("{bin} exited before its pid was read")))?;
    // Windows: the child was created suspended; put it in the Job Object,
    // then resume it. On error `child` is dropped, which kills it.
    let isolation = guard.after_spawn(root_pid, isolation)?;

    emit(
        sink,
        EngineEvent::ContainerStarted {
            id,
            command: argv.join(" "),
            timeout_ms: limits.timeout.as_millis() as u64,
            mem_limit_bytes: limits.mem_bytes,
            max_procs: limits.max_procs,
            isolation,
        },
    );
    tracing::info!(target: "sandbox", id, pid = root_pid, %bin, isolation = isolation.label(), "container started");

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_task = tokio::spawn(read_capped(stdout_pipe, GATE_MAX_OUTPUT_BYTES));
    let stderr_task = tokio::spawn(read_capped(stderr_pipe, GATE_MAX_OUTPUT_BYTES));

    // ---- Watchdog ---------------------------------------------------------
    // Polling is the only portable way to see a process tree. It enforces
    // the timeout and the memory cap and kills the whole tree, but it cannot
    // stop an exponential fork bomb in time: measured, a bash bomb reached
    // ~6k processes within ~35 ms, before a second scan could finish (a scan
    // costs ~30 ms at a normal process count and ~1.4 s during the bomb).
    // Sampling faster did not change that. Fork-bomb prevention needs an OS
    // primitive: RLIMIT_NPROC on Unix (see `harden`).
    let mut sys = System::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(GATE_SAMPLE_INTERVAL_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut peak_rss = 0u64;
    let mut tree: Vec<Pid> = vec![Pid::from_u32(root_pid)];

    let watched = tokio::time::timeout(limits.timeout, async {
        loop {
            tokio::select! {
                status = child.wait() => return Watch::Exited(status),
                _ = ticker.tick() => {
                    let s = sample_tree(&mut sys, root_pid);
                    tree = s.pids.clone();
                    peak_rss = peak_rss.max(s.rss_bytes);
                    emit(sink, EngineEvent::ContainerSample {
                        id,
                        cpu_pct: s.cpu_pct,
                        rss_bytes: s.rss_bytes,
                        procs: s.pids.len() as u32,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                    });
                    if s.rss_bytes > limits.mem_bytes {
                        return Watch::Violation(ContainerExit::MemoryLimit);
                    }
                    if s.pids.len() as u32 > limits.max_procs {
                        return Watch::Violation(ContainerExit::ProcessLimit);
                    }
                }
            }
        }
    })
    .await;

    let (exit, exit_code) = match watched {
        Ok(Watch::Exited(Ok(status))) => classify_status(status),
        Ok(Watch::Exited(Err(e))) => {
            return Err(Error::SandboxError(format!("wait {bin}: {e}")));
        }
        Ok(Watch::Violation(reason)) => {
            kill_tree(root_pid, &tree, &mut sys, &guard);
            let _ = child.kill().await;
            (reason, -1)
        }
        Err(_elapsed) => {
            kill_tree(root_pid, &tree, &mut sys, &guard);
            let _ = child.kill().await;
            (ContainerExit::TimedOut, -1)
        }
    };

    // Grandchildren that inherited the pipes can keep them open after the
    // tree kill races; never let output collection hang the gate.
    let stdout = collect_output(stdout_task).await;
    let mut stderr = collect_output(stderr_task).await;
    let elapsed = started.elapsed();

    match exit {
        ContainerExit::TimedOut => stderr.push_str(&format!(
            "\n[sandbox] killed: exceeded the {}s timeout\n",
            limits.timeout.as_secs()
        )),
        ContainerExit::MemoryLimit => stderr.push_str(&format!(
            "\n[sandbox] killed: process tree exceeded the {} MiB memory limit\n",
            limits.mem_bytes / (1024 * 1024)
        )),
        ContainerExit::ProcessLimit => stderr.push_str(&format!(
            "\n[sandbox] killed: process tree exceeded {} processes\n",
            limits.max_procs
        )),
        ContainerExit::Signaled => stderr.push_str("\n[sandbox] terminated by signal (rlimit or external kill)\n"),
        ContainerExit::Exited(_) => {}
    }

    emit(
        sink,
        EngineEvent::ContainerFinished {
            id,
            exit,
            elapsed_ms: elapsed.as_millis() as u64,
            peak_rss_bytes: peak_rss,
        },
    );
    tracing::info!(target: "sandbox", id, ?exit, ms = elapsed.as_millis() as u64, "container finished");

    Ok(SandboxOutput {
        stdout,
        stderr,
        exit,
        exit_code,
        elapsed,
        peak_rss_bytes: peak_rss,
        isolation,
    })
}

enum Watch {
    Exited(std::io::Result<std::process::ExitStatus>),
    Violation(ContainerExit),
}

fn classify_status(status: std::process::ExitStatus) -> (ContainerExit, i32) {
    match status.code() {
        Some(code) => (ContainerExit::Exited(code), code),
        None => (ContainerExit::Signaled, -1),
    }
}

async fn read_capped<R: AsyncRead + Unpin>(pipe: Option<R>, cap: usize) -> String {
    let Some(mut r) = pipe else {
        return String::new();
    };
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    let mut dropped = 0usize;
    loop {
        match r.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                // Keep draining past the cap so the child never blocks on a
                // full pipe; just stop storing.
                let room = cap.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
                dropped += n.saturating_sub(room);
            }
            Err(e) => {
                kept.extend_from_slice(format!("\n[sandbox] pipe read error: {e}\n").as_bytes());
                break;
            }
        }
    }
    let mut s = String::from_utf8_lossy(&kept).into_owned();
    if dropped > 0 {
        s.push_str(&format!("\n[sandbox] output truncated: {dropped} more bytes discarded\n"));
    }
    s
}

async fn collect_output(task: tokio::task::JoinHandle<String>) -> String {
    match tokio::time::timeout(Duration::from_secs(2), task).await {
        Ok(Ok(s)) => s,
        Ok(Err(join)) => format!("[sandbox] output reader failed: {join}\n"),
        Err(_) => "[sandbox] output pipe held open by a surviving process; output abandoned\n".into(),
    }
}

// ---------------------------------------------------------------------------
// Process-tree sampling + killing (cross-platform via sysinfo)
// ---------------------------------------------------------------------------

struct TreeSample {
    pids: Vec<Pid>,
    cpu_pct: f32,
    rss_bytes: u64,
}

fn sample_tree(sys: &mut System, root_pid: u32) -> TreeSample {
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cpu().with_memory().without_tasks(),
    );
    let root = Pid::from_u32(root_pid);
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, p) in sys.processes() {
        if p.thread_kind().is_some() {
            continue;
        }
        if let Some(parent) = p.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    let mut pids = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if sys.process(pid).is_some() {
            pids.push(pid);
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    let (cpu_pct, rss_bytes) = pids
        .iter()
        .filter_map(|p| sys.process(*p))
        .fold((0.0f32, 0u64), |(c, m), p| (c + p.cpu_usage(), m + p.memory()));
    TreeSample { pids, cpu_pct, rss_bytes }
}

/// Kill the whole tree. On Windows the Job Object is terminated (it holds
/// every descendant). On Unix the child leads its own process group, so a
/// single `killpg` reaches every descendant that did not `setsid()` away.
/// The sysinfo pass catches anything left, on every OS.
fn kill_tree(root_pid: u32, last_known: &[Pid], sys: &mut System, guard: &HardenGuard) {
    guard.terminate();
    #[cfg(unix)]
    // SAFETY: plain syscall; a negative pid addresses the process group we
    // created with `process_group(0)`, whose id equals the child's pid.
    unsafe {
        libc::kill(-(root_pid as libc::pid_t), libc::SIGKILL);
    }
    let fresh = sample_tree(sys, root_pid).pids;
    for pid in last_known.iter().chain(fresh.iter()) {
        if let Some(p) = sys.process(*pid) {
            p.kill();
        }
    }
}

// ---------------------------------------------------------------------------
// Kernel hardening
// ---------------------------------------------------------------------------

/// Kernel handles owned for the lifetime of one sandboxed run.
#[derive(Default)]
struct HardenGuard {
    #[cfg(target_os = "linux")]
    _ruleset: Option<landlock::Ruleset>,
    /// Kept alive until the child has exec'd (the child reads it pre-exec).
    #[cfg(target_os = "linux")]
    _filter: Option<seccomp::Filter>,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl HardenGuard {
    /// Finish hardening once the child exists. Windows: assign the
    /// CREATE_SUSPENDED child to the Job Object *before* it runs (so it cannot
    /// fork past the limits first), then resume it — always, even when the
    /// assignment fails, which downgrades the reported isolation.
    #[cfg_attr(not(windows), allow(clippy::unnecessary_wraps, unused_variables))]
    fn after_spawn(&mut self, pid: u32, isolation: Isolation) -> Result<Isolation> {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            let assigned = job.assign(pid);
            job::resume_process(pid)
                .map_err(|e| Error::SandboxError(format!("could not resume the sandboxed process: {e}")))?;
            if let Err(reason) = assigned {
                tracing::warn!(target: "sandbox", %reason, "Job Object assignment failed; this gate runs with the watchdog only");
                self.job = None;
                return Ok(Isolation::Watchdog);
            }
        }
        Ok(isolation)
    }

    /// Kill everything in the Job Object (Windows); no-op elsewhere.
    fn terminate(&self) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
    }
}

/// Which [`Isolation`] level is in force, from which kernel layers
/// actually initialised (Linux only: `harden`'s other branches are fixed).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn isolation_for(has_landlock: bool, has_seccomp: bool) -> Isolation {
    match (has_landlock, has_seccomp) {
        (true, true) => Isolation::LandlockSeccomp,
        (true, false) => Isolation::Landlock,
        (false, true) => Isolation::Seccomp,
        (false, false) => Isolation::Rlimits,
    }
}

/// Apply every hardening layer available on this platform to `cmd` and
/// report which one ended up in force.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn harden(cmd: &mut Command, root: &Path, bin: &str, limits: SandboxLimits) -> Result<(Isolation, HardenGuard)> {
    let require_fs = std::env::var(REQUIRE_FS_ISOLATION_ENV).as_deref() == Ok("1");
    if !limits.native_hardening {
        return Ok((Isolation::Watchdog, HardenGuard::default()));
    }

    #[cfg(target_os = "linux")]
    let landlock = match landlock::prepare(root, &gate_read_roots(bin, &|k| std::env::var(k).ok())) {
        Ok(ruleset) => Some(ruleset),
        Err(reason) => {
            if require_fs {
                return Err(Error::SandboxError(format!(
                    "filesystem isolation required ({REQUIRE_FS_ISOLATION_ENV}=1) but Landlock is unavailable: {reason}"
                )));
            }
            tracing::warn!(target: "sandbox", %reason, "Landlock unavailable; gate writes are NOT confined to the sandbox dir");
            None
        }
    };
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root;
        if require_fs {
            return Err(Error::SandboxError(format!(
                "filesystem isolation required ({REQUIRE_FS_ISOLATION_ENV}=1) but is only implemented on Linux (Landlock)"
            )));
        }
    }

    #[cfg(unix)]
    {
        let nproc = current_user_tasks().map(|n| n + GATE_NPROC_HEADROOM);
        let cpus = std::thread::available_parallelism().map(|n| n.get() as u64).unwrap_or(1);
        // RLIMIT_CPU counts CPU-seconds across all threads, so a parallel
        // rustc legitimately burns more than wall time. Backstop only.
        let cpu_secs = limits.timeout.as_secs().max(1) * cpus + 5;
        let fsize = GATE_MAX_FILE_BYTES;

        #[cfg(target_os = "linux")]
        let ruleset_fd: Option<i32> = landlock.as_ref().map(|r| r.fd());
        // Built before the fork: the child only passes a pointer to it.
        #[cfg(target_os = "linux")]
        let net_filter = seccomp::Filter::no_inet();
        #[cfg(target_os = "linux")]
        let filter_prog = net_filter.as_ref().map(seccomp::Filter::prog);

        cmd.process_group(0);
        // SAFETY: the closure runs between fork and exec. It only issues
        // async-signal-safe syscalls (setrlimit, prctl, landlock_restrict_self)
        // on values computed before the fork; it does not allocate or lock.
        unsafe {
            cmd.pre_exec(move || {
                set_rlimit!(libc::RLIMIT_CORE, 0);
                set_rlimit!(libc::RLIMIT_FSIZE, fsize);
                set_rlimit!(libc::RLIMIT_CPU, cpu_secs);
                if let Some(n) = nproc {
                    set_rlimit!(libc::RLIMIT_NPROC, n);
                }
                #[cfg(target_os = "linux")]
                if let Some(fd) = ruleset_fd {
                    landlock::restrict_self(fd)?;
                }
                #[cfg(target_os = "linux")]
                if let Some(prog) = &filter_prog {
                    seccomp::install(prog)?;
                }
                Ok(())
            });
        }

        // `return` ends the Linux branch; the other OSes fall through.
        #[cfg(target_os = "linux")]
        #[allow(clippy::needless_return)]
        {
            let isolation = isolation_for(landlock.is_some(), net_filter.is_some());
            if net_filter.is_none() {
                tracing::warn!(target: "sandbox", "seccomp unavailable on this architecture; gates can open UDP sockets");
            }
            return Ok((isolation, HardenGuard { _ruleset: landlock, _filter: net_filter }));
        }
        #[cfg(not(target_os = "linux"))]
        return Ok((Isolation::Rlimits, HardenGuard::default()));
    }

    // Windows: a Job Object is the per-tree kernel cap Unix lacks — an
    // active-process limit (fork bombs), a job-wide memory limit, and
    // kill-on-close. The child starts suspended; `after_spawn` assigns it.
    #[cfg(windows)]
    {
        match job::Job::create(limits.max_procs, limits.mem_bytes) {
            Ok(job) => {
                cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
                Ok((Isolation::JobObject, HardenGuard { job: Some(job) }))
            }
            Err(reason) => {
                tracing::warn!(target: "sandbox", %reason, "Job Object unavailable; this gate runs with the watchdog only");
                Ok((Isolation::Watchdog, HardenGuard::default()))
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (cmd, limits);
        Ok((Isolation::Watchdog, HardenGuard::default()))
    }
}

/// Windows Job Objects via raw Win32 (windows-sys). Written against the
/// windows-sys 0.61 signatures; exercised on a real Windows host by CI's
/// `windows` job (`.github/workflows/engine.yml`, `windows-latest`) on
/// every push, including the `windows_*` tests below.
#[cfg(windows)]
mod job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenThread, ResumeThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE, THREAD_SUSPEND_RESUME,
    };

    /// An owned Job Object handle. Closing it kills every process still in
    /// the job (JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE).
    pub struct Job(HANDLE);
    // SAFETY: a Win32 kernel handle may be used and closed from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: we own the handle; it is closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn last_error(what: &str) -> String {
        format!("{what}: {}", std::io::Error::last_os_error())
    }

    impl Job {
        pub fn create(max_procs: u32, mem_bytes: u64) -> Result<Job, String> {
            // SAFETY: null attributes and name create an anonymous job.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(last_error("CreateJobObjectW"));
            }
            let job = Job(handle);
            // SAFETY: plain-old-data struct; all-zero is a valid "no limits" value.
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_JOB_MEMORY
                | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
            info.BasicLimitInformation.ActiveProcessLimit = max_procs.max(1);
            info.JobMemoryLimit = usize::try_from(mem_bytes).unwrap_or(usize::MAX);
            // SAFETY: `info` is a valid, fully initialised struct of the size passed.
            let ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                return Err(last_error("SetInformationJobObject"));
            }
            Ok(job)
        }

        pub fn assign(&self, pid: u32) -> Result<(), String> {
            // SAFETY: plain handle-returning call; checked for null below.
            let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
            if process.is_null() {
                return Err(last_error("OpenProcess"));
            }
            // SAFETY: both handles are valid; the process handle is closed right after.
            let ok = unsafe { AssignProcessToJobObject(self.0, process) };
            let err = (ok == 0).then(|| last_error("AssignProcessToJobObject"));
            unsafe { CloseHandle(process) };
            err.map_or(Ok(()), Err)
        }

        pub fn terminate(&self) {
            // SAFETY: valid job handle; failure (job already empty) is harmless.
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    /// Resume every thread of a process created with CREATE_SUSPENDED
    /// (std's Command does not expose the primary thread handle).
    pub fn resume_process(pid: u32) -> Result<u32, String> {
        // SAFETY: snapshot of all threads; the handle is closed below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateToolhelp32Snapshot"));
        }
        // SAFETY: plain-old-data struct; dwSize is set as the API requires.
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = 0u32;
        // SAFETY: valid snapshot and a correctly sized entry.
        let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: handle checked for null and closed after use.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !thread.is_null() {
                    if unsafe { ResumeThread(thread) } != u32::MAX {
                        resumed += 1;
                    }
                    unsafe { CloseHandle(thread) };
                }
            }
            more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot) };
        if resumed == 0 {
            return Err(format!("no thread of process {pid} could be resumed"));
        }
        Ok(resumed)
    }
}

/// setrlimit inside `pre_exec`. A macro rather than a fn because the
/// resource parameter's C type differs between libc targets (glibc uses an
/// enum typedef, musl/BSD an int); the constants always match their target.
#[cfg(unix)]
macro_rules! set_rlimit {
    ($resource:expr, $value:expr) => {{
        let lim = libc::rlimit { rlim_cur: $value as libc::rlim_t, rlim_max: $value as libc::rlim_t };
        // SAFETY: plain syscall with a valid pointer to a stack value.
        #[allow(unused_unsafe)] // callers are already inside `unsafe`
        if unsafe { libc::setrlimit($resource, &lim) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }};
}
#[cfg(unix)]
use set_rlimit;

/// Tasks (threads) currently owned by this user — the quantity the kernel
/// compares RLIMIT_NPROC against on Linux.
#[cfg(target_os = "linux")]
fn current_user_tasks() -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    if uid == 0 {
        return None; // root bypasses RLIMIT_NPROC anyway
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        match entry.metadata() {
            Ok(m) if m.uid() == uid => {}
            _ => continue,
        }
        if let Ok(tasks) = std::fs::read_dir(entry.path().join("task")) {
            total += tasks.count() as u64;
        }
    }
    Some(total)
}

/// Processes owned by this user — what RLIMIT_NPROC counts on BSD/macOS.
#[cfg(all(unix, not(target_os = "linux")))]
fn current_user_tasks() -> Option<u64> {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    if uid == 0 {
        return None;
    }
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_user(sysinfo::UpdateKind::Always));
    Some(
        sys.processes()
            .values()
            .filter(|p| p.user_id().map(|u| **u == uid).unwrap_or(false))
            .count() as u64,
    )
}

/// The folders a gate may read and execute from, besides its own sandbox
/// (Linux, Landlock). Everything else is invisible to it: the user's home
/// folder, other mounts, other temp dirs. That's what keeps secrets on disk
/// (shell config, key files) out of reach of model-written code; the
/// environment is scrubbed separately.
///
/// - System folders that exist (`/usr`, `/bin`, `/lib*`, `/etc`, `/opt`,
///   `/proc`, `/sys`, `/dev`, `/var/cache`, `/nix`, `/snap`, `/gnu`).
/// - The Rust toolchain: `CARGO_HOME`, `RUSTUP_HOME` (default `~/.cargo`,
///   `~/.rustup`).
/// - The gate program's own folder, and — only when that folder is
///   conventionally named `bin` (a venv's or a toolchain's own layout,
///   e.g. `venv/bin/python`) — its parent too, never the home folder or
///   anything above it. A caller of `execute_and_validate` fully controls
///   `validation_command`, so this stays narrow: naming an arbitrary
///   binary elsewhere on PATH grants only that binary's own directory,
///   not a whole install tree, unless it actually sits in a `bin/` folder.
/// - [`GATE_READ_PATHS_ENV`], for toolchains elsewhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn gate_read_roots(bin: &str, env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = ["/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt", "/proc", "/sys", "/dev", "/nix", "/snap", "/gnu"]
        .iter()
        .map(PathBuf::from)
        .chain(std::iter::once(Path::new("/var").join("cache")))
        .collect();
    let home = env("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    let under_home = |name: &str| home.as_ref().map(|h| h.join(name));
    roots.extend(env("CARGO_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| under_home(".cargo")));
    roots.extend(env("RUSTUP_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| under_home(".rustup")));
    // Never the home folder, `/`, or anything above the home folder.
    let too_wide = |p: &Path| p.parent().is_none() || home.as_ref().is_some_and(|h| h.starts_with(p));
    if let Some(prog) = program_path(bin, env).and_then(|p| std::fs::canonicalize(p).ok()) {
        let dir = prog.parent().map(Path::to_path_buf);
        // The parent only widens for a conventional `bin/` layout (a venv,
        // ~/.cargo/bin, /usr/bin): that's the one case this exists for.
        // Any other binary a caller names (execute_and_validate takes an
        // arbitrary validation_command) grants only its own directory.
        let is_bin_dir = dir.as_ref().and_then(|d| d.file_name()).is_some_and(|n| n == "bin");
        let prefix = is_bin_dir.then(|| dir.as_ref().and_then(|d| d.parent()).map(Path::to_path_buf)).flatten();
        roots.extend(dir.into_iter().chain(prefix).filter(|p| !too_wide(p)));
    }
    if let Some(extra) = env(GATE_READ_PATHS_ENV) {
        // The same guard as the auto-discovered roots above: this is an
        // escape hatch for a toolchain elsewhere, never a way to widen a
        // gate's reach to the home folder (CLAUDE.md's own rule), whether
        // that's a deliberate mistake or an inherited/templated env var.
        for p in std::env::split_paths(&extra).filter(|p| !p.as_os_str().is_empty()) {
            if too_wide(&p) {
                tracing::warn!(target: "sandbox", path = %p.display(), "{GATE_READ_PATHS_ENV} names the home folder or above it; ignored");
            } else {
                roots.push(p);
            }
        }
    }
    roots.retain(|p| p.exists());
    roots.sort();
    roots.dedup();
    roots
}

/// Where `bin` is: itself when it names a path, else the first match on PATH.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn program_path(bin: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let p = Path::new(bin);
    if p.components().count() > 1 {
        return Some(p.to_path_buf());
    }
    std::env::split_paths(&env("PATH")?).map(|d| d.join(bin)).find(|c| c.is_file())
}

/// Linux Landlock via raw syscalls (no extra dependency; the ABI is three
/// syscalls and two structs).
#[cfg(target_os = "linux")]
mod landlock {
    use std::os::fd::{AsRawFd, OwnedFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    const CREATE_RULESET_VERSION: u32 = 1 << 0;
    const RULE_PATH_BENEATH: u32 = 1;

    const FS_EXECUTE: u64 = 1 << 0;
    const FS_WRITE_FILE: u64 = 1 << 1;
    const FS_READ_FILE: u64 = 1 << 2;
    const FS_READ_DIR: u64 = 1 << 3;
    const FS_REMOVE_DIR: u64 = 1 << 4;
    const FS_REMOVE_FILE: u64 = 1 << 5;
    const FS_MAKE_CHAR: u64 = 1 << 6;
    const FS_MAKE_DIR: u64 = 1 << 7;
    const FS_MAKE_REG: u64 = 1 << 8;
    const FS_MAKE_SOCK: u64 = 1 << 9;
    const FS_MAKE_FIFO: u64 = 1 << 10;
    const FS_MAKE_BLOCK: u64 = 1 << 11;
    const FS_MAKE_SYM: u64 = 1 << 12;
    const FS_REFER: u64 = 1 << 13; // ABI 2
    const FS_TRUNCATE: u64 = 1 << 14; // ABI 3
    const NET_BIND_TCP: u64 = 1 << 0; // ABI 4
    const NET_CONNECT_TCP: u64 = 1 << 1; // ABI 4

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
        handled_access_net: u64,
    }

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    /// A prepared ruleset: reads and execution only beneath the read roots
    /// (system folders and toolchains, see `gate_read_roots`) and the
    /// sandbox; every write-type right only in the sandbox (and device
    /// writes under /dev); TCP denied (ABI 4).
    pub struct Ruleset {
        fd: OwnedFd,
    }

    impl Ruleset {
        pub fn fd(&self) -> i32 {
            self.fd.as_raw_fd()
        }
    }

    fn abi_version() -> Result<i64, String> {
        // SAFETY: documented probe form: NULL attr, size 0, VERSION flag.
        let v = unsafe {
            libc::syscall(libc::SYS_landlock_create_ruleset, std::ptr::null::<RulesetAttr>(), 0usize, CREATE_RULESET_VERSION)
        };
        if v < 1 {
            return Err(format!("kernel reports no Landlock support ({})", std::io::Error::last_os_error()));
        }
        Ok(v)
    }

    pub fn prepare(sandbox_root: &Path, read_roots: &[std::path::PathBuf]) -> Result<Ruleset, String> {
        let abi = abi_version()?;
        let read_rights = FS_EXECUTE | FS_READ_FILE | FS_READ_DIR;

        let mut write_rights = FS_WRITE_FILE
            | FS_REMOVE_DIR
            | FS_REMOVE_FILE
            | FS_MAKE_CHAR
            | FS_MAKE_DIR
            | FS_MAKE_REG
            | FS_MAKE_SOCK
            | FS_MAKE_FIFO
            | FS_MAKE_BLOCK
            | FS_MAKE_SYM;
        if abi >= 2 {
            write_rights |= FS_REFER;
        }
        if abi >= 3 {
            write_rights |= FS_TRUNCATE;
        }
        let net = if abi >= 4 { NET_BIND_TCP | NET_CONNECT_TCP } else { 0 };

        let attr = RulesetAttr { handled_access_fs: write_rights | read_rights, handled_access_net: net };
        let size = if abi >= 4 { std::mem::size_of::<RulesetAttr>() } else { std::mem::size_of::<u64>() };
        // SAFETY: attr is a valid, initialised struct of at least `size` bytes.
        let fd = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, &attr as *const RulesetAttr, size, 0u32) };
        if fd < 0 {
            return Err(format!("landlock_create_ruleset: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: the kernel just handed us this fd; we own it exclusively.
        let ruleset = Ruleset { fd: unsafe { OwnedFd::from_raw_fd(fd as i32) } };

        add_rule(&ruleset, sandbox_root, write_rights | read_rights)?;
        for root in read_roots {
            // /dev/null, /dev/tty etc. Writing to device nodes is harmless and
            // many tools redirect there; creating/removing entries is not allowed.
            let extra = if root == Path::new("/dev") { FS_WRITE_FILE | if abi >= 3 { FS_TRUNCATE } else { 0 } } else { 0 };
            add_rule(&ruleset, root, read_rights | extra)?;
        }
        Ok(ruleset)
    }

    fn add_rule(ruleset: &Ruleset, path: &Path, rights: u64) -> Result<(), String> {
        let dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| format!("open {} for landlock rule: {e}", path.display()))?;
        let attr = PathBeneathAttr { allowed_access: rights, parent_fd: dir.as_raw_fd() };
        // SAFETY: valid ruleset fd, valid packed attr pointer.
        let rc = unsafe {
            libc::syscall(libc::SYS_landlock_add_rule, ruleset.fd(), RULE_PATH_BENEATH, &attr as *const PathBeneathAttr, 0u32)
        };
        if rc != 0 {
            return Err(format!("landlock_add_rule {}: {}", path.display(), std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Called in the forked child before exec. Async-signal-safe: two
    /// syscalls, no allocation.
    pub fn restrict_self(fd: i32) -> std::io::Result<()> {
        // SAFETY: plain syscalls on an inherited fd.
        unsafe {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::syscall(libc::SYS_landlock_restrict_self, fd, 0u32) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

/// Linux seccomp filter via raw BPF (no extra dependency). Landlock can
/// deny TCP but has no rule for UDP, so a gate could still send data out
/// in DNS queries. This filter makes `socket()` fail with EACCES for IPv4,
/// IPv6 and raw packet sockets (Unix sockets, which local tools use, stay
/// allowed), refuses io_uring (which can open sockets without `socket()`),
/// and refuses every syscall made through another ABI (x32, 32-bit), which
/// would otherwise skip the check.
#[cfg(target_os = "linux")]
mod seccomp {
    const RET_ALLOW: u32 = 0x7fff_0000;
    const RET_ERRNO: u32 = 0x0005_0000;
    const LD_W_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    const JEQ_K: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
    const JSET_K: u16 = 0x45; // BPF_JMP | BPF_JSET | BPF_K
    const RET_K: u16 = 0x06; // BPF_RET | BPF_K
    // struct seccomp_data: nr @0, arch @4, instruction_pointer @8, args @16.
    const OFF_NR: u32 = 0;
    const OFF_ARCH: u32 = 4;
    const OFF_ARG0_LO: u32 = 16; // little-endian: the low word of args[0]

    #[cfg(target_arch = "x86_64")]
    const ARCH: Option<u32> = Some(0xC000_003E);
    #[cfg(target_arch = "aarch64")]
    const ARCH: Option<u32> = Some(0xC000_00B7);
    #[cfg(target_arch = "riscv64")]
    const ARCH: Option<u32> = Some(0xC000_00F3);
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")))]
    const ARCH: Option<u32> = None;
    /// x32 syscalls on x86_64 carry this bit; they'd bypass the numbers below.
    const X32_BIT: u32 = 0x4000_0000;

    pub struct Filter {
        code: Vec<libc::sock_filter>,
    }

    fn stmt(code: u16, k: u32) -> libc::sock_filter {
        libc::sock_filter { code, jt: 0, jf: 0, k }
    }
    fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
        libc::sock_filter { code, jt, jf, k }
    }

    impl Filter {
        /// `None` on an architecture this filter doesn't know.
        pub fn no_inet() -> Option<Self> {
            let arch = ARCH?;
            let deny = RET_ERRNO | libc::EACCES as u32;
            let socket = u32::try_from(libc::SYS_socket).ok()?;
            let io_uring = u32::try_from(libc::SYS_io_uring_setup).ok()?;
            let code = vec![
                /* 0 */ stmt(LD_W_ABS, OFF_ARCH),
                /* 1 */ jump(JEQ_K, arch, 1, 0),
                /* 2 */ stmt(RET_K, deny), // another ABI
                /* 3 */ stmt(LD_W_ABS, OFF_NR),
                /* 4 */ jump(JSET_K, X32_BIT, 0, 1),
                /* 5 */ stmt(RET_K, deny), // x32
                /* 6 */ jump(JEQ_K, io_uring, 0, 1),
                /* 7 */ stmt(RET_K, deny),
                /* 8 */ jump(JEQ_K, socket, 1, 0),
                /* 9 */ stmt(RET_K, RET_ALLOW),
                /* 10 */ stmt(LD_W_ABS, OFF_ARG0_LO),
                /* 11 */ jump(JEQ_K, libc::AF_INET as u32, 4, 0),
                /* 12 */ jump(JEQ_K, libc::AF_INET6 as u32, 3, 0),
                /* 13 */ jump(JEQ_K, libc::AF_PACKET as u32, 2, 0),
                // A Unix socket reaches whatever listens on the machine —
                // dockerd, the session bus — and Landlock doesn't govern
                // connect(); the user's groups (docker …) can't be dropped
                // unprivileged. socketpair(), a separate syscall that the
                // interpreters use for their own child pipes, stays allowed.
                /* 14 */ jump(JEQ_K, libc::AF_UNIX as u32, 1, 0),
                /* 15 */ stmt(RET_K, RET_ALLOW),
                /* 16 */ stmt(RET_K, deny),
            ];
            Some(Self { code })
        }

        pub fn prog(&self) -> Prog {
            Prog(libc::sock_fprog { len: self.code.len() as u16, filter: self.code.as_ptr().cast_mut() })
        }
    }

    /// The kernel's view of a [`Filter`]: a length and a pointer into it.
    pub struct Prog(libc::sock_fprog);

    // SAFETY: the pointer targets a Filter's Vec that is never mutated and
    // outlives the child's exec (HardenGuard holds it); the kernel only
    // reads it, once, in the forked child.
    unsafe impl Send for Prog {}
    unsafe impl Sync for Prog {}

    /// Called in the forked child before exec. Async-signal-safe: two
    /// syscalls on memory prepared before the fork. NO_NEW_PRIVS is
    /// required for an unprivileged filter (Landlock sets it too).
    pub fn install(prog: &Prog) -> std::io::Result<()> {
        // SAFETY: `prog` points at a filter that outlives the exec (the
        // parent keeps it in HardenGuard); plain prctl calls.
        unsafe {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &prog.0 as *const libc::sock_fprog) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod isolation_reporting {
    use super::{isolation_for, Isolation};

    #[test]
    fn every_combination_of_kernel_layers_reports_correctly() {
        assert_eq!(isolation_for(true, true), Isolation::LandlockSeccomp);
        assert_eq!(isolation_for(true, false), Isolation::Landlock);
        // The one this fixes: seccomp alone (an old kernel with no
        // Landlock, or Landlock init failing for some other reason) must
        // not be under-reported as merely rlimits — real network denial
        // is in force and callers keyed off the isolation level need to
        // know that.
        assert_eq!(isolation_for(false, true), Isolation::Seccomp);
        assert_eq!(isolation_for(false, false), Isolation::Rlimits);
    }
}

// Linux only, like the Landlock rules these roots feed.
#[cfg(all(test, target_os = "linux"))]
mod read_roots {
    use super::gate_read_roots;
    use std::path::{Path, PathBuf};

    #[test]
    fn roots_hold_system_and_toolchain_folders_never_the_home_folder() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path().to_path_buf();
        for d in [".cargo", ".rustup", "bin", "tools/venv/bin"] {
            std::fs::create_dir_all(h.join(d)).unwrap();
        }
        let py = h.join("tools").join("venv").join("bin").join("python");
        std::fs::write(&py, "").unwrap();
        let mine = h.join("bin").join("mytool");
        std::fs::write(&mine, "").unwrap();
        let env_with = |extra: Option<String>| {
            let h = h.clone();
            move |k: &str| match k {
                "HOME" => Some(h.to_string_lossy().into_owned()),
                "PATH" => Some(h.join("bin").to_string_lossy().into_owned()),
                crate::config::GATE_READ_PATHS_ENV => extra.clone(),
                _ => None,
            }
        };
        let canon = |p: PathBuf| std::fs::canonicalize(p).unwrap();
        let roots = gate_read_roots(&py.to_string_lossy(), &env_with(None));
        assert!(roots.contains(&PathBuf::from("/usr")), "{roots:?}");
        assert!(roots.contains(&h.join(".cargo")) && roots.contains(&h.join(".rustup")), "the toolchain: {roots:?}");
        assert!(roots.contains(&canon(h.join("tools").join("venv"))), "a venv's prefix: {roots:?}");
        assert!(!roots.iter().any(|r| r == &h || h.starts_with(r) && r != Path::new("/")), "never the home folder or above it: {roots:?}");
        assert!(!roots.contains(&PathBuf::from("/")), "never the root");
        // A tool in ~/bin: its folder, but its prefix would be the home folder.
        let roots = gate_read_roots("mytool", &env_with(None));
        assert!(roots.contains(&canon(h.join("bin"))), "{roots:?}");
        assert!(!roots.contains(&canon(h.clone())), "~/bin's prefix is home: refused {roots:?}");
        let extra = gate_read_roots("mytool", &env_with(Some(h.join("tools").to_string_lossy().into_owned())));
        assert!(extra.contains(&h.join("tools")), "GATE_READ_PATHS_ENV adds folders: {extra:?}");
    }

    /// execute_and_validate takes an arbitrary `validation_command` from
    /// any caller, so naming a binary that happens to live somewhere else
    /// on PATH must grant only its own directory, not that directory's
    /// parent too — unless it's a conventional `bin/` layout (a venv, a
    /// toolchain), the one case the parent-widening exists for.
    #[test]
    fn only_a_conventional_bin_directory_widens_to_its_parent() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path().to_path_buf();
        std::fs::create_dir_all(h.join("stash")).unwrap();
        let odd = h.join("stash").join("oddtool");
        std::fs::write(&odd, "").unwrap();
        let env_with = |path_dir: &str| {
            let (h, path_dir) = (h.clone(), path_dir.to_string());
            move |k: &str| match k {
                "HOME" => Some(h.to_string_lossy().into_owned()),
                "PATH" => Some(path_dir.clone()),
                _ => None,
            }
        };
        let canon = |p: std::path::PathBuf| std::fs::canonicalize(p).unwrap();
        let roots = gate_read_roots("oddtool", &env_with(&h.join("stash").to_string_lossy()));
        assert!(roots.contains(&canon(h.join("stash"))), "its own directory: {roots:?}");
        assert!(!roots.contains(&canon(h.clone())), "but not the parent (\"stash\" isn't named bin): {roots:?}");
        // The same binary, in a directory literally named `bin`: now the
        // parent is included too (the venv/toolchain case).
        std::fs::create_dir_all(h.join("toolchain").join("bin")).unwrap();
        let conventional = h.join("toolchain").join("bin").join("oddtool");
        std::fs::write(&conventional, "").unwrap();
        let roots = gate_read_roots("oddtool", &env_with(&h.join("toolchain").join("bin").to_string_lossy()));
        assert!(roots.contains(&canon(h.join("toolchain").join("bin"))));
        assert!(roots.contains(&canon(h.join("toolchain"))), "a bin/ layout still widens to its parent: {roots:?}");
    }

    #[test]
    fn gate_read_paths_cannot_widen_to_the_home_folder_or_root() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path().to_path_buf();
        let env = |extra: String| {
            let h = h.clone();
            move |k: &str| match k {
                "HOME" => Some(h.to_string_lossy().into_owned()),
                "PATH" => None,
                crate::config::GATE_READ_PATHS_ENV => Some(extra.clone()),
                _ => None,
            }
        };
        // The home folder itself, and the root: both refused, whatever the user sets.
        let via_home = gate_read_roots("mytool", &env(h.to_string_lossy().into_owned()));
        assert!(!via_home.contains(&h), "the home folder must not be grantable via the env var: {via_home:?}");
        let via_root = gate_read_roots("mytool", &env("/".into()));
        assert!(!via_root.contains(&PathBuf::from("/")), "nor the root: {via_root:?}");
        // A real subfolder of home, requested explicitly, is still fine.
        let sub = h.join("toolchain");
        std::fs::create_dir_all(&sub).unwrap();
        let via_sub = gate_read_roots("mytool", &env(sub.to_string_lossy().into_owned()));
        assert!(via_sub.contains(&sub), "a named subfolder is a legitimate escape hatch: {via_sub:?}");
    }
}

#[cfg(test)]
mod timing {
    #[test]
    #[ignore = "measurement, run with --ignored --nocapture"]
    fn sample_cost() {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
        let mut sys = System::new();
        let n = |sys: &System| sys.processes().len();
        let t = std::time::Instant::now();
        for _ in 0..20 {
            super::sample_tree(&mut sys, std::process::id());
        }
        eprintln!("full sample (cpu+mem, all procs): {:.1} ms avg, {} procs", t.elapsed().as_secs_f64() * 1000.0 / 20.0, n(&sys));
        let t = std::time::Instant::now();
        for _ in 0..20 {
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().without_tasks());
        }
        eprintln!("listing only (parents, no cpu/mem): {:.1} ms avg", t.elapsed().as_secs_f64() * 1000.0 / 20.0);
    }
}
