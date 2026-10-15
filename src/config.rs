//! Limits and environment names. Every tunable the sandbox and the gate
//! runner read is here, so a reader can see the whole contract at once.

/// Default wall-clock timeout for a gate command.
pub const GATE_DEFAULT_TIMEOUT_SECS: u64 = 120;
/// Hard ceiling on any caller-supplied timeout.
pub const GATE_MAX_TIMEOUT_SECS: u64 = 900;
/// Default memory limit for the gate's whole process tree (resident set).
pub const GATE_DEFAULT_MEM_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Default cap on live processes in the gate's process tree.
pub const GATE_DEFAULT_MAX_PROCS: u32 = 64;
/// Extra kernel tasks granted above the user's current count via
/// RLIMIT_NPROC: a last-ditch backstop against a fork bomb that outruns
/// the watchdog's sampling. The per-gate cap is the watchdog's
/// `max_procs`, not this. Too low and a concurrent fork bomb makes a
/// healthy build fail; too high and the bomb starves a concurrent linker.
pub const GATE_NPROC_HEADROOM: u64 = 1024;
/// Largest single file the gate may write (RLIMIT_FSIZE).
pub const GATE_MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
/// Captured stdout/stderr is truncated beyond this many bytes per stream.
pub const GATE_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Watchdog sampling interval.
pub const GATE_SAMPLE_INTERVAL_MS: u64 = 100;
/// How much of a failing gate's stderr goes into the feedback text.
pub const FEEDBACK_STDERR_MAX_BYTES: usize = 6_000;

/// Set to "1" to refuse to run gates when kernel filesystem isolation
/// (Landlock) is unavailable, instead of degrading to rlimits + watchdog.
pub const REQUIRE_FS_ISOLATION_ENV: &str = "GATESTRAINER_REQUIRE_FS_ISOLATION";
/// Extra folders a gate may read (Linux), in the OS's path-list form
/// (`:`-separated): a toolchain installed outside the system folders.
pub const GATE_READ_PATHS_ENV: &str = "GATESTRAINER_GATE_READ_PATHS";
/// Where validated artifacts are kept (default: the temp dir's
/// `gatestrainer/output`).
pub const OUT_DIR_ENV: &str = "GATESTRAINER_OUT";
/// Where records that outlive a run are kept, such as the plant record
/// (default: the temp dir's `gatestrainer`).
pub const RECORDS_DIR_ENV: &str = "GATESTRAINER_RECORDS";
/// A folder of the user's own blueprints (`*.json`), beside the built-in ones.
pub const BLUEPRINTS_ENV: &str = "GATESTRAINER_BLUEPRINTS";

/// Overrides the `cargo` the rust preset runs.
pub const CARGO_BIN_ENV: &str = "GATESTRAINER_CARGO_BIN";
/// Overrides the `openscad` the openscad preset runs.
pub const OPENSCAD_BIN_ENV: &str = "GATESTRAINER_OPENSCAD_BIN";
/// Overrides the Python the python preset and the `regex` check run.
pub const PYTHON_BIN_ENV: &str = "GATESTRAINER_PYTHON_BIN";
/// Overrides the Node.js the javascript preset and the `regex` check run.
pub const NODE_BIN_ENV: &str = "GATESTRAINER_NODE_BIN";

/// Environment variables passed through to gate processes. Everything else
/// (API keys included) is scrubbed.
pub const GATE_ENV_PASSTHROUGH: &[&str] = &[
    "PATH", "HOME", "USER", "LANG", "LC_ALL", "TERM",
    "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN",
    // Windows essentials: without these many tools cannot even start.
    "SYSTEMROOT", "SystemRoot", "WINDIR", "PATHEXT", "COMSPEC",
    "USERPROFILE", "APPDATA", "LOCALAPPDATA", "PROGRAMDATA",
];

/// A program's name from the environment, else the default. The OS PATH
/// resolves it; no path is ever hardcoded.
pub fn bin_name(env_var: &str, default: &str) -> String {
    std::env::var(env_var).ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| default.to_string())
}
