//! `--blueprint list | test`: for blueprint authors. `test` loads a
//! blueprint (every load-time rule), fills in a case file's inputs, and
//! runs the real gate — same sandbox, same `{{gatestrainer}}` — on an empty
//! answer, the right answers and the wrong ones. A gate is only as good as
//! what it rejects, so a case file needs at least one of each.
//!
//! A case file (JSON):
//! `{"inputs": {...}, "good": ["answer", ...], "bad": ["answer", ...]}`.
//! Any string in it may instead be `{"file": "path"}`, read relative to the
//! case file, so tests and answers can stay in their own files.

use crate::blueprint::{self, Blueprint, Expanded};
use crate::runner::run_gate;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// The process environment, for `blueprint::user_dir`.
fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

pub const USAGE: &str = "\
gatestrainer blueprint list
          every blueprint (built in, then yours) with what it proves and its inputs
gatestrainer blueprint test BLUEPRINT CASES.json
          BLUEPRINT is a name or a .json file. Runs its gate on an empty answer
          and on CASES' \"good\" and \"bad\" answers (\"inputs\" fill it in; any
          string may be {\"file\": \"path\"}, relative to CASES). Exit 0 when the
          empty and bad answers fail and the good ones pass, 1 otherwise, 2 when
          the blueprint or the cases don't load
";

/// A case file, its `{"file": …}` strings read.
#[derive(Debug)]
pub struct Cases {
    pub inputs: Map<String, Value>,
    pub good: Vec<String>,
    pub bad: Vec<String>,
}

/// `v` with every `{"file": "path"}` replaced by that file's text.
fn read_files(v: &Value, read: &dyn Fn(&str) -> Result<String, String>) -> Result<Value, String> {
    Ok(match v {
        Value::Object(m) if m.len() == 1 && m.get("file").is_some_and(Value::is_string) => Value::String(read(m["file"].as_str().unwrap_or_default())?),
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| Ok((k.clone(), read_files(v, read)?))).collect::<Result<_, String>>()?),
        Value::Array(a) => Value::Array(a.iter().map(|v| read_files(v, read)).collect::<Result<_, _>>()?),
        other => other.clone(),
    })
}

impl Cases {
    /// `{"file": …}` strings read relative to `base`.
    ///
    /// # Errors
    /// Not JSON, an unknown key, a missing `good` or `bad`, an unreadable file.
    pub fn parse(text: &str, base: &Path) -> Result<Self, String> {
        Self::parse_with(text, &|rel: &str| {
            let path = base.join(rel);
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
        })
    }

    /// `{"file": …}` strings read by `read` (the demo's are compiled in).
    ///
    /// # Errors
    /// As [`Cases::parse`].
    pub fn parse_with(text: &str, read: &dyn Fn(&str) -> Result<String, String>) -> Result<Self, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
        let Value::Object(top) = read_files(&v, read)? else { return Err("a case file is a JSON object".into()) };
        if let Some(k) = top.keys().find(|k| !["inputs", "good", "bad", "about"].contains(&k.as_str())) {
            return Err(format!("unknown key `{k}` (inputs, good, bad, about)"));
        }
        let answers = |key: &str| -> Result<Vec<String>, String> {
            let list = top.get(key).and_then(Value::as_array).ok_or_else(|| format!("`{key}`: a list of answers is required"))?;
            let out: Vec<String> = list.iter().map(|a| a.as_str().map(str::to_string).ok_or_else(|| format!("`{key}`: every answer is a string (or {{\"file\": …}})"))).collect::<Result<_, _>>()?;
            if out.is_empty() {
                return Err(format!("`{key}`: at least one answer"));
            }
            Ok(out)
        };
        let inputs = match top.get("inputs") {
            None => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err("`inputs` is an object".into()),
        };
        Ok(Self { inputs, good: answers("good")?, bad: answers("bad")? })
    }
}

/// The blueprint by path (a `.json` that exists) or by name.
fn load(which: &str) -> Result<(Blueprint, Vec<String>), String> {
    let dir = blueprint::user_dir(&process_env);
    let path = Path::new(which);
    if which.ends_with(".json") || path.is_file() {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{which}: {e}"))?;
        let bp = Blueprint::parse(&text)?;
        let mut notes = Vec::new();
        let (all, _) = blueprint::load_all(None);
        if all.iter().any(|b| b.name == bp.name) {
            notes.push(format!("a built-in is called `{}`: rename yours, or it won't load from the blueprints folder", bp.name));
        }
        return Ok((bp, notes));
    }
    Ok((blueprint::find(which, dir.as_deref())?, Vec::new()))
}

/// One run's outcome, as the test reports it.
pub struct Trial {
    pub label: String,
    pub want_pass: bool,
    pub passed: bool,
    /// The first line of what the gate said on failure.
    pub said: String,
    /// How the run was confined (`landlock_seccomp`, …).
    pub isolation: String,
}

impl Trial {
    pub fn ok(&self) -> bool {
        self.want_pass == self.passed
    }

    pub fn line(&self) -> String {
        let got = if self.passed { "passes" } else { "fails " };
        let mark = if self.ok() { "ok" } else { "WRONG" };
        let said = if self.said.is_empty() { String::new() } else { format!("  {}", self.said) };
        format!("  {mark:<5} {:<8} {got}{said}", self.label)
    }
}

/// The first line worth showing from a failed gate; after a count ("2 of
/// 12 examples wrong") the first instance too, which is what convinces.
fn first_line(feedback: &str) -> String {
    // Rules like unittest's "=====" say nothing: lines with words only.
    // OpenSCAD's render statistics say nothing either.
    let stats = regex::Regex::new(r"^(?:Geometries in cache|Geometry cache size|CGAL (?:Polyhedrons in cache|cache size)|Total rendering time|Rendering Polygon Mesh|Compiling design|Parsing design|Normalized (?:CSG )?tree|(?:Top level object is a 3D object|Simple|Vertices|Halfedges|Edges|Halffacets|Facets|Volumes):)").ok();
    let mut lines = feedback.lines().map(str::trim).filter(|l| l.chars().any(char::is_alphanumeric) && !stats.as_ref().is_some_and(|r| r.is_match(l)));
    // Node's test runner prints passing tests (✔) and its own frames
    // (node:internal…) before the reason: the first failing test (✖) or
    // the error's own line says what broke.
    let node = feedback.lines().map(str::trim_start).any(|l| l.starts_with(['✔', '✖', 'ℹ']) || l.starts_with("node:internal"));
    let error = regex::Regex::new(r"^(?:✖ |[A-Z][A-Za-z]*Error\b)").ok();
    if let Some(l) = feedback.lines().map(str::trim).find(|l| node && error.as_ref().is_some_and(|r| r.is_match(l))) {
        return clip(l);
    }
    let first = lines.next().unwrap_or("");
    let counted = regex::Regex::new(r"FAILED — \d+ of \d+ ").is_ok_and(|r| r.is_match(first));
    let line = match lines.next().filter(|_| counted) {
        Some(next) => format!("{first}; {next}"),
        None => first.to_string(),
    };
    clip(&line)
}

fn clip(line: &str) -> String {
    if line.chars().count() > 160 {
        format!("{}…", line.chars().take(160).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Run the expanded gate on the empty answer, then each good and bad one.
///
/// # Errors
/// The gate couldn't run at all (a sandbox or I/O failure).
pub async fn trials(e: &Expanded, cases: &Cases) -> Result<Vec<Trial>, String> {
    let mut runs: Vec<(String, bool, &str)> = vec![("empty".into(), false, "")];
    runs.extend(cases.good.iter().enumerate().map(|(i, a)| (format!("good {}", i + 1), true, a.as_str())));
    runs.extend(cases.bad.iter().enumerate().map(|(i, a)| (format!("bad {}", i + 1), false, a.as_str())));
    let mut out = Vec::new();
    for (label, want_pass, answer) in runs {
        let r = run_gate(&e.gate, answer, 1, 1, "blueprint-test", None).await.map_err(|e| e.to_string())?;
        r.discard();
        let said = if r.passed { String::new() } else { first_line(&r.feedback()) };
        // A precheck that turned the answer down ran nothing: no sandbox to report.
        let isolation = if r.ran { serde_json::to_value(r.isolation).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default() } else { String::new() };
        out.push(Trial { label, want_pass, passed: r.passed, said, isolation });
    }
    Ok(out)
}

/// The demo's case files, in the order shown, and the files they refer
/// to, compiled in (paths relative to `blueprints/cases/`).
const DEMO: &[(&str, &str)] = &[
    ("math-answer.json", include_str!("../blueprints/cases/math-answer.json")),
    ("date-answer.json", include_str!("../blueprints/cases/date-answer.json")),
    ("python-tests.json", include_str!("../blueprints/cases/python-tests.json")),
    ("typescript-tests.json", include_str!("../blueprints/cases/typescript-tests.json")),
    ("python-refactor.json", include_str!("../blueprints/cases/python-refactor.json")),
    ("sql-query.json", include_str!("../blueprints/cases/sql-query.json")),
    ("regex.json", include_str!("../blueprints/cases/regex.json")),
    ("openscad-part.json", include_str!("../blueprints/cases/openscad-part.json")),
    ("json-schema.json", include_str!("../blueprints/cases/json-schema.json")),
    ("data-convert.json", include_str!("../blueprints/cases/data-convert.json")),
    ("extract-json.json", include_str!("../blueprints/cases/extract-json.json")),
    ("rewrite-text.json", include_str!("../blueprints/cases/rewrite-text.json")),
    ("purge-term.json", include_str!("../blueprints/cases/purge-term.json")),
    ("cited-answer.json", include_str!("../blueprints/cases/cited-answer.json")),
];

const DEMO_FILES: &[(&str, &str)] = &[
    ("python-tests/test_solution.py", include_str!("../blueprints/cases/python-tests/test_solution.py")),
    ("typescript-tests/solution.test.ts", include_str!("../blueprints/cases/typescript-tests/solution.test.ts")),
    ("python-refactor/original.py", include_str!("../blueprints/cases/python-refactor/original.py")),
    ("python-refactor/test_solution.py", include_str!("../blueprints/cases/python-refactor/test_solution.py")),
    ("python-refactor/cases.jsonl", include_str!("../blueprints/cases/python-refactor/cases.jsonl")),
    ("sql-query/data.sql", include_str!("../blueprints/cases/sql-query/data.sql")),
    ("sql-query/reference.sql", include_str!("../blueprints/cases/sql-query/reference.sql")),
    ("json-schema/schema.json", include_str!("../blueprints/cases/json-schema/schema.json")),
    ("data-convert/products.csv", include_str!("../blueprints/cases/data-convert/products.csv")),
    ("extract-json/invoice.txt", include_str!("../blueprints/cases/extract-json/invoice.txt")),
    ("extract-json/schema.json", include_str!("../blueprints/cases/extract-json/schema.json")),
    ("rewrite-text/source.md", include_str!("../blueprints/cases/rewrite-text/source.md")),
    ("purge-term/source.md", include_str!("../blueprints/cases/purge-term/source.md")),
    ("cited-answer/sources.txt", include_str!("../blueprints/cases/cited-answer/sources.txt")),
];

fn demo_file(rel: &str) -> Result<String, String> {
    DEMO_FILES.iter().find(|(p, _)| *p == rel).map(|(_, t)| t.to_string()).ok_or_else(|| format!("{rel}: not in the demo"))
}

/// A worked example for blueprint `name`, from its demo case file: its
/// inputs, a right answer and a near miss (the console's "Load example").
pub fn example(name: &str) -> Option<Value> {
    let (_, text) = DEMO.iter().find(|(f, _)| f.strip_suffix(".json") == Some(name))?;
    let c = Cases::parse_with(text, &demo_file).ok()?;
    Some(serde_json::json!({ "about": about(text), "inputs": c.inputs, "answer": c.good.first(), "near_miss": c.bad.first() }))
}

/// The `about` line of a case file, if it has one.
fn about(text: &str) -> String {
    serde_json::from_str::<Value>(text).ok().and_then(|v| v["about"].as_str().map(str::to_string)).unwrap_or_default()
}

/// `--demo`: every demo case file through its real gate, with no model.
pub async fn demo() -> i32 {
    println!("Gatestrainer demo: the gate decides what counts as done, not the model.");
    println!("No model runs here. Each gate gets an empty answer, right answers and near misses");
    println!("written in advance; it must pass the right ones and catch every other.\n");
    let started = std::time::Instant::now();
    let (mut gates, mut answers, mut wrong, mut skipped) = (0, 0, 0, Vec::new());
    let mut isolation = std::collections::BTreeSet::new();
    for (file, text) in DEMO {
        let name = file.trim_end_matches(".json");
        let ran = async {
            let bp = blueprint::find(name, None)?;
            let cases = Cases::parse_with(text, &demo_file)?;
            let e = bp.expand(&cases.inputs)?;
            let t0 = std::time::Instant::now();
            let t = trials(&e, &cases).await?;
            Ok::<_, String>((bp, t, t0.elapsed()))
        }
        .await;
        match ran {
            Ok((bp, t, took)) => {
                println!("{name} ({}): {}", format!("{:?}", bp.proves).to_lowercase(), about(text));
                for trial in &t {
                    println!("{}", trial.line());
                    isolation.insert(trial.isolation.clone());
                }
                println!("  {:.1} s\n", took.as_secs_f64());
                gates += 1;
                answers += t.len();
                wrong += t.iter().filter(|x| !x.ok()).count();
            }
            Err(e) => {
                println!("{name}: skipped ({e})\n");
                skipped.push(name);
            }
        }
    }
    let verdict = if wrong == 0 { "every answer judged as expected".to_string() } else { format!("{wrong} judged WRONG") };
    let iso: Vec<String> = isolation.into_iter().filter(|i| !i.is_empty()).collect();
    println!("{gates} gates, {answers} answers: {verdict} in {:.1} s; sandbox: {}", started.elapsed().as_secs_f64(), iso.join(", "));
    if !skipped.is_empty() {
        println!("skipped (a tool missing on this machine?): {}", skipped.join(", "));
    }
    i32::from(wrong > 0)
}

fn list() -> i32 {
    let dir = blueprint::user_dir(&process_env);
    let (all, problems) = blueprint::load_all(dir.as_deref());
    for b in &all {
        let inputs: Vec<String> = b.inputs.iter().map(|(n, i)| if i.default.is_some() { format!("{n}?") } else { n.clone() }).collect();
        println!("{:<22} {:<8} {}", b.name, format!("{:?}", b.proves).to_lowercase(), inputs.join(" "));
    }
    if let Some(d) = &dir {
        println!("\nyours go in {}", d.display());
    }
    for p in &problems {
        eprintln!("not loaded: {p}");
    }
    i32::from(!problems.is_empty())
}

async fn test(which: &str, cases_path: &str) -> i32 {
    let (bp, notes) = match load(which) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let base: PathBuf = Path::new(cases_path).parent().map(Path::to_path_buf).unwrap_or_default();
    let cases = match std::fs::read_to_string(cases_path).map_err(|e| e.to_string()).and_then(|t| Cases::parse(&t, &base)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{cases_path}: {e}");
            return 2;
        }
    };
    let e = match bp.expand(&cases.inputs) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    println!("blueprint {} ({}): rules ok", bp.name, format!("{:?}", bp.proves).to_lowercase());
    for n in &notes {
        println!("  note: {n}");
    }
    let trials = match trials(&e, &cases).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("the gate couldn't run: {e}");
            return 2;
        }
    };
    for t in &trials {
        println!("{}", t.line());
    }
    let right = trials.iter().filter(|t| t.ok()).count();
    println!("{right} of {} as expected", trials.len());
    if trials.first().is_some_and(|t| t.passed) {
        println!("the empty answer passes, so a pass from this gate says nothing: make a step fail on it");
    }
    i32::from(right != trials.len())
}

/// `--blueprint …` from the command line; returns the exit code.
pub async fn cli(args: &[String]) -> i32 {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["list"] => list(),
        ["test", which, cases] => test(which, cases).await,
        _ => {
            eprint!("{USAGE}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn case_files() {
        let d = tempfile::tempdir().expect("tempdir");
        std::fs::write(d.path().join("t.py"), "import unittest\n").expect("write");
        let text = json!({ "inputs": { "tests": { "file": "t.py" }, "n": 3 }, "good": ["a"], "bad": [{ "file": "t.py" }] }).to_string();
        let c = Cases::parse(&text, d.path()).expect("parses");
        assert_eq!(c.inputs["tests"], "import unittest\n");
        assert_eq!(c.inputs["n"], 3);
        assert_eq!((c.good.len(), c.bad[0].as_str()), (1, "import unittest\n"));
        for (bad, want) in [
            (json!({ "good": ["a"] }), "`bad`"),
            (json!({ "good": [], "bad": ["b"] }), "at least one"),
            (json!({ "good": ["a"], "bad": [1] }), "every answer"),
            (json!({ "good": ["a"], "bad": ["b"], "expected": 1 }), "unknown key"),
            (json!({ "good": [{ "file": "missing.txt" }], "bad": ["b"] }), "missing.txt"),
            (json!(["a"]), "JSON object"),
        ] {
            let e = Cases::parse(&bad.to_string(), d.path()).expect_err("refused");
            assert!(e.contains(want), "{bad}: {e}");
        }
    }

    #[test]
    fn demo_case_files_load() {
        assert!(!DEMO.is_empty());
        for (file, text) in DEMO {
            let c = Cases::parse_with(text, &demo_file).unwrap_or_else(|e| panic!("{file}: {e}"));
            let bp = blueprint::find(file.trim_end_matches(".json"), None).expect("a built-in");
            bp.expand(&c.inputs).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert!(!about(text).is_empty(), "{file}: give it an about line");
            let e = example(file.trim_end_matches(".json")).expect("an example");
            assert!(e["answer"].is_string() && e["near_miss"].is_string() && e["inputs"].is_object(), "{file}: {e}");
        }
        assert!(example("rust-tests").is_none(), "no case file, no example");
    }

    #[test]
    fn trial_lines() {
        let t = Trial { label: "bad 1".into(), want_pass: false, passed: true, said: String::new(), isolation: String::new() };
        assert!(!t.ok());
        assert!(t.line().contains("WRONG"), "{}", t.line());
        let t = Trial { label: "empty".into(), want_pass: false, passed: false, said: "numeric: FAILED — no match".into(), isolation: String::new() };
        assert!(t.ok() && t.line().contains("fails") && t.line().contains("no match"), "{}", t.line());
        assert_eq!(first_line("\n  x: FAILED — y\nmore"), "x: FAILED — y");
        assert_eq!(first_line("=====\nFAIL: test_x (t.T.test_x)\n-----"), "FAIL: test_x (t.T.test_x)");
        assert_eq!(first_line("Geometries in cache: 1\nGeometry cache size in bytes: 0\nCGAL Polyhedrons in cache: 0\nCGAL cache size in bytes: 0\nTotal rendering time: 0:00:00.000\nCurrent top level object is empty."), "Current top level object is empty.");
        assert_eq!(first_line("✔ plain units (0.8ms)\n✖ malformed input throws (1.2ms)\n  AssertionError [ERR_ASSERTION]: Missing expected exception"), "✖ malformed input throws (1.2ms)");
        assert_eq!(
            first_line("node:internal/modules/run_main:111\n    triggerUncaughtException(\n    ^\nSyntaxError [ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX]: TypeScript enum is not supported in strip-only mode"),
            "SyntaxError [ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX]: TypeScript enum is not supported in strip-only mode"
        );
        assert_eq!(first_line("regex: FAILED — 1 of 3 examples wrong (rust, whole line)\n  should match: \"a\""), "regex: FAILED — 1 of 3 examples wrong (rust, whole line); should match: \"a\"");
    }

    /// Every built-in case file (`blueprints/cases/NAME.json`) goes as
    /// expected — those whose gate needs no `{{gatestrainer}}` (in a test run
    /// this binary is the test harness); the demo runs the rest.
    #[cfg(unix)]
    #[tokio::test]
    async fn built_in_case_files() {
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            eprintln!("skipping: python3 missing");
            return;
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("blueprints").join("cases");
        let mut ran = 0;
        for path in std::fs::read_dir(&dir).expect("cases dir").flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")) {
            let name = path.file_stem().and_then(|s| s.to_str()).expect("name").to_string();
            let bp = blueprint::find(&name, None).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            if bp.steps.iter().any(|s| s.contains("{{gatestrainer}}")) {
                continue;
            }
            // A blueprint whose tool this machine lacks (node for the
            // JavaScript and TypeScript ones) is skipped, not failed.
            let bin = |t: &'static str| if t == "python" { "python3" } else { t };
            if let Some(t) = bp.tools().into_iter().find(|t| std::process::Command::new(bin(t)).arg("--version").output().is_err()) {
                eprintln!("skipping {name}: {t} missing");
                continue;
            }
            let cases = Cases::parse(&std::fs::read_to_string(&path).expect("read"), &dir).unwrap_or_else(|e| panic!("{name}: {e}"));
            let t = trials(&bp.expand(&cases.inputs).expect("expands"), &cases).await.expect("runs");
            assert!(t.iter().all(Trial::ok), "{name}: {:?}", t.iter().map(Trial::line).collect::<Vec<_>>());
            ran += 1;
        }
        assert!(ran > 0, "no case file ran");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn python_blueprint_against_cases() {
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            eprintln!("skipping: python3 missing");
            return;
        }
        let bp = blueprint::find("python-tests", None).expect("built in");
        let tests = "import unittest\nfrom solution import add\nclass T(unittest.TestCase):\n    def test(self):\n        self.assertEqual(add(2, 3), 5)\n";
        let cases = Cases { inputs: json!({ "tests": tests }).as_object().cloned().unwrap_or_default(), good: vec!["def add(a, b):\n    return a + b\n".into()], bad: vec!["def add(a, b):\n    return a * b\n".into()] };
        let t = trials(&bp.expand(&cases.inputs).expect("expands"), &cases).await.expect("runs");
        assert_eq!(t.len(), 3);
        assert!(t.iter().all(Trial::ok), "{:?}", t.iter().map(Trial::line).collect::<Vec<_>>());
    }
}
