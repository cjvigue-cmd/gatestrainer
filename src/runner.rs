//! §15 — Generalized Execution Gates.
//!
//! Instead of hardcoded `validate_rust` / `validate_openscad`, a single
//! tool: `execute_and_validate`. The caller supplies a HashMap of files, a
//! validation command and a timeout; the files are written into an isolated
//! `tempfile::TempDir` and the command runs there under the sandbox in
//! [`super::sandbox`] (timeout, memory + process caps, rlimits, Landlock).
//! Only the exit code decides the verdict — never the model's own claim.
//!
//! The same function gates everything the Local seat produces, whoever
//! serves it. Nothing bypasses it.

use crate::sandbox::{run_sandboxed, split_command, SandboxLimits};
use crate::config::GATE_MAX_TIMEOUT_SECS;
use crate::error::{Error, Result};
use crate::events::{emit, ContainerExit, EngineEvent, EventSink, Isolation};
use crate::spec::GateSpec;
use crate::util::slugify;
use crate::verdict::{extract_reason, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;

/// One gate invocation.
#[derive(Debug, Clone)]
pub struct GateRequest {
    /// filename → contents, written relative to the sandbox root.
    pub files: HashMap<String, String>,
    /// e.g. `cargo build --offline`, `pytest -q`, `tsc --noEmit`.
    pub validation_command: String,
    /// Further commands, run in order after it in the same folder; the
    /// first that fails decides the verdict (blueprints, design/gates.md).
    pub then: Vec<String>,
    pub timeout_seconds: u64,
    pub mem_limit_bytes: u64,
    pub max_procs: u32,
    /// 1-based strike number (for the verdict).
    pub attempt: u8,
    pub max_strikes: u8,
    /// Used to name the persisted artifact dir (typically the prompt).
    /// Falls back to the validation command.
    pub artifact_label: Option<String>,
}

/// §12B — what happened to the validated files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ArtifactStatus {
    /// Files were written to this directory.
    Saved { dir: PathBuf },
    /// The gate passed but persisting failed. The code is still valid; the
    /// Conductor should tell the user it is only in memory.
    NotSaved { reason: String },
    /// The gate failed (nothing to persist) or no gate applied.
    NotApplicable,
}

/// The result of one `execute_and_validate` invocation. The sandbox dir is
/// already deleted when this is returned.
#[derive(Debug, Clone)]
pub struct GateResult {
    pub stdout: String,
    pub stderr: String,
    /// Exit code, or -1 if the process was killed.
    pub exit: i32,
    pub container_exit: ContainerExit,
    pub passed: bool,
    /// The deterministic §16A verdict.
    pub verdict: Verdict,
    pub isolation: Isolation,
    pub artifact: ArtifactStatus,
    pub elapsed_ms: u64,
    /// Peak resident memory of the whole process tree, as sampled.
    pub peak_rss_bytes: u64,
    /// A process ran in the sandbox (false: a precheck turned the answer
    /// down first, so `isolation` describes nothing that ran).
    pub ran: bool,
    /// Where the sandbox lived (already removed); lets tests prove cleanup.
    #[cfg(test)]
    pub sandbox_path: PathBuf,
}

impl GateResult {
    /// Delete the saved artifact: a pass that was then refused (a gate
    /// that passes an empty answer too) leaves nothing worth keeping.
    pub fn discard(&self) {
        if let ArtifactStatus::Saved { dir } = &self.artifact {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// The text to feed back to the driver on failure. Some tools (pytest,
    /// openscad in places) report errors on stdout, so fall back to it when
    /// stderr is empty.
    pub fn feedback(&self) -> String {
        combined_output(&self.stderr, &self.stdout)
    }
}

/// §15A — The generic execution gate.
///
/// Returns `Ok` whenever the command ran — pass, fail, timeout or limit kill
/// — with the verdict describing which. Returns `Err` only when the gate
/// itself could not be set up.
///
/// # Errors
/// - [`Error::SandboxError`] — empty/unbalanced command, hostile
///   filename (`..`, absolute), zero or excessive `timeout_seconds`, or a
///   sandbox setup failure.
/// - [`Error::DependencyMissing`] — the command's binary is not on PATH.
pub async fn execute_and_validate(req: GateRequest, sink: Option<&EventSink>) -> Result<GateResult> {
    if req.timeout_seconds == 0 || req.timeout_seconds > GATE_MAX_TIMEOUT_SECS {
        return Err(Error::SandboxError(format!(
            "timeout_seconds must be 1..={GATE_MAX_TIMEOUT_SECS}, got {}",
            req.timeout_seconds
        )));
    }
    // Every command is checked before any runs: a typo in step 3 shouldn't
    // cost steps 1 and 2.
    let commands: Vec<String> = std::iter::once(&req.validation_command).chain(&req.then).map(|c| with_program(c)).collect::<Result<_>>()?;
    let argvs: Vec<Vec<String>> = commands.iter().map(|c| split_command(c)).collect::<Result<_>>()?;

    // §15B — isolated temp dir; removed when `tempdir` drops at the end.
    let tempdir: TempDir = tempfile::tempdir()
        .map_err(|e| Error::SandboxError(format!("tempdir create: {e}")))?;
    let root: PathBuf = tempdir.path().to_path_buf();
    write_files(&root, &req.files)?;

    tracing::info!(
        target: "gate",
        cmd = %req.validation_command,
        timeout_s = req.timeout_seconds,
        files = req.files.len(),
        attempt = req.attempt,
        "execute_and_validate"
    );

    let limits = SandboxLimits {
        timeout: Duration::from_secs(req.timeout_seconds),
        mem_bytes: req.mem_limit_bytes,
        max_procs: req.max_procs,
        native_hardening: true,
    };
    // Each step gets the gate's own limits; the first failure stops it.
    let (mut elapsed, mut peak, mut passed_out) = (Duration::ZERO, 0u64, Vec::new());
    let mut out = None;
    for (i, argv) in argvs.iter().enumerate() {
        let mut step = run_sandboxed(&root, argv, limits, sink).await?;
        elapsed += step.elapsed;
        peak = peak.max(step.peak_rss_bytes);
        // Every gate runs in a fresh temp dir; its path in the output would make
        // two identical failures look different (the strike loop compares them)
        // and tells the model nothing.
        step.stdout = scrub_gate_dir(&step.stdout, &root);
        step.stderr = scrub_gate_dir(&step.stderr, &root);
        let last = i + 1 == argvs.len();
        if !step.success() && argvs.len() > 1 {
            // Which step, after its own output (the first line stays the reason).
            step.stderr = format!("{}\n(gate step {} of {}: {})", step.stderr.trim_end(), i + 1, argvs.len(), commands[i]);
        }
        if step.success() && !last {
            passed_out.push(step.stdout.trim_end().to_string());
            continue;
        }
        if step.success() && !passed_out.is_empty() {
            passed_out.push(step.stdout.trim_end().to_string());
            step.stdout = passed_out.join("\n") + "\n";
        }
        out = Some(step);
        break;
    }
    let Some(mut out) = out else {
        return Err(Error::SandboxError("a gate with no commands".into()));
    };
    out.elapsed = elapsed;
    out.peak_rss_bytes = peak;
    let passed = out.success();

    // §16A — deterministic verdict from the process outcome + output.
    let next_action = || {
        if req.attempt >= req.max_strikes {
            "StrikeOut — returning to the Conductor for judgement.".to_string()
        } else {
            format!("Feeding stderr back to the driver for Strike {}.", req.attempt + 1)
        }
    };
    let verdict = match out.exit {
        ContainerExit::Exited(0) => Verdict::passed(req.attempt, "Ok(String) returned upward — gate passed."),
        ContainerExit::TimedOut => Verdict::timeout(req.attempt, next_action()),
        ContainerExit::MemoryLimit => Verdict::failed(
            req.attempt,
            format!("memory limit exceeded ({} MiB)", req.mem_limit_bytes / (1024 * 1024)),
            next_action(),
        ),
        ContainerExit::ProcessLimit => Verdict::failed(
            req.attempt,
            format!("process limit exceeded ({} processes)", req.max_procs),
            next_action(),
        ),
        ContainerExit::Signaled | ContainerExit::Exited(_) => {
            Verdict::failed(req.attempt, extract_reason(&combined_output(&out.stderr, &out.stdout)), next_action())
        }
    };
    emit(sink, EngineEvent::Verdict(verdict.clone()));

    // §12B — on pass, persist the validated files outside the sandbox.
    let artifact = if passed {
        let label = req.artifact_label.as_deref().unwrap_or(&req.validation_command);
        match persist_validated_files(&req.files, label) {
            Ok(dir) => ArtifactStatus::Saved { dir },
            Err(e) => {
                tracing::warn!(target: "gate", err = %e, "could not persist validated files");
                ArtifactStatus::NotSaved { reason: e.to_string() }
            }
        }
    } else {
        ArtifactStatus::NotApplicable
    };

    drop(tempdir);
    Ok(GateResult {
        stdout: out.stdout,
        stderr: out.stderr,
        exit: out.exit_code,
        container_exit: out.exit,
        passed,
        verdict,
        isolation: out.isolation,
        artifact,
        elapsed_ms: out.elapsed.as_millis() as u64,
        peak_rss_bytes: out.peak_rss_bytes,
        ran: true,
        #[cfg(test)]
        sandbox_path: root,
    })
}

/// Stands in for this binary in a gate command, so a gate can call
/// `{{gatestrainer}} --check …` wherever this program is installed.
pub const PROGRAM_PLACEHOLDER: &str = "{{gatestrainer}}";

/// `cmd` with [`PROGRAM_PLACEHOLDER`] replaced by this binary's path, quoted.
fn with_program(cmd: &str) -> Result<String> {
    if !cmd.contains(PROGRAM_PLACEHOLDER) {
        return Ok(cmd.to_string());
    }
    let exe = std::env::current_exe().map_err(|e| Error::SandboxError(format!("{PROGRAM_PLACEHOLDER}: this program's path: {e}")))?;
    let exe = exe.to_string_lossy().into_owned();
    let quoted = shlex::try_quote(&exe).map_err(|e| Error::SandboxError(format!("{PROGRAM_PLACEHOLDER}: {exe}: {e}")))?;
    Ok(cmd.replace(PROGRAM_PLACEHOLDER, &quoted))
}

/// stderr followed by stdout. Test runners put the failing assertion on
/// stdout (`cargo test`, pytest) and only a generic "test failed" line on
/// stderr, so feeding back stderr alone hides the actual failure.
/// Stands in for the gate's temp dir in its output.
pub const GATE_DIR_PLACEHOLDER: &str = "<gate>";

/// `text` with the sandbox root (as given and as the OS resolves it, e.g.
/// macOS's `/private/var/…`) replaced by [`GATE_DIR_PLACEHOLDER`].
fn scrub_gate_dir(text: &str, root: &Path) -> String {
    let mut out = text.to_string();
    let canonical = std::fs::canonicalize(root).ok();
    for r in [Some(root.to_path_buf()), canonical].into_iter().flatten() {
        let r = r.to_string_lossy().into_owned();
        if !r.is_empty() {
            out = out.replace(&r, GATE_DIR_PLACEHOLDER);
        }
    }
    out
}

fn combined_output(stderr: &str, stdout: &str) -> String {
    match (stderr.trim().is_empty(), stdout.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => stderr.to_string(),
        (true, false) => stdout.to_string(),
        (false, false) => format!("{}\n--- stdout ---\n{}", stderr.trim_end(), stdout),
    }
}

fn write_files(root: &Path, files: &HashMap<String, String>) -> Result<()> {
    // PathBuf everywhere (§9); reject any filename that escapes the root.
    for (name, contents) in files {
        let p = join_safe(root, name)?;
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::SandboxError(format!("mkdir {}: {e}", parent.display())))?;
        }
        std::fs::write(&p, contents)
            .map_err(|e| Error::SandboxError(format!("write {}: {e}", p.display())))?;
    }
    Ok(())
}

/// Join `root` + `name` into a PathBuf, rejecting path-traversal attempts
/// (`..`, absolute paths). Fail loudly (§2) on anything hostile.
///
/// `pub(crate)` so the test module can exercise the sanitiser directly.
pub(crate) fn join_safe(root: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty() {
        return Err(Error::SandboxError("empty filename".into()));
    }
    let p = Path::new(name);
    if p.is_absolute() {
        return Err(Error::SandboxError(format!(
            "absolute filename rejected: {name}"
        )));
    }
    // Reject any component that is `..`.
    for c in p.components() {
        if matches!(c, std::path::Component::ParentDir) {
            return Err(Error::SandboxError(format!(
                "path traversal rejected: {name}"
            )));
        }
    }
    Ok(root.join(p))
}

/// §12B — write the validated files to `{scratchpad}/output/{slug}/`. The
/// slug derives from `label` (deterministic, collision-resistant via a
/// hash suffix). Returns the directory.
fn persist_validated_files(files: &HashMap<String, String>, label: &str) -> Result<PathBuf> {
    let out_dir: PathBuf = crate::util::output_dir()?.join(slugify(label));
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| Error::SandboxError(format!("mkdir {}: {e}", out_dir.display())))?;
    write_files(&out_dir, files)?;
    Ok(out_dir)
}

/// One gated attempt for `code` under `gate`: the prechecks on the text
/// alone first (no sandbox needed to see an unfinished answer), then the
/// sandboxed gate. `attempt` and `max_strikes` only shape the verdict's
/// wording for a retry loop; a single check passes 1 and 1.
///
/// # Errors
/// The sandbox or the gate's program could not be run ([`Error`]); a gate
/// that ran and failed is a `GateResult` with `passed: false`.
pub async fn run_gate(gate: &GateSpec, code: &str, attempt: u8, max_strikes: u8, label: &str, sink: Option<&EventSink>) -> Result<GateResult> {
    for p in &gate.prechecks {
        let report = match p.as_str() {
            "placeholder" => crate::check::placeholder::report(code, &gate.entry_file),
            "claims" => crate::check::claims::report(code, &gate.entry_file),
            "secrets" => crate::check::secrets::report(code, &gate.entry_file),
            _ => None,
        };
        if let Some(stderr) = report {
            let action = if attempt >= max_strikes { "StrikeOut — back to the caller for judgement.".to_string() } else { format!("Feeding stderr back for attempt {}.", attempt + 1) };
            let verdict = Verdict::failed(attempt, stderr.lines().next().unwrap_or_default(), action);
            emit(sink, EngineEvent::Verdict(verdict.clone()));
            return Ok(GateResult {
                stdout: String::new(),
                stderr,
                exit: 1,
                container_exit: ContainerExit::Exited(1),
                passed: false,
                verdict,
                isolation: Isolation::Watchdog,
                artifact: ArtifactStatus::NotApplicable,
                elapsed_ms: 0,
                peak_rss_bytes: 0,
                ran: false,
                #[cfg(test)]
                sandbox_path: std::path::PathBuf::new(),
            });
        }
    }
    execute_and_validate(
        GateRequest {
            files: gate.files_with(code),
            validation_command: gate.validation_command.clone(),
            then: gate.steps.clone(),
            timeout_seconds: gate.timeout_seconds,
            mem_limit_bytes: gate.mem_limit_bytes,
            max_procs: gate.max_procs,
            attempt,
            max_strikes,
            artifact_label: Some(label.to_string()),
        },
        sink,
    )
    .await
}

#[cfg(all(test, unix))]
mod tests {
    //! Exhaustive unit tests for the §15 Generic Execution Gate.
    //!
    //! These tests use real, universally-available CLI binaries (`true`,
    //! `false`, `echo`, `cat`, `sleep`) to exercise the sandboxing,
    //! file-writing, timeout enforcement, and verdict-extraction logic
    //! without needing cargo/openscad/pdftotext installed. They run on the
    //! primary Linux target + macOS. Windows CI would need
    //! conditional skips (the binaries differ).

    use super::*;

    #[tokio::test]
    async fn steps_run_in_order_and_the_first_failure_decides() {
        let _shared = EXCLUSIVE.read().await;
        let mut r = req(HashMap::from([("a.txt".to_string(), "x".to_string())]), "echo one", 10, 1, 3);
        r.then = vec!["echo two".into()];
        let g = execute_and_validate(r, None).await.unwrap();
        assert!(g.passed, "{}", g.stderr);
        assert_eq!(g.stdout, "one\ntwo\n", "every passing step's output");
        if let ArtifactStatus::Saved { dir } = &g.artifact {
            let _ = std::fs::remove_dir_all(dir);
        }
        let mut r = req(HashMap::from([("a.txt".to_string(), "x".to_string())]), "echo one", 10, 1, 3);
        r.then = vec!["cat a.txt".into(), "cat missing.txt".into(), "echo never".into()];
        let g = execute_and_validate(r, None).await.unwrap();
        assert!(!g.passed);
        assert!(g.stderr.contains("missing.txt"), "the failing step's own output: {}", g.stderr);
        assert!(g.stderr.trim_end().ends_with("(gate step 3 of 4: cat missing.txt)"), "{}", g.stderr);
        assert!(!g.stdout.contains("never"), "later steps don't run: {}", g.stdout);
        let mut r = req(HashMap::new(), "true", 10, 1, 3);
        r.then = vec!["echo \"unbalanced".into()];
        assert!(execute_and_validate(r, None).await.is_err(), "every command is checked before any runs");
    }

    #[test]
    fn the_program_placeholder_is_this_binary_quoted() {
        let cmd = with_program("{{gatestrainer}} --check count --pattern x --eq 0 a.txt").unwrap();
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        assert_eq!(split_command(&cmd).unwrap()[0], exe);
        assert_eq!(with_program("cargo test").unwrap(), "cargo test");
    }

    #[test]
    fn the_gate_dir_is_scrubbed_from_output() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("src").join("main.rs");
        let text = format!("error at {}:3:5", file.display());
        let scrubbed = scrub_gate_dir(&text, dir.path());
        assert!(scrubbed.starts_with(&format!("error at {GATE_DIR_PLACEHOLDER}")), "{scrubbed}");
        assert!(scrubbed.ends_with("main.rs:3:5"), "{scrubbed}");
        assert!(!scrubbed.contains(&dir.path().to_string_lossy().into_owned()));
    }
    use std::collections::HashMap;

    fn req(files: HashMap<String, String>, cmd: &str, timeout: u64, attempt: u8, max: u8) -> GateRequest {
        GateRequest {
            files,
            validation_command: cmd.into(),
            then: vec![],
            timeout_seconds: timeout,
            mem_limit_bytes: crate::config::GATE_DEFAULT_MEM_BYTES,
            max_procs: crate::config::GATE_DEFAULT_MAX_PROCS,
            attempt,
            max_strikes: max,
            artifact_label: Some(format!("gate-test-{}", uuid::Uuid::new_v4())),
        }
    }

    /// The resource-attack tests (fork bomb, memory hog) take this exclusively;
    /// every other gate test holds it shared. RLIMIT_NPROC counts the user's
    /// processes system-wide, so a bomb running beside a healthy gate makes
    /// the healthy gate's forks fail (see GATE_NPROC_HEADROOM).
    static EXCLUSIVE: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

    /// Run a gate and delete any persisted artifact so test runs don't
    /// accumulate output dirs in the real scratchpad.
    async fn run(files: HashMap<String, String>, cmd: &str, timeout: u64, attempt: u8, max: u8) -> Result<GateResult> {
        let _shared = EXCLUSIVE.read().await;
        let r = execute_and_validate(req(files, cmd, timeout, attempt, max), None).await;
        if let Ok(GateResult { artifact: ArtifactStatus::Saved { dir }, .. }) = &r {
            let _ = std::fs::remove_dir_all(dir);
        }
        r
    }

    /// Helper: build a single-file map.
    fn one_file(name: &str, contents: &str) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(name.into(), contents.into());
        m
    }

    // -------- join_safe sanitiser --------

    #[test]
    fn join_safe_rejects_empty_filename() {
        let root = std::env::temp_dir();
        let r = join_safe(&root, "");
        assert!(matches!(r, Err(Error::SandboxError(_))));
    }

    #[test]
    fn join_safe_rejects_absolute_path() {
        let root = std::env::temp_dir();
        let r = join_safe(&root, "/etc/passwd");
        assert!(matches!(r, Err(Error::SandboxError(s)) if s.contains("absolute")));
    }

    #[test]
    fn join_safe_rejects_parent_dir_traversal() {
        let root = std::env::temp_dir();
        let r = join_safe(&root, "../escape.rs");
        assert!(matches!(r, Err(Error::SandboxError(s)) if s.contains("traversal")));
    }

    #[test]
    fn join_safe_rejects_nested_parent_dir_traversal() {
        let root = std::env::temp_dir();
        // A sneaky traversal buried in a subdir.
        let r = join_safe(&root, "sub/../../escape.rs");
        assert!(matches!(r, Err(Error::SandboxError(s)) if s.contains("traversal")));
    }

    #[test]
    fn join_safe_allows_simple_filename() {
        let root = std::env::temp_dir();
        let r = join_safe(&root, "main.rs");
        assert!(r.is_ok());
        assert_eq!(r.unwrap().file_name().unwrap(), "main.rs");
    }

    #[test]
    fn join_safe_allows_nested_subdir_filename() {
        // Subdirs within the sandbox are fine — only `..` and absolute
        // paths are rejected.
        let root = std::env::temp_dir();
        let r = join_safe(&root, "src/main.rs");
        assert!(r.is_ok());
        let p = r.unwrap();
        assert!(p.ends_with("src/main.rs"));
    }

    // -------- execute_and_validate: success path --------

    #[tokio::test]
    async fn gate_passes_when_validation_command_exits_zero() {
        // `true` exits 0 on every POSIX system.
        let files = one_file("hello.txt", "hi");
        let r = run(files, "true", 10, 1, 3).await;
        let g = r.expect("gate should pass");
        assert!(g.passed);
        assert_eq!(g.exit, 0);
        assert_eq!(g.verdict.status, crate::verdict::VerdictStatus::Passed);
        assert!(g.verdict.reason.is_empty());
        assert!(g.verdict.action.contains("gate passed"));
    }

    #[tokio::test]
    async fn gate_writes_file_to_tempdir_before_validation() {
        // `cat hello.txt` only succeeds if the file was written first.
        let files = one_file("hello.txt", "world\n");
        let r = run(files, "cat hello.txt", 10, 1, 3).await;
        let g = r.expect("cat should succeed on a written file");
        assert!(g.passed);
        assert!(g.stdout.contains("world"));
    }

    #[tokio::test]
    async fn gate_writes_multiple_files() {
        let mut files = HashMap::new();
        files.insert("a.txt".into(), "alpha".into());
        files.insert("b.txt".into(), "beta".into());
        // `cat a.txt b.txt` succeeds only if BOTH files exist.
        let r = run(files, "cat a.txt b.txt", 10, 1, 3).await;
        let g = r.expect("cat should succeed on both files");
        assert!(g.passed);
        assert!(g.stdout.contains("alpha"));
        assert!(g.stdout.contains("beta"));
    }

    // -------- execute_and_validate: failure path --------

    #[tokio::test]
    async fn gate_fails_when_validation_command_exits_nonzero() {
        // `false` exits 1 on every POSIX system.
        let files = one_file("hello.txt", "hi");
        let r = run(files, "false", 10, 1, 3).await;
        let g = r.expect("gate returns Ok even on failure — verdict carries the reason");
        assert!(!g.passed);
        assert_eq!(g.verdict.status, crate::verdict::VerdictStatus::Failed);
        assert!(!g.verdict.reason.is_empty());
        // Strike 1 of 3 → retry action.
        assert!(g.verdict.action.contains("Strike 2"));
    }

    #[tokio::test]
    async fn gate_failure_on_final_strike_yields_strike_out() {
        let files = one_file("hello.txt", "hi");
        let r = run(files, "false", 10, 3, 3).await;
        let g = r.unwrap();
        assert!(!g.passed);
        // Strike 3 of 3 → StrikeOut: the task goes back to the Conductor.
        assert!(g.verdict.action.contains("StrikeOut"), "{}", g.verdict.action);
    }

    #[tokio::test]
    async fn gate_failure_extracts_reason_from_stderr() {
        // `sh -c 'echo "error: boom" >&2; exit 1'` — but we don't use sh.
        // Instead, use `false` (no stderr) and verify the verdict reason
        // is the fallback "gate produced no stderr" or similar.
        let files = one_file("hello.txt", "hi");
        let r = run(files, "false", 10, 1, 3).await;
        let g = r.unwrap();
        // `false` produces no stderr → extract_reason returns the fallback.
        assert!(!g.verdict.reason.is_empty());
    }

    // -------- execute_and_validate: timeout enforcement --------

    #[tokio::test]
    async fn gate_times_out_and_kills_child() {
        // `sleep 10` with a 1s timeout → must return a Timeout verdict.
        let files = one_file("hello.txt", "hi");
        let r = run(files, "sleep 10", 1, 1, 3).await;
        let g = r.expect("timeout returns Ok(GateResult) with a Timeout verdict");
        assert!(!g.passed);
        assert_eq!(g.verdict.status, crate::verdict::VerdictStatus::Timeout);
        assert_eq!(g.exit, -1);
        assert!(g.verdict.reason.contains("timed out"));
    }

    #[tokio::test]
    async fn gate_does_not_timeout_when_command_finishes_fast() {
        // `true` finishes instantly; a 1s timeout must NOT fire.
        let files = one_file("hello.txt", "hi");
        let r = run(files, "true", 5, 1, 3).await;
        let g = r.unwrap();
        assert!(g.passed);
        assert_ne!(g.verdict.status, crate::verdict::VerdictStatus::Timeout);
    }

    // -------- execute_and_validate: empty + missing command --------

    #[tokio::test]
    async fn gate_rejects_empty_validation_command() {
        let files = one_file("hello.txt", "hi");
        let r = run(files, "", 10, 1, 3).await;
        assert!(matches!(r, Err(Error::SandboxError(_))));
    }

    #[tokio::test]
    async fn gate_rejects_whitespace_only_validation_command() {
        let files = one_file("hello.txt", "hi");
        let r = run(files, "   ", 10, 1, 3).await;
        assert!(matches!(r, Err(Error::SandboxError(_))));
    }

    #[tokio::test]
    async fn gate_returns_dependency_missing_for_nonexistent_binary() {
        // A binary that definitely does not exist.
        let files = one_file("hello.txt", "hi");
        let r = run(files, "gatestrainer-nonexistent-binary-xyz-123", 10, 1, 3)
        .await;
        match r {
            Err(Error::DependencyMissing(name)) => {
                assert!(name.contains("gatestrainer-nonexistent-binary-xyz-123"));
            }
            other => panic!("expected DependencyMissing, got {other:?}"),
        }
    }

    // -------- execute_and_validate: sandboxing guarantees --------

    #[tokio::test]
    async fn gate_rejects_path_traversal_filename() {
        // A filename attempting to escape the tempdir.
        let mut files = HashMap::new();
        files.insert("../escape.txt".into(), "pwned".into());
        let r = run(files, "true", 10, 1, 3).await;
        assert!(matches!(r, Err(Error::SandboxError(s)) if s.contains("traversal")));
    }

    #[tokio::test]
    async fn gate_rejects_absolute_filename() {
        let mut files = HashMap::new();
        files.insert("/tmp/gatestrainer-absolute-test.txt".into(), "pwned".into());
        let r = run(files, "true", 10, 1, 3).await;
        assert!(matches!(r, Err(Error::SandboxError(s)) if s.contains("absolute")));
    }

    #[tokio::test]
    async fn gate_allows_nested_subdir_writes() {
        let files = one_file("src/main.rs", "fn main() {}");
        // `test -f src/main.rs` exits 0 only if the nested file exists.
        let r = run(files, "test -f src/main.rs", 10, 1, 3).await;
        let g = r.expect("nested file should be written + testable");
        assert!(g.passed);
    }

    // -------- execute_and_validate: tempdir auto-cleanup --------

    #[tokio::test]
    async fn gate_tempdir_is_removed_before_returning() {
        let g = run(one_file("hello.txt", "hi"), "true", 10, 1, 3).await.unwrap();
        assert!(!g.sandbox_path.exists(), "sandbox dir must be gone once the gate returns");
    }

    // -------- §12B artifact persistence --------

    #[tokio::test]
    async fn gate_pass_persists_files_and_reports_dir() {
        let g = execute_and_validate(req(one_file("src/lib.rs", "// ok"), "true", 10, 1, 3), None).await.unwrap();
        match &g.artifact {
            ArtifactStatus::Saved { dir } => {
                assert_eq!(std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(), "// ok");
                let _ = std::fs::remove_dir_all(dir);
            }
            other => panic!("expected Saved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn gate_failure_persists_nothing() {
        let g = run(one_file("a.txt", "x"), "false", 10, 1, 3).await.unwrap();
        assert_eq!(g.artifact, ArtifactStatus::NotApplicable);
    }

    // -------- argument validation --------

    #[tokio::test]
    async fn gate_rejects_zero_timeout() {
        let r = run(one_file("a.txt", "x"), "true", 0, 1, 3).await;
        assert!(matches!(r, Err(Error::SandboxError(s)) if s.contains("timeout_seconds")));
    }

    #[tokio::test]
    async fn gate_rejects_excessive_timeout() {
        let r = run(one_file("a.txt", "x"), "true", crate::config::GATE_MAX_TIMEOUT_SECS + 1, 1, 3).await;
        assert!(matches!(r, Err(Error::SandboxError(_))));
    }

    #[tokio::test]
    async fn gate_rejects_unbalanced_quotes() {
        let r = run(one_file("a.txt", "x"), "echo 'oops", 10, 1, 3).await;
        assert!(matches!(r, Err(Error::SandboxError(s)) if s.contains("quotes")));
    }

    #[tokio::test]
    async fn gate_honours_quoted_arguments() {
        let g = run(one_file("my file.txt", "spaced"), "cat 'my file.txt'", 10, 1, 3).await.unwrap();
        assert!(g.passed, "stderr: {}", g.stderr);
        assert!(g.stdout.contains("spaced"));
    }

    // -------- stderr/stdout feedback --------

    #[tokio::test]
    async fn gate_failure_reason_comes_from_stderr() {
        let g = run(one_file("a.txt", "x"), "ls definitely-missing-file", 10, 1, 3).await.unwrap();
        assert!(!g.passed);
        assert!(g.verdict.reason.contains("definitely-missing-file"), "{}", g.verdict.reason);
        assert!(g.feedback().contains("definitely-missing-file"));
    }

    // -------- §15B sandbox enforcement --------

    #[tokio::test]
    async fn gate_env_is_scrubbed_of_secrets() {
        // SAFETY: test-only; no other test reads this variable.
        unsafe { std::env::set_var("OPENROUTER_API_KEY", "sk-must-not-leak") };
        let g = run(one_file("a.txt", "x"), "env", 10, 1, 3).await.unwrap();
        assert!(g.passed);
        assert!(!g.stdout.contains("sk-must-not-leak"), "API key leaked into the sandbox env");
        assert!(g.stdout.contains("PATH="), "PATH must be passed through");
    }

    #[tokio::test]
    async fn gate_timeout_kills_grandchildren_too() {
        // The script backgrounds a long sleep, then sleeps itself. Killing
        // only the direct child would leave the grandchild running.
        let marker = format!("gatestrainer-grandchild-{}", uuid::Uuid::new_v4().simple());
        let script = format!("sleep 30 &\nexec -a {marker} sleep 30\n");
        let started = std::time::Instant::now();
        let g = run(one_file("s.sh", &script), "bash s.sh", 1, 1, 3).await.unwrap();
        assert_eq!(g.container_exit, ContainerExit::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let survivors = sys
            .processes()
            .values()
            .filter(|p| p.cmd().iter().any(|a| a.to_string_lossy().contains(&marker)))
            .count();
        assert_eq!(survivors, 0, "timed-out process tree must be fully killed");
    }

    #[tokio::test]
    async fn gate_kills_fork_bomb() {
        // Classic bash fork bomb, bounded so a broken sandbox can't take the
        // test machine down with it: each level forks twice, 12 levels deep.
        let script = "f() { [ \"$1\" -gt 12 ] && { sleep 5; return; }; f $(( $1 + 1 )) & f $(( $1 + 1 )) & wait; }\nf 0\n";
        let _exclusive = EXCLUSIVE.write().await;
        let mut r = req(one_file("bomb.sh", script), "bash bomb.sh", 20, 1, 3);
        r.max_procs = 32;
        let g = execute_and_validate(r, None).await.unwrap();
        assert!(!g.passed);
        assert!(
            matches!(
                g.container_exit,
                ContainerExit::ProcessLimit | ContainerExit::MemoryLimit | ContainerExit::Exited(_) | ContainerExit::Signaled
            ),
            "got {:?}",
            g.container_exit
        );
        assert_ne!(g.container_exit, ContainerExit::TimedOut, "fork bomb must be stopped before the timeout");
        eprintln!("fork bomb stopped by: {:?} (isolation {:?})", g.container_exit, g.isolation);
    }

    #[tokio::test]
    async fn gate_kills_memory_hog() {
        // `head -c` into a variable forces bash to hold ~400 MiB resident.
        let script = "x=$(head -c 400000000 /dev/zero | tr '\\0' 'a')\nsleep 5\n";
        let _exclusive = EXCLUSIVE.write().await;
        let mut r = req(one_file("hog.sh", script), "bash hog.sh", 30, 1, 3);
        r.mem_limit_bytes = 64 * 1024 * 1024;
        let g = execute_and_validate(r, None).await.unwrap();
        assert_eq!(g.container_exit, ContainerExit::MemoryLimit, "stderr: {}", g.stderr);
        assert!(g.verdict.reason.contains("memory limit"));
        assert!(g.stderr.contains("memory limit"));
    }

    /// What the portable watchdog alone (no rlimits, no process group, no
    /// Landlock — i.e. a Windows or macOS host) does against a fork bomb.
    /// Bounded to depth 10 so even a total watchdog failure cannot hurt the
    /// machine. Measured on Linux, 16 cores: the bomb reaches ~6k processes
    /// before the second scan completes, then dies on the memory cap after
    /// ~1.5 s with no survivors. Ignored by default: it deliberately lets a
    /// bomb grow. Run with `cargo test -- --ignored watchdog_alone --nocapture`.
    #[tokio::test]
    #[ignore = "measurement: deliberately lets a bounded fork bomb grow"]
    async fn watchdog_alone_contains_a_fork_bomb() {
        use crate::events::EngineEvent;
        use crate::sandbox::{run_sandboxed, SandboxLimits};
        let _exclusive = EXCLUSIVE.write().await;
        let dir = tempfile::tempdir().unwrap();
        let name = format!("bomb-{}.sh", uuid::Uuid::new_v4().simple());
        std::fs::write(
            dir.path().join(&name),
            "f() { [ \"$1\" -gt 10 ] && { sleep 5; return; }; f $(( $1 + 1 )) & f $(( $1 + 1 )) & wait; }\nf 0\n",
        )
        .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let limits = SandboxLimits {
            timeout: std::time::Duration::from_secs(20),
            mem_bytes: 1024 * 1024 * 1024,
            max_procs: 32,
            native_hardening: false,
        };
        let started = std::time::Instant::now();
        let out = run_sandboxed(dir.path(), &["bash".into(), name.clone()], limits, Some(&tx)).await.unwrap();
        let elapsed = started.elapsed();
        drop(tx);
        let (mut peak_procs, mut peak_rss, mut samples) = (0u32, 0u64, 0u32);
        while let Some(ev) = rx.recv().await {
            if let EngineEvent::ContainerSample { procs, rss_bytes, .. } = ev {
                samples += 1;
                peak_procs = peak_procs.max(procs);
                peak_rss = peak_rss.max(rss_bytes);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let survivors = sys
            .processes()
            .values()
            .filter(|p| p.cmd().iter().any(|a| a.to_string_lossy().contains(&name)))
            .count();
        eprintln!(
            "WATCHDOG-ONLY fork bomb: exit={:?} after {} ms, {} samples, peak sampled procs={}, peak sampled RSS={} MiB, survivors={}",
            out.exit,
            elapsed.as_millis(),
            samples,
            peak_procs,
            peak_rss / (1024 * 1024),
            survivors
        );
        assert_eq!(out.isolation, crate::events::Isolation::Watchdog);
        assert!(matches!(out.exit, ContainerExit::ProcessLimit | ContainerExit::MemoryLimit), "{:?}", out.exit);
        assert_eq!(survivors, 0, "every bomb process must be killed without a process group");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn gate_cannot_write_outside_sandbox_under_landlock() {
        let g = run(one_file("a.txt", "x"), "true", 10, 1, 3).await.unwrap();
        if !g.isolation.confines_files() {
            eprintln!("skipping: Landlock unavailable on this kernel");
            return;
        }
        let target = std::env::temp_dir().join(format!("gatestrainer-escape-{}", uuid::Uuid::new_v4().simple()));
        let cmd = format!("touch {}", target.display());
        let g = run(one_file("a.txt", "x"), &cmd, 10, 1, 3).await.unwrap();
        assert!(!g.passed, "write outside the sandbox must fail");
        assert!(!target.exists(), "escaped file was created: {}", target.display());
        let _ = std::fs::remove_file(&target);
    }

    /// Secrets on disk (shell config holding API keys, key files) stay out
    /// of reach: a gate reads only its sandbox, system folders and the
    /// toolchain. The secret here sits in another temp dir, as a file in the
    /// home folder would.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn gate_cannot_read_outside_its_sandbox_and_toolchain_under_landlock() {
        let g = run(one_file("a.txt", "x"), "true", 10, 1, 3).await.unwrap();
        if !g.isolation.confines_files() {
            eprintln!("skipping: Landlock unavailable on this kernel");
            return;
        }
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("fish_variables");
        std::fs::write(&secret, "SETUVAR --export SOME_API_KEY:sk-not-real\n").unwrap();
        let g = run(one_file("a.txt", "x"), &format!("cat {}", secret.display()), 10, 1, 3).await.unwrap();
        assert!(!g.passed, "reading outside the sandbox must fail");
        assert!(!g.stdout.contains("sk-not-real"), "the secret leaked: {}", g.stdout);
        let listing = run(one_file("a.txt", "x"), &format!("ls {}", outside.path().display()), 10, 1, 3).await.unwrap();
        assert!(!listing.passed && !listing.stdout.contains("fish_variables"), "nor list it: {}", listing.stdout);
        let own = run(one_file("a.txt", "inside"), "cat a.txt", 10, 1, 3).await.unwrap();
        assert!(own.passed && own.stdout.contains("inside"), "its own files still read: {}", own.stderr);
        // Keys in environment variables are safe too: another process's
        // /proc/<pid>/environ is outside the gate's Landlock domain.
        let environ = std::path::Path::new("/proc").join(std::process::id().to_string()).join("environ");
        let env_read = run(one_file("a.txt", "x"), &format!("cat {}", environ.display()), 10, 1, 3).await.unwrap();
        assert!(!env_read.passed, "another process's environment must not be readable: {}", env_read.stdout.len());
        let os_release = std::path::Path::new("/etc").join("os-release");
        let system = run(one_file("a.txt", "x"), &format!("cat {}", os_release.display()), 10, 1, 3).await.unwrap();
        assert!(system.passed, "system files still read: {}", system.stderr);
    }

    /// Landlock can't deny UDP, so a gate could send data out in DNS
    /// queries; the seccomp filter refuses IPv4 / IPv6 sockets outright.
    /// Unix sockets, which local tools use, still work.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn gate_cannot_open_udp_or_tcp_sockets_under_seccomp() {
        let g = run(one_file("a.txt", "x"), "true", 10, 1, 3).await.unwrap();
        if g.isolation != Isolation::LandlockSeccomp {
            eprintln!("skipping: seccomp filter not in force ({:?})", g.isolation);
            return;
        }
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            eprintln!("skipping: python3 missing");
            return;
        }
        let probe = r#"import socket
for fam, kind in [(socket.AF_INET, socket.SOCK_DGRAM), (socket.AF_INET6, socket.SOCK_DGRAM), (socket.AF_INET, socket.SOCK_STREAM)]:
    try:
        s = socket.socket(fam, kind)
        if kind == socket.SOCK_DGRAM:
            s.sendto(b"leak", ("127.0.0.1", 9))
        print("OPENED", fam, kind)
    except PermissionError:
        print("DENIED", fam, kind)
socket.socket(socket.AF_UNIX, socket.SOCK_STREAM).close()
print("UNIX OK")
"#;
        let g = run(one_file("probe.py", probe), "python3 probe.py", 20, 1, 3).await.unwrap();
        assert!(g.passed, "the probe itself runs: {}", g.stderr);
        assert!(!g.stdout.contains("OPENED"), "an inet socket opened: {}", g.stdout);
        assert_eq!(g.stdout.matches("DENIED").count(), 3, "{}", g.stdout);
        assert!(g.stdout.contains("UNIX OK"), "Unix sockets still work: {}", g.stdout);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn gate_can_write_inside_sandbox_under_landlock() {
        let g = run(one_file("a.txt", "x"), "mkdir -p out/deep", 10, 1, 3).await.unwrap();
        assert!(g.passed, "writes inside the sandbox must work: {}", g.stderr);
    }

    // -------- execute_and_validate: no shell wrapper (§9) --------

    #[tokio::test]
    async fn gate_does_not_invoke_shell_for_metacharacters() {
        // A command with shell metacharacters. Because we split on
        // whitespace and invoke `Command::new(bin)` directly (no `sh -c`),
        // `echo` receives `;` as a literal argument, not a separator.
        // `echo a;b` prints "a;b" (single arg), NOT "a" then runs "b".
        let files = one_file("hello.txt", "hi");
        let r = run(files, "echo a;b", 10, 1, 3).await;
        let g = r.expect("echo should succeed");
        assert!(g.passed);
        // The stdout should be "a;b" (the metacharacter is literal),
        // proving no shell wrapper interpreted it.
        assert!(g.stdout.contains("a;b"));
    }

    // -------- real toolchain presets (cargo is present wherever these tests run) --------

    fn preset_req(task: crate::spec::TaskType, code: &str) -> GateRequest {
        let spec = crate::spec::GateSpec::preset(task).unwrap();
        let mut r = req(spec.files_with(code), &spec.validation_command, spec.timeout_seconds, 1, 3);
        r.mem_limit_bytes = spec.mem_limit_bytes;
        r.max_procs = spec.max_procs;
        r
    }

    #[tokio::test]
    async fn rust_preset_passes_valid_code_inside_the_sandbox() {
        let _shared = EXCLUSIVE.read().await;
        let g = execute_and_validate(preset_req(crate::spec::TaskType::RustCodegen, "fn main() { println!(\"hi\"); }\n"), None)
            .await
            .unwrap();
        assert!(g.passed, "cargo build failed in sandbox ({:?}): {}", g.isolation, g.stderr);
        if let ArtifactStatus::Saved { dir } = &g.artifact {
            assert!(dir.join("src/main.rs").is_file());
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[tokio::test]
    async fn rust_preset_reports_compiler_error_as_reason() {
        let _shared = EXCLUSIVE.read().await;
        let g = execute_and_validate(preset_req(crate::spec::TaskType::RustCodegen, "fn main() { let x: i32 = y; }\n"), None)
            .await
            .unwrap();
        assert!(!g.passed);
        assert!(g.verdict.reason.starts_with("error[E0425]"), "reason: {}", g.verdict.reason);
        assert!(g.stderr.contains("cannot find value `y`"));
    }

    #[tokio::test]
    async fn openscad_preset_gates_real_geometry() {
        let _shared = EXCLUSIVE.read().await;
        let good = execute_and_validate(preset_req(crate::spec::TaskType::OpenScadCodegen, "cube([10, 20, 5]);\n"), None).await;
        let good = match good {
            Err(Error::DependencyMissing(_)) => {
                eprintln!("skipping: openscad not installed");
                return;
            }
            other => other.unwrap(),
        };
        assert!(good.passed, "openscad failed in sandbox ({:?}): {}", good.isolation, good.stderr);
        if let ArtifactStatus::Saved { dir } = &good.artifact {
            let _ = std::fs::remove_dir_all(dir);
        }
        let bad = execute_and_validate(preset_req(crate::spec::TaskType::OpenScadCodegen, "cube([10, 20, 5]\n"), None)
            .await
            .unwrap();
        assert!(!bad.passed, "a syntax error must fail the gate");
        assert!(!bad.verdict.reason.is_empty());
    }

    /// The caller's tests decide: right code passes, wrong code and an
    /// empty answer fail — for real, in the sandbox.
    async fn tests_preset_gates(task: crate::spec::TaskType, tests: &str, right: &str, wrong: &str) {
        let spec = crate::spec::GateSpec::preset_with_tests(task, Some(tests)).unwrap().unwrap();
        let run = |code: &str| {
            let mut r = req(spec.files_with(code), &spec.validation_command, spec.timeout_seconds, 1, 3);
            r.mem_limit_bytes = spec.mem_limit_bytes;
            r.max_procs = spec.max_procs;
            execute_and_validate(r, None)
        };
        let good = match run(right).await {
            Err(Error::DependencyMissing(m)) => {
                eprintln!("skipping: {m}");
                return;
            }
            other => other.unwrap(),
        };
        assert!(good.passed, "{task:?} failed in sandbox ({:?}): {}", good.isolation, good.stderr);
        for code in [wrong, ""] {
            let bad = run(code).await.unwrap();
            assert!(!bad.passed, "{task:?} passed {code:?}");
        }
        if let ArtifactStatus::Saved { dir } = &good.artifact {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[tokio::test]
    async fn python_preset_runs_the_callers_unittest_tests() {
        let _shared = EXCLUSIVE.read().await;
        let tests = "import unittest\nfrom solution import add\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n";
        tests_preset_gates(crate::spec::TaskType::PythonCodegen, tests, "def add(a, b):\n    return a + b\n", "def add(a, b):\n    return a - b\n").await;
    }

    #[tokio::test]
    async fn javascript_preset_runs_the_callers_node_tests() {
        let _shared = EXCLUSIVE.read().await;
        let tests = "import { test } from 'node:test';\nimport assert from 'node:assert/strict';\nimport { add } from './solution.mjs';\ntest('add', () => assert.equal(add(2, 3), 5));\n";
        tests_preset_gates(crate::spec::TaskType::JavaScriptCodegen, tests, "export function add(a, b) { return a + b; }\n", "export function add(a, b) { return a - b; }\n").await;
    }

    #[test]
    fn test_presets_need_tests_and_other_presets_refuse_them() {
        use crate::spec::{GateSpec, TaskType};
        assert!(GateSpec::preset_with_tests(TaskType::PythonCodegen, None).unwrap_err().contains("needs `tests`"));
        assert!(GateSpec::preset_with_tests(TaskType::JavaScriptCodegen, Some("  ")).is_err(), "blank tests are no tests");
        assert!(GateSpec::preset_with_tests(TaskType::RustCodegen, Some("x")).unwrap_err().contains("gate"));
        assert!(GateSpec::preset_with_tests(TaskType::RustCodegen, None).unwrap().is_some());
        let g = GateSpec::preset_with_tests(TaskType::PythonCodegen, Some("t")).unwrap().unwrap();
        assert_eq!(g.support_files.get(crate::spec::PYTHON_TESTS).map(String::as_str), Some("t"));
        assert_eq!(g.entry_file, crate::spec::PYTHON_ENTRY);
    }
}

/// Windows-only checks of the Job Object layer, using only tools every
/// Windows install has (hostname, ping, powershell). Written and
/// type-checked on Linux (x86_64-pc-windows-gnu) but not yet run on Windows:
/// run them with `cargo test windows_` on a Windows machine.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::events::Isolation;

    fn req(files: HashMap<String, String>, cmd: &str, timeout: u64, max_procs: u32) -> GateRequest {
        GateRequest {
            files,
            validation_command: cmd.into(),
            then: vec![],
            timeout_seconds: timeout,
            mem_limit_bytes: crate::config::GATE_DEFAULT_MEM_BYTES,
            max_procs,
            attempt: 1,
            max_strikes: 3,
            artifact_label: Some(format!("win-test-{}", uuid::Uuid::new_v4())),
        }
    }

    fn one_file(name: &str, contents: &str) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(name.to_string(), contents.to_string());
        m
    }

    fn cleanup(g: &GateResult) {
        if let ArtifactStatus::Saved { dir } = &g.artifact {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    fn count_named(name: &str) -> usize {
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        sys.processes().values().filter(|p| p.name().to_string_lossy().eq_ignore_ascii_case(name)).count()
    }

    #[tokio::test]
    async fn windows_gate_runs_under_a_job_object() {
        let g = execute_and_validate(req(one_file("a.txt", "x"), "hostname", 30, 8), None).await.unwrap();
        assert!(g.passed, "stderr: {}", g.stderr);
        assert_eq!(g.isolation, Isolation::JobObject, "expected the Job Object layer to be active");
        assert!(!g.stdout.trim().is_empty());
        cleanup(&g);
    }

    #[tokio::test]
    async fn windows_job_caps_a_process_storm_and_leaves_no_survivors() {
        let before = count_named("PING.EXE");
        // Try to start 60 long-lived pings; the job allows 8 live processes.
        let script = "1..60 | ForEach-Object { Start-Process -NoNewWindow -FilePath ping -ArgumentList '-n','30','127.0.0.1' }\nStart-Sleep -Seconds 2\n";
        let g = execute_and_validate(
            req(one_file("storm.ps1", script), "powershell -NoProfile -ExecutionPolicy Bypass -File storm.ps1", 20, 8),
            None,
        )
        .await
        .unwrap();
        assert_eq!(g.isolation, Isolation::JobObject);
        assert!(g.stderr.len() + g.stdout.len() > 0, "the capped Start-Process calls should have reported errors");
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(count_named("PING.EXE") <= before, "job close must kill every ping it started");
        cleanup(&g);
    }

    #[tokio::test]
    async fn windows_timeout_kills_the_whole_tree() {
        let before = count_named("PING.EXE");
        let script = "Start-Process -NoNewWindow -FilePath ping -ArgumentList '-n','60','127.0.0.1'\nStart-Sleep -Seconds 60\n";
        let started = std::time::Instant::now();
        let g = execute_and_validate(
            req(one_file("slow.ps1", script), "powershell -NoProfile -ExecutionPolicy Bypass -File slow.ps1", 2, 8),
            None,
        )
        .await
        .unwrap();
        assert_eq!(g.container_exit, ContainerExit::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(15));
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(count_named("PING.EXE") <= before, "the grandchild ping must be killed with the tree");
    }
}
