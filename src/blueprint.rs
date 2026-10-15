//! Gate blueprints (design/gates.md §4): a gate described as data — fixed
//! files, named inputs and steps — that expands into a [`GateSpec`] per
//! task. An outside author writes a JSON file, never Rust.
//!
//! Built-in blueprints are compiled in (`blueprints/*.json`); the user's own
//! are plain JSON files in `<config dir>/blueprints/`. Every rule is checked
//! when a blueprint loads, so a bad one is refused before any model call.

use crate::spec::{GateKind, GateSpec};
use crate::config::{bin_name, BLUEPRINTS_ENV, CARGO_BIN_ENV, GATE_DEFAULT_MAX_PROCS, GATE_DEFAULT_MEM_BYTES, GATE_DEFAULT_TIMEOUT_SECS, GATE_MAX_TIMEOUT_SECS, NODE_BIN_ENV, PYTHON_BIN_ENV};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

/// How many blueprints are built in (they load first).
pub const BUILT_IN_COUNT: usize = BUILT_IN.len();

const BUILT_IN: &[&str] = &[
    include_str!("../blueprints/rust-tests.json"),
    include_str!("../blueprints/python-tests.json"),
    include_str!("../blueprints/javascript-tests.json"),
    include_str!("../blueprints/json-schema.json"),
    include_str!("../blueprints/csv-table.json"),
    include_str!("../blueprints/math-answer.json"),
    include_str!("../blueprints/date-answer.json"),
    include_str!("../blueprints/python-refactor.json"),
    include_str!("../blueprints/javascript-refactor.json"),
    include_str!("../blueprints/python-tests-for.json"),
    include_str!("../blueprints/javascript-tests-for.json"),
    include_str!("../blueprints/rust-tests-for.json"),
    include_str!("../blueprints/markdown-docs.json"),
    include_str!("../blueprints/rewrite-text.json"),
    include_str!("../blueprints/summarize.json"),
    include_str!("../blueprints/cited-answer.json"),
    include_str!("../blueprints/sql-query.json"),
    include_str!("../blueprints/regex.json"),
];

/// The kinds of task a blueprint's `pick` may claim, for a router that
/// suggests a blueprint from a request. `none` = no blueprint fits.
pub const PICK_KINDS: [&str; 13] = ["code", "refactor", "tests", "sql", "regex", "docs", "json", "csv", "math", "rewrite", "summary", "cited", "none"];
/// The languages a `pick` may name. `none` = no programming language involved.
pub const PICK_LANGUAGES: [&str; 4] = ["rust", "python", "javascript", "none"];

/// Placeholders that name a program, not an input. `gatestrainer` is this program
/// (resolved when the gate runs); the others resolve like the presets do.
const PROGRAMS: [&str; 4] = ["gatestrainer", "cargo", "python", "node"];

/// What a pass means (design/gates.md §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proves {
    /// Correct against an independent check.
    Full,
    /// Necessary conditions only: never report the output as verified.
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    /// Text, written to the file named by `to`.
    File,
    /// Text, put where `{{name}}` appears (quoted as one argument in a step).
    Text,
    /// A finite number, put where `{{name}}` appears.
    Number,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub kind: InputKind,
    /// What the caller should give, in a sentence.
    pub about: String,
    /// `file` inputs: where the text is written in the gate's folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// It comes from the caller, not the model under test (hidden tests, a
    /// schema, an expected value). A `full` blueprint needs at least one.
    #[serde(default)]
    pub independent: bool,
    /// Used when the caller leaves it out; no default = required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Blueprint {
    pub name: String,
    /// The task type it serves (design/gates.md §2).
    pub task: String,
    pub about: String,
    pub proves: Proves,
    /// Where the model's answer is written.
    pub entry_file: String,
    /// Added to the task's prompt: what the gate expects of the answer's form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Fixed files; `{{name}}` in them takes a text or number input as is.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, Input>,
    /// Commands run in order; the first to fail decides. No shell.
    pub steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    /// Where Laya's suggestion lands on it (`triage_prompt` with
    /// `suggest_blueprint`): a task kind and, for code, a language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pick: Option<PickSlot>,
    /// False answers planted after a pass, in order of preference (the
    /// first that applies to the answer): `plant::KINDS`. design/gates.md §7.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plants: Vec<String>,
    /// Checks of the answer's text, run in-process before the gate's steps
    /// (`tools::PRECHECKS`): `placeholder` for code that must be finished.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prechecks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickSlot {
    /// One of `triage::PICK_KINDS` (not `none`).
    pub kind: String,
    /// One of `triage::PICK_LANGUAGES` (not `none`); absent = any language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// A blueprint expanded for one task.
#[derive(Debug, Clone)]
pub struct Expanded {
    pub gate: GateSpec,
    pub proves: Proves,
    pub instructions: Option<String>,
    /// The blueprint's `plants`.
    pub plants: Vec<String>,
}

fn placeholder() -> Result<Regex, String> {
    Regex::new(r"\{\{([A-Za-z_][A-Za-z0-9_]*)\}\}").map_err(|e| e.to_string())
}

/// A path inside the gate folder: relative, no `..`, not empty.
fn inside(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)))
}

impl Blueprint {
    /// Parse and check every rule (design/gates.md §4).
    ///
    /// # Errors
    /// Each broken rule, by name.
    pub fn parse(text: &str) -> Result<Self, String> {
        let bp: Self = serde_json::from_str(text).map_err(|e| format!("not a blueprint: {e}"))?;
        bp.check().map_err(|e| format!("blueprint {}: {e}", bp.name))?;
        Ok(bp)
    }

    fn check(&self) -> Result<(), String> {
        if self.name.is_empty() || !self.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err("`name` is lowercase letters, digits and dashes".into());
        }
        if self.steps.is_empty() {
            return Err("no `steps`".into());
        }
        if !inside(&self.entry_file) {
            return Err(format!("entry_file {:?} must be a relative path inside the gate folder", self.entry_file));
        }
        if self.timeout_seconds.is_some_and(|t| t == 0 || t > GATE_MAX_TIMEOUT_SECS) {
            return Err(format!("timeout_seconds must be 1..={GATE_MAX_TIMEOUT_SECS}"));
        }
        let mut paths = vec![self.entry_file.as_str()];
        for path in self.files.keys() {
            if !inside(path) {
                return Err(format!("file {path:?} must be a relative path inside the gate folder"));
            }
            paths.push(path);
        }
        for (name, input) in &self.inputs {
            if PROGRAMS.contains(&name.as_str()) {
                return Err(format!("input `{name}` would hide the {{{{{name}}}}} program placeholder"));
            }
            match (input.kind, &input.to) {
                (InputKind::File, Some(to)) if inside(to) => paths.push(to),
                (InputKind::File, _) => return Err(format!("file input `{name}` needs `to`, a relative path inside the gate folder")),
                (_, Some(_)) => return Err(format!("input `{name}`: only file inputs take `to`")),
                _ => {}
            }
            if let Some(d) = &input.default {
                value_of(name, input, d)?;
            }
        }
        paths.sort_unstable();
        if let Some(w) = paths.windows(2).find(|w| w[0] == w[1]) {
            return Err(format!("{:?} is written twice (entry_file, files and file inputs must differ)", w[0]));
        }
        // Rule 3: every placeholder names a declared input (or a program, in
        // a step), and every input is used.
        let re = placeholder()?;
        let mut used: Vec<&str> = self.inputs.iter().filter(|(_, i)| i.kind == InputKind::File).map(|(n, _)| n.as_str()).collect();
        for (place, text, programs_ok) in self.steps.iter().map(|s| ("a step", s, true)).chain(self.files.iter().map(|(p, t)| (p.as_str(), t, false))) {
            for cap in re.captures_iter(text) {
                let name = cap.get(1).map_or("", |m| m.as_str());
                if PROGRAMS.contains(&name) {
                    if !programs_ok {
                        return Err(format!("{{{{{name}}}}} is for steps, not file {place}"));
                    }
                    continue;
                }
                match self.inputs.get(name) {
                    Some(i) if i.kind == InputKind::File && !programs_ok => {
                        return Err(format!("file input `{name}` can't be pasted into {place}; make it a text input"))
                    }
                    Some(_) => used.push(self.inputs.get_key_value(name).map_or(name, |(k, _)| k.as_str())),
                    None => return Err(format!("{place} uses {{{{{name}}}}}, which isn't a declared input")),
                }
            }
        }
        if let Some(unused) = self.inputs.keys().find(|k| !used.contains(&k.as_str())) {
            return Err(format!("input `{unused}` is never used"));
        }
        if let Some(p) = &self.pick {
            if p.kind == "none" || !PICK_KINDS.contains(&p.kind.as_str()) {
                return Err(format!("pick.kind {:?}: one of {}", p.kind, PICK_KINDS[..PICK_KINDS.len() - 1].join(", ")));
            }
            if let Some(l) = p.language.as_deref().filter(|l| *l == "none" || !PICK_LANGUAGES.contains(l)) {
                return Err(format!("pick.language {l:?}: one of {}", PICK_LANGUAGES[..PICK_LANGUAGES.len() - 1].join(", ")));
            }
        }
        if let Some(k) = self.prechecks.iter().find(|k| !crate::spec::PRECHECKS.contains(&k.as_str())) {
            return Err(format!("prechecks: {k:?} isn't one of {}", crate::spec::PRECHECKS.join(", ")));
        }
        if let Some(k) = self.plants.iter().find(|k| !crate::plant::KINDS.contains(&k.as_str())) {
            return Err(format!("plants: {k:?} isn't one of {}", crate::plant::KINDS.join(", ")));
        }
        // Rule 2: a full blueprint rests on something the model didn't write.
        if self.proves == Proves::Full && !self.inputs.values().any(|i| i.independent) {
            return Err("`proves: full` needs an independent input (hidden tests, an expected value …)".into());
        }
        Ok(())
    }

    /// What the logs call this gate: measured by the checks alone, or
    /// resting on the caller's tests.
    fn kind(&self) -> GateKind {
        if self.steps.iter().all(|s| s.trim_start().starts_with("{{gatestrainer}} --check ")) {
            GateKind::Mechanical
        } else if self.inputs.values().any(|i| i.independent) {
            GateKind::Tests
        } else {
            GateKind::Custom
        }
    }

    /// Fill the inputs in.
    ///
    /// # Errors
    /// An unknown or missing input, or one of the wrong kind.
    pub fn expand(&self, given: &serde_json::Map<String, Value>) -> Result<Expanded, String> {
        if let Some(extra) = given.keys().find(|k| !self.inputs.contains_key(*k)) {
            return Err(format!("blueprint {} has no input `{extra}`; its inputs: {}", self.name, self.input_names()));
        }
        let mut values: HashMap<&str, String> = HashMap::new();
        let mut support = HashMap::new();
        for (name, input) in &self.inputs {
            let v = given.get(name).filter(|v| !v.is_null()).or(input.default.as_ref()).ok_or_else(|| {
                format!("blueprint {} needs `{name}`: {}", self.name, input.about)
            })?;
            let v = value_of(name, input, v).map_err(|e| format!("blueprint {}: {e}", self.name))?;
            match (&input.kind, &input.to) {
                (InputKind::File, Some(to)) => {
                    support.insert(to.clone(), v);
                    values.insert(name, to.clone());
                }
                _ => {
                    values.insert(name, v);
                }
            }
        }
        let re = placeholder()?;
        let program = |name: &str| match name {
            "cargo" => Some(bin_name(CARGO_BIN_ENV, "cargo")),
            "python" => Some(bin_name(PYTHON_BIN_ENV, if cfg!(windows) { "python" } else { "python3" })),
            "node" => Some(bin_name(NODE_BIN_ENV, "node")),
            _ => None,
        };
        let fill = |text: &str, quote: bool| -> String {
            re.replace_all(text, |cap: &regex::Captures| {
                let name = cap.get(1).map_or("", |m| m.as_str());
                if name == "gatestrainer" {
                    // The gate runner resolves it to this binary.
                    return cap[0].to_string();
                }
                let raw = program(name).or_else(|| values.get(name).cloned()).unwrap_or_default();
                if quote {
                    // One argument, whatever it contains: a value can't add flags.
                    shlex::try_quote(&raw).map(|q| q.into_owned()).unwrap_or(raw)
                } else {
                    raw
                }
            })
            .into_owned()
        };
        for (path, text) in &self.files {
            support.insert(path.clone(), fill(text, false));
        }
        let mut steps = self.steps.iter().map(|s| fill(s, true));
        let first = steps.next().ok_or_else(|| format!("blueprint {}: no steps", self.name))?;
        Ok(Expanded {
            gate: GateSpec {
                entry_file: self.entry_file.clone(),
                support_files: support,
                validation_command: first,
                steps: steps.collect(),
                timeout_seconds: self.timeout_seconds.unwrap_or(GATE_DEFAULT_TIMEOUT_SECS),
                mem_limit_bytes: GATE_DEFAULT_MEM_BYTES,
                max_procs: GATE_DEFAULT_MAX_PROCS,
                // Rule 1: a blueprint can't turn the empty-answer canary off.
                canary: true,
                kind: self.kind(),
                prechecks: self.prechecks.clone(),
            },
            proves: self.proves,
            instructions: self.instructions.clone(),
            plants: self.plants.clone(),
        })
    }

    fn input_names(&self) -> String {
        if self.inputs.is_empty() {
            return "none".into();
        }
        self.inputs
            .iter()
            .map(|(n, i)| if i.default.is_some() { format!("{n} (optional)") } else { n.clone() })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// One line per input, for listings.
    /// The programs its gate runs besides this binary (`cargo`, `python`,
    /// `node`), read from its steps: `{{cargo}}`, `{{python}}`, `{{node}}`,
    /// and the interpreter a `--engine` names (or that input's default, so
    /// regex needs python unless a task picks another engine). For the
    /// doctor: which blueprints a missing tool stops.
    pub fn tools(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for step in &self.steps {
            for (tool, mark) in [("cargo", "{{cargo}}"), ("python", "{{python}}"), ("node", "{{node}}")] {
                if step.contains(mark) && !out.contains(&tool) {
                    out.push(tool);
                }
            }
            let words: Vec<&str> = step.split_whitespace().collect();
            for w in words.windows(2).filter(|w| w[0] == "--engine") {
                let engine = match w[1].strip_prefix("{{").and_then(|r| r.strip_suffix("}}")) {
                    Some(input) => self.inputs.get(input).and_then(|i| i.default.as_ref()).and_then(Value::as_str).unwrap_or_default().to_string(),
                    None => w[1].to_string(),
                };
                let tool = match engine.as_str() {
                    "python" => "python",
                    "javascript" => "node",
                    _ => continue,
                };
                if !out.contains(&tool) {
                    out.push(tool);
                }
            }
        }
        out
    }

    pub fn summary(&self) -> Value {
        serde_json::json!({
            "name": self.name, "task": self.task, "about": self.about, "proves": self.proves,
            "entry_file": self.entry_file,
            "pick": self.pick,
            "inputs": self.inputs.iter().map(|(n, i)| (n.clone(), serde_json::json!({
                "kind": i.kind, "about": i.about, "required": i.default.is_none(), "independent": i.independent, "default": i.default,
            }))).collect::<serde_json::Map<_, _>>(),
        })
    }
}

/// An input's value as text, checked against its kind.
fn value_of(name: &str, input: &Input, v: &Value) -> Result<String, String> {
    match (input.kind, v) {
        (InputKind::Number, Value::Number(n)) if n.as_f64().is_some_and(f64::is_finite) => Ok(n.to_string()),
        (InputKind::Number, Value::String(s)) if s.trim().parse::<f64>().is_ok_and(f64::is_finite) => Ok(s.trim().to_string()),
        (InputKind::Number, _) => Err(format!("`{name}` must be a finite number")),
        (_, Value::String(s)) => Ok(s.clone()),
        _ => Err(format!("`{name}` must be a string")),
    }
}

/// The blueprint for Laya's pick: the first (built-ins come first) whose
/// slot has this kind and this language, or any language.
pub fn for_pick<'a>(all: &'a [Blueprint], kind: &str, language: &str) -> Option<&'a Blueprint> {
    all.iter().find(|b| b.pick.as_ref().is_some_and(|p| p.kind == kind && p.language.as_deref().is_none_or(|l| l == language)))
}


/// Where the user's own blueprints live: the folder `GATESTRAINER_BLUEPRINTS` names.
pub fn user_dir(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    env(BLUEPRINTS_ENV).filter(|v| !v.trim().is_empty()).map(PathBuf::from)
}

/// Every blueprint: built-in first, then the user's. A user file that
/// doesn't load, or reuses a built-in name, is reported, never used.
pub fn load_all(dir: Option<&Path>) -> (Vec<Blueprint>, Vec<String>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for text in BUILT_IN {
        match Blueprint::parse(text) {
            Ok(bp) => out.push(bp),
            Err(e) => problems.push(format!("built-in {e}")),
        }
    }
    let Some(dir) = dir.filter(|d| d.is_dir()) else { return (out, problems) };
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect(),
        Err(e) => {
            problems.push(format!("{}: {e}", dir.display()));
            return (out, problems);
        }
    };
    files.sort();
    for path in files {
        let loaded = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| Blueprint::parse(&t));
        match loaded {
            Ok(bp) if out.iter().any(|b| b.name == bp.name) => {
                problems.push(format!("{}: the name `{}` is taken (built-in or an earlier file); not loaded", path.display(), bp.name))
            }
            Ok(bp) => out.push(bp),
            Err(e) => problems.push(format!("{}: {e}", path.display())),
        }
    }
    (out, problems)
}

/// The blueprint called `name`, from the built-ins and `dir`.
///
/// # Errors
/// No such blueprint (with the names there are, and any file that failed to load).
pub fn find(name: &str, dir: Option<&Path>) -> Result<Blueprint, String> {
    let (all, problems) = load_all(dir);
    let names: Vec<&str> = all.iter().map(|b| b.name.as_str()).collect();
    match all.iter().find(|b| b.name == name) {
        Some(b) => Ok(b.clone()),
        None => Err(format!(
            "no blueprint `{name}`; there are: {}{}",
            names.join(", "),
            if problems.is_empty() { String::new() } else { format!(" (not loaded: {})", problems.join("; ")) }
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> serde_json::Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn built_ins_all_load() {
        let (all, problems) = load_all(None);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(all.len(), BUILT_IN.len());
    }

    #[test]
    fn expansion_quotes_values_into_steps_and_pastes_them_into_files() {
        let bp = find("math-answer", None).expect("built in");
        let e = bp.expand(&obj(json!({ "expect": 3.5, "rel": "1e-6" }))).expect("expand");
        assert_eq!(e.proves, Proves::Full);
        assert_eq!(e.gate.kind, GateKind::Mechanical);
        assert!(e.gate.validation_command.starts_with("{{gatestrainer}} --check numeric"), "{}", e.gate.validation_command);
        assert!(e.gate.validation_command.contains("--expect 3.5"), "{}", e.gate.validation_command);
        assert!(e.gate.validation_command.contains("--rel 1e-6"), "{}", e.gate.validation_command);
        assert!(e.instructions.is_some());
        let r = find("rust-tests", None).expect("built in").expand(&obj(json!({ "tests": "#[test] fn t() {}" }))).expect("expand");
        assert_eq!(r.gate.support_files["tests/hidden.rs"], "use task::*;\n#[test] fn t() {}");
        assert_eq!(r.gate.kind, GateKind::Tests);
        assert!(!r.gate.validation_command.contains("{{"), "{}", r.gate.validation_command);
        let c = find("csv-table", None).expect("built in").expand(&obj(json!({ "columns": "a:int,b:str --max-rows 0" }))).expect("expand");
        assert!(c.gate.validation_command.contains("'a:int,b:str --max-rows 0'"), "a value stays one argument: {}", c.gate.validation_command);
        assert_eq!(c.proves, Proves::Partial);
    }

    #[test]
    fn expansion_refuses_unknown_missing_and_mistyped_inputs() {
        let bp = find("math-answer", None).expect("built in");
        assert!(bp.expand(&obj(json!({}))).is_err_and(|e| e.contains("needs `expect`")));
        assert!(bp.expand(&obj(json!({ "expect": 1, "tolerance": 2 }))).is_err_and(|e| e.contains("no input `tolerance`")));
        assert!(bp.expand(&obj(json!({ "expect": "abc" }))).is_err_and(|e| e.contains("finite number")));
        let py = find("python-tests", None).expect("built in");
        assert!(py.expand(&obj(json!({ "tests": 5 }))).is_err());
        let e = py.expand(&obj(json!({ "tests": "import unittest" }))).expect("expand");
        assert_eq!(e.gate.support_files["test_solution.py"], "import unittest");
    }

    fn bp(v: Value) -> Result<Blueprint, String> {
        Blueprint::parse(&v.to_string())
    }

    fn base() -> Value {
        json!({ "name": "t", "task": "x", "about": "x", "proves": "full", "entry_file": "out.txt",
                "inputs": { "want": { "kind": "number", "about": "x", "independent": true } },
                "steps": ["{{gatestrainer}} --check numeric --file out.txt --pattern \"(\\\\d+)\" --expect {{want}}"] })
    }

    #[test]
    fn the_rules_are_checked_at_load() {
        assert!(bp(base()).is_ok());
        let with = |path: &str, v: Value| {
            let mut b = base();
            b[path] = v;
            bp(b)
        };
        // Rule 1: no canary opt-out (unknown field).
        assert!(with("canary", json!(false)).is_err());
        // Rule 2: full needs an independent input.
        assert!(with("inputs", json!({ "want": { "kind": "number", "about": "x" } })).is_err_and(|e| e.contains("independent")));
        assert!(with("proves", json!("partial")).is_ok());
        // Rule 3: placeholders and inputs match.
        assert!(with("steps", json!(["{{gatestrainer}} --check count --pattern x --eq {{n}} out.txt"])).is_err_and(|e| e.contains("{{n}}")));
        assert!(with("steps", json!(["{{gatestrainer}} --check count --pattern x --eq 1 out.txt"])).is_err_and(|e| e.contains("never used")));
        assert!(with("files", json!({ "a.txt": "{{gatestrainer}}" })).is_err(), "program placeholders are for steps");
        // Paths stay inside and don't collide.
        assert!(with("entry_file", json!("../x")).is_err());
        assert!(with("files", json!({ "out.txt": "x" })).is_err_and(|e| e.contains("twice")));
        assert!(with("steps", json!([])).is_err());
        assert!(with("name", json!("Bad Name")).is_err());
        assert!(with("timeout_seconds", json!(0)).is_err());
        assert!(bp(json!({ "name": "t", "task": "x", "about": "x", "proves": "partial", "entry_file": "o",
            "inputs": { "s": { "kind": "file", "about": "x" } }, "steps": ["true"] })).is_err_and(|e| e.contains("`to`")));
        assert!(bp(json!({ "name": "t", "task": "x", "about": "x", "proves": "partial", "entry_file": "o",
            "inputs": { "gatestrainer": { "kind": "text", "about": "x" } }, "steps": ["x {{gatestrainer}}"] })).is_err());
    }

    #[test]
    fn every_pick_slot_has_one_built_in() {
        let (all, _) = load_all(None);
        for (kind, lang, want) in [
            ("code", "rust", Some("rust-tests")),
            ("tests", "python", Some("python-tests-for")),
            ("refactor", "javascript", Some("javascript-refactor")),
            ("refactor", "rust", None),
            ("summary", "python", Some("summarize")),
            ("docs", "none", Some("markdown-docs")),
            ("sql", "none", Some("sql-query")),
            ("regex", "javascript", Some("regex")),
            ("none", "none", None),
        ] {
            assert_eq!(for_pick(&all, kind, lang).map(|b| b.name.as_str()), want, "{kind}/{lang}");
        }
        assert!(bp(json!({ "name": "t", "task": "x", "about": "x", "proves": "partial", "entry_file": "o", "steps": ["true"], "pick": { "kind": "poetry" } })).is_err());
        assert!(bp(json!({ "name": "t", "task": "x", "about": "x", "proves": "partial", "entry_file": "o", "steps": ["true"], "pick": { "kind": "code", "language": "cobol" } })).is_err());
    }

    #[test]
    fn each_blueprint_names_the_tools_its_gate_runs() {
        let tools = |n: &str| find(n, None).expect(n).tools();
        assert_eq!(tools("rust-tests-for"), ["cargo"]);
        assert_eq!(tools("python-tests"), ["python"]);
        assert_eq!(tools("sql-query"), ["python"]);
        assert_eq!(tools("javascript-refactor"), ["node"]);
        assert_eq!(tools("regex"), ["python"], "its engine's default");
        assert!(tools("math-answer").is_empty() && tools("cited-answer").is_empty(), "this binary's own checks");
    }


    #[test]
    fn user_blueprints_load_beside_built_ins_and_cannot_replace_them() {
        let d = tempfile::tempdir().expect("tempdir");
        let mut mine = base();
        mine["name"] = json!("mine");
        std::fs::write(d.path().join("mine.json"), mine.to_string()).expect("write");
        let mut clash = base();
        clash["name"] = json!("rust-tests");
        std::fs::write(d.path().join("clash.json"), clash.to_string()).expect("write");
        std::fs::write(d.path().join("broken.json"), "{").expect("write");
        std::fs::write(d.path().join("notes.txt"), "ignored").expect("write");
        let (all, problems) = load_all(Some(d.path()));
        assert_eq!(all.len(), BUILT_IN.len() + 1);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(find("mine", Some(d.path())).is_ok());
        assert!(find("rust-tests", Some(d.path())).is_ok_and(|b| b.proves == Proves::Full && b.entry_file == "src/lib.rs"));
        assert!(find("nope", Some(d.path())).is_err_and(|e| e.contains("mine") && e.contains("not loaded")));
    }
}
