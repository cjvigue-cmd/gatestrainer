//! What a gate reports while it runs.
//!
//! The runner holds no state, but it reports what it is doing while it does
//! it: it takes an optional [`EventSink`] and pushes [`EngineEvent`]s into
//! it; the caller owns the receiving end. Dropping the receiver is harmless;
//! emits become no-ops.

use crate::verdict::Verdict;
use serde::Serialize;

/// Sending half of the event stream. Unbounded so a slow renderer can never
/// stall a gate or a driver call.
pub type EventSink = tokio::sync::mpsc::UnboundedSender<EngineEvent>;

/// Push `ev` into `sink` if there is one. A closed receiver is not an error:
/// the Conductor may have quit mid-task, and the tool's `Result` is still the
/// authoritative outcome.
pub fn emit(sink: Option<&EventSink>, ev: EngineEvent) {
    if let Some(s) = sink {
        let _ = s.send(ev);
    }
}


/// How strongly a sandboxed process was isolated. Reported with every
/// container so the user can see what protection actually applied on their
/// OS/kernel instead of assuming the strongest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// Watchdog (timeout, memory, process count) + scrubbed env only.
    Watchdog,
    /// Watchdog + kernel rlimits (fork-bomb / file-size / CPU caps).
    Rlimits,
    /// Watchdog + rlimits + Landlock: reads confined to the sandbox, system
    /// folders and toolchains; writes to the sandbox; TCP bind/connect
    /// denied where the kernel supports it.
    Landlock,
    /// Landlock + a seccomp filter: no IPv4 / IPv6 / packet sockets at all
    /// (so no UDP either: DNS can't carry data out) and no io_uring.
    LandlockSeccomp,
    /// Watchdog + rlimits + the seccomp filter, but no Landlock (an old
    /// kernel: seccomp has existed since 3.5, Landlock only since 5.13).
    /// No filesystem confinement, but the network is still denied.
    Seccomp,
    /// Windows: watchdog + a Job Object (kernel per-tree process cap,
    /// job-wide memory limit, kill-on-close). No filesystem confinement.
    JobObject,
}

impl Isolation {
    /// Filesystem confinement applied (Landlock, with or without seccomp).
    pub fn confines_files(self) -> bool {
        matches!(self, Self::Landlock | Self::LandlockSeccomp)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Watchdog => "watchdog",
            Self::Rlimits => "rlimits",
            Self::Landlock => "landlock",
            Self::LandlockSeccomp => "landlock+seccomp",
            Self::Seccomp => "seccomp",
            Self::JobObject => "job_object",
        }
    }
}

/// Why a sandboxed process stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerExit {
    /// Exited on its own with the given code.
    Exited(i32),
    /// Killed after exceeding its wall-clock timeout.
    TimedOut,
    /// Killed after its process tree exceeded the memory limit.
    MemoryLimit,
    /// Killed after its process tree exceeded the process-count limit.
    ProcessLimit,
    /// Terminated by a signal we did not send (e.g. SIGXCPU from RLIMIT_CPU).
    Signaled,
}

/// One event from the execution layer.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EngineEvent {
    /// A sandboxed process was spawned.
    ContainerStarted {
        id: u64,
        command: String,
        timeout_ms: u64,
        mem_limit_bytes: u64,
        max_procs: u32,
        isolation: Isolation,
    },
    /// Periodic resource sample for a running container (whole process tree).
    ContainerSample {
        id: u64,
        cpu_pct: f32,
        rss_bytes: u64,
        procs: u32,
        elapsed_ms: u64,
    },
    /// The container stopped.
    ContainerFinished {
        id: u64,
        exit: ContainerExit,
        elapsed_ms: u64,
        peak_rss_bytes: u64,
    },
    /// §16A verdict for one gated attempt.
    Verdict(Verdict),
}
