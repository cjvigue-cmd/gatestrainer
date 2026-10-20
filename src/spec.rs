//! Gate specs and the built-in presets.

use crate::config::{
    bin_name, CARGO_BIN_ENV, GATE_DEFAULT_MAX_PROCS, GATE_DEFAULT_MEM_BYTES, GATE_DEFAULT_TIMEOUT_SECS, NODE_BIN_ENV, OPENSCAD_BIN_ENV,
    PYTHON_BIN_ENV,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The built-in gate presets: what kind of artifact is being checked.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    /// Generate Rust source code that must pass `cargo build`.
    RustCodegen,
    /// Generate OpenSCAD source that must pass `openscad` parse + render.
    OpenScadCodegen,
    /// Generate `solution.py` that must pass the caller's unittest tests.
    PythonCodegen,
    /// Generate `solution.mjs` that must pass the caller's `node --test` tests.
    JavaScriptCodegen,
    /// Free-form prose / summary that has no compilation gate.
    Prose,
}

/// How an artifact is proven to work: the files around it, the command
/// whose exit code decides, and the limits it runs under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateSpec {
    /// Where the model's code is written inside the sandbox (e.g. `src/main.rs`).
    pub entry_file: String,
    /// Fixed scaffolding written next to it (e.g. `Cargo.toml`, a test file).
    #[serde(default)]
    pub support_files: HashMap<String, String>,
    /// The command whose exit code is the verdict. Tokenised without a shell.
    /// `{{gatestrainer}}` stands for this program (e.g. `{{gatestrainer}} --check …`).
    pub validation_command: String,
    /// Further commands run after it, in order, in the same folder; the
    /// first that fails decides. Blueprints use them for several checks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_mem")]
    pub mem_limit_bytes: u64,
    #[serde(default = "default_procs")]
    pub max_procs: u32,
    /// Before the first attempt, check that the gate rejects an empty
    /// answer; one that accepts it can't tell right from wrong. Off only
    /// when empty really is a valid answer.
    #[serde(default = "default_canary")]
    pub canary: bool,
    /// What kind of proof the gate is, for the logs: a pass rate built
    /// on independent checks means more than one built on tests written
    /// from the same understanding as the spec.
    #[serde(default)]
    pub kind: GateKind,
    /// Checks of the answer's text alone, run in-process before the
    /// sandboxed gate (no process, no files): `placeholder`. A failure is
    /// the gate's failure, with that check's report.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prechecks: Vec<String>,
}

/// The prechecks a gate may name.
pub const PRECHECKS: [&str; 3] = ["placeholder", "claims", "secrets"];

/// What a gate proves. Logged with every task so pass rates can be split
/// by how much each rests on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    /// It builds (the rust preset): not that it's right.
    Compile,
    /// It renders (the openscad preset): not that it's the right shape.
    Render,
    /// Tests the caller wrote (the python / javascript presets, hidden
    /// tests). Written from the same reading as the spec, so a misread
    /// spec can reappear in them.
    Tests,
    /// A check independent of the spec's wording, declared by the caller:
    /// it measures the artifact itself (an STL's bounding box, a schema).
    Mechanical,
    /// A custom gate that didn't say.
    #[default]
    Custom,
}

impl GateKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Compile => "compile",
            Self::Render => "render",
            Self::Tests => "tests",
            Self::Mechanical => "mechanical",
            Self::Custom => "custom",
        }
    }
}

fn default_canary() -> bool {
    true
}

fn default_timeout() -> u64 {
    GATE_DEFAULT_TIMEOUT_SECS
}
fn default_mem() -> u64 {
    GATE_DEFAULT_MEM_BYTES
}
fn default_procs() -> u32 {
    GATE_DEFAULT_MAX_PROCS
}

impl GateSpec {
    /// Built-in presets for the common task types. `Prose` has no gate.
    /// Program names honour the `GATESTRAINER_*_BIN` overrides.
    pub fn preset(task_type: TaskType) -> Option<Self> {
        match task_type {
            TaskType::RustCodegen => {
                let cargo = bin_name(CARGO_BIN_ENV, "cargo");
                let mut support = HashMap::new();
                support.insert(
                    "Cargo.toml".to_string(),
                    "[package]\nname = \"sandbox\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n".to_string(),
                );
                Some(Self {
                    entry_file: "src/main.rs".into(),
                    support_files: support,
                    validation_command: format!("{} build --offline --quiet", quote(&cargo)),
                    steps: Vec::new(),
                    timeout_seconds: GATE_DEFAULT_TIMEOUT_SECS,
                    mem_limit_bytes: GATE_DEFAULT_MEM_BYTES,
                    max_procs: GATE_DEFAULT_MAX_PROCS,
                    canary: true,
                    kind: GateKind::Compile,
                    prechecks: vec!["placeholder".into(), "claims".into(), "secrets".into()],
                })
            }
            TaskType::OpenScadCodegen => {
                let openscad = bin_name(OPENSCAD_BIN_ENV, "openscad");
                Some(Self {
                    entry_file: "model.scad".into(),
                    support_files: HashMap::new(),
                    validation_command: format!("{} -o model.stl model.scad", quote(&openscad)),
                    steps: Vec::new(),
                    timeout_seconds: GATE_DEFAULT_TIMEOUT_SECS,
                    mem_limit_bytes: GATE_DEFAULT_MEM_BYTES,
                    max_procs: GATE_DEFAULT_MAX_PROCS,
                    canary: true,
                    kind: GateKind::Render,
                    prechecks: Vec::new(),
                })
            }
            TaskType::PythonCodegen => {
                let python = bin_name(PYTHON_BIN_ENV, if cfg!(windows) { "python" } else { "python3" });
                Some(Self {
                    entry_file: PYTHON_ENTRY.into(),
                    support_files: HashMap::new(),
                    // stdlib only: discovers test*.py; no tests at all is exit 5.
                    // `ran`: the answer's code runs in the test process, so exit 0
                    // alone could be its own `os._exit(0)`.
                    validation_command: format!("{{{{gatestrainer}}}} --check ran --match '^Ran [1-9][0-9]* tests? in ' --match '^OK' -- {} -m unittest -q", quote(&python)),
                    steps: Vec::new(),
                    timeout_seconds: GATE_DEFAULT_TIMEOUT_SECS,
                    mem_limit_bytes: GATE_DEFAULT_MEM_BYTES,
                    max_procs: GATE_DEFAULT_MAX_PROCS,
                    canary: true,
                    kind: GateKind::Tests,
                    prechecks: vec!["placeholder".into(), "claims".into(), "secrets".into()],
                })
            }
            TaskType::JavaScriptCodegen => {
                let node = bin_name(NODE_BIN_ENV, "node");
                Some(Self {
                    entry_file: JAVASCRIPT_ENTRY.into(),
                    support_files: HashMap::new(),
                    // Node's own runner: finds *.test.mjs and friends.
                    // A file reported as a test by itself exited before its tests.
                    validation_command: format!("{{{{gatestrainer}}}} --check ran --forbid '^✔ \\S+\\.test\\.[cm]?[jt]s\\b' -- {} --test", quote(&node)),
                    steps: Vec::new(),
                    timeout_seconds: GATE_DEFAULT_TIMEOUT_SECS,
                    mem_limit_bytes: GATE_DEFAULT_MEM_BYTES,
                    max_procs: GATE_DEFAULT_MAX_PROCS,
                    canary: true,
                    kind: GateKind::Tests,
                    prechecks: vec!["placeholder".into(), "claims".into(), "secrets".into()],
                })
            }
            TaskType::Prose => None,
        }
    }

    /// A preset with the caller's tests beside the model's code. Python
    /// and JavaScript only: their gates run tests and nothing else, so
    /// they need them (the Rust and OpenSCAD presets gate a build).
    ///
    /// # Errors
    /// Tests for a preset that doesn't run tests, or a test preset
    /// without them.
    pub fn preset_with_tests(task_type: TaskType, tests: Option<&str>) -> Result<Option<Self>, String> {
        let file = match task_type {
            TaskType::PythonCodegen => PYTHON_TESTS,
            TaskType::JavaScriptCodegen => JAVASCRIPT_TESTS,
            _ if tests.is_some() => return Err("`tests` goes with task_type python or javascript; for rust, pass a `gate` whose validation_command runs them".into()),
            _ => return Ok(Self::preset(task_type)),
        };
        let tests = tests.filter(|t| !t.trim().is_empty()).ok_or_else(|| {
            format!("task_type {} needs `tests`: they are written to {file} and must come from you, not the model under test", if file == PYTHON_TESTS { "python" } else { "javascript" })
        })?;
        let mut g = Self::preset(task_type).ok_or("no preset")?;
        g.support_files.insert(file.into(), tests.to_string());
        Ok(Some(g))
    }

    /// Every file for one attempt: the scaffolding plus the generated code.
    pub fn files_with(&self, code: &str) -> HashMap<String, String> {
        let mut files = self.support_files.clone();
        files.insert(self.entry_file.clone(), code.to_string());
        files
    }
}

/// Where the `python` preset writes the model's code (tests import `solution`).
pub const PYTHON_ENTRY: &str = "solution.py";
/// Where it writes the caller's tests.
pub const PYTHON_TESTS: &str = "test_solution.py";
/// Where the `javascript` preset writes the model's code (tests import `./solution.mjs`).
pub const JAVASCRIPT_ENTRY: &str = "solution.mjs";
/// Where it writes the caller's tests.
pub const JAVASCRIPT_TESTS: &str = "solution.test.mjs";

/// Quote a binary path for the gate command line (it may contain spaces,
/// e.g. `C:\Program Files\OpenSCAD\openscad.exe`).
fn quote(bin: &str) -> String {
    shlex::try_quote(bin).map(|c| c.into_owned()).unwrap_or_else(|_| bin.to_string())
}

