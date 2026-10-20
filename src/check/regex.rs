//! `regex`: a pattern matches every line it should and none it shouldn't,
//! in the engine it will run in. Engines differ (lookbehind, `\d` and
//! Unicode, `$` before a final newline), so Python's `re` and JavaScript's
//! `RegExp` run in their own interpreter; `rust` is the `regex` crate here.

use super::{read_artifact, Args, Fail, Outcome, Run};
use super::{bin_name, NODE_BIN_ENV, PYTHON_BIN_ENV};
use std::io::Write;
use std::process::{Command, Stdio};

/// Wrong examples listed.
const SHOWN: usize = 10;

/// Reads `{"pattern", "full", "lines"}` on stdin; prints a JSON list of
/// booleans, or exits 3 with the compile error on stderr.
const PYTHON: &str = r#"import json, re, sys
d = json.load(sys.stdin)
try:
    r = re.compile(d["pattern"])
except Exception as e:  # re.error, and OverflowError/RecursionError on absurd patterns
    print(f"{type(e).__name__}: {e}", file=sys.stderr)
    sys.exit(3)
f = r.fullmatch if d["full"] else r.search
print(json.dumps([f(s) is not None for s in d["lines"]]))
"#;

const JAVASCRIPT: &str = r#"let t = "";
process.stdin.on("data", (c) => (t += c));
process.stdin.on("end", () => {
  const d = JSON.parse(t);
  let r;
  try {
    r = new RegExp(d.full ? "^(?:" + d.pattern + ")$" : d.pattern);
  } catch (e) {
    process.stderr.write(e.message + "\n");
    process.exit(3);
  }
  process.stdout.write(JSON.stringify(d.lines.map((s) => r.test(s))) + "\n");
});
"#;

/// Whether each line matches, or the pattern's compile error (`Ok(Err)`,
/// the answer's fault). `Err` is a broken check: no interpreter, bad output.
fn matches(engine: &str, pattern: &str, full: bool, lines: &[&str]) -> Result<Result<Vec<bool>, String>, String> {
    if engine == "rust" {
        // Under a verbose flag `(?x)` a `#` comment would swallow the closing
        // `)$`: a newline (whitespace there) ends the comment first.
        let verbose = regex::Regex::new(r"\(\?[a-wyzA-Z]*x[a-zA-Z]*(?:-[a-zA-Z]*)?\)").is_ok_and(|v| v.is_match(pattern));
        let p = if full { format!("^(?:{pattern}{})$", if verbose { "\n" } else { "" }) } else { pattern.to_string() };
        return Ok(match regex::Regex::new(&p) {
            Ok(r) => Ok(lines.iter().map(|l| r.is_match(l)).collect()),
            Err(e) => Err(e.to_string()),
        });
    }
    let (bin, flag, script) = match engine {
        "python" => (bin_name(PYTHON_BIN_ENV, if cfg!(windows) { "python" } else { "python3" }), "-c", PYTHON),
        "javascript" => (bin_name(NODE_BIN_ENV, "node"), "-e", JAVASCRIPT),
        other => return Err(format!("--engine {other:?}: python, javascript or rust")),
    };
    let input = serde_json::json!({ "pattern": pattern, "full": full, "lines": lines }).to_string();
    let mut child = Command::new(&bin)
        .args([flag, script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{bin}: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin pipe")?;
    // Written beside the read, so a large input can't deadlock on full pipes.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().map_err(|e| format!("{bin}: {e}"))?;
    let _ = writer.join();
    if out.status.code() == Some(3) {
        return Ok(Err(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    if !out.status.success() {
        return Err(format!("{bin} exited with {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let got: Vec<bool> = serde_json::from_slice(&out.stdout).map_err(|e| format!("{bin} printed something unexpected: {e}"))?;
    if got.len() != lines.len() {
        return Err(format!("{bin} answered {} lines for {}", got.len(), lines.len()));
    }
    Ok(Ok(got))
}

/// The examples in a file: one per line, blank lines skipped.
fn examples(path: &str) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Ok(text.lines().map(|l| l.strip_suffix('\r').unwrap_or(l)).filter(|l| !l.trim().is_empty()).map(str::to_string).collect())
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["file", "match", "no-match", "engine", "mode"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let engine = a.one("engine")?.unwrap_or("rust");
    let full = match a.one("mode")?.unwrap_or("search") {
        "full" => true,
        "search" => false,
        other => return Err(format!("--mode {other:?}: full (the whole line) or search (anywhere in it)")),
    };
    let yes = a.all("match").into_iter().map(examples).collect::<Result<Vec<_>, _>>()?.concat();
    let no = a.all("no-match").into_iter().map(examples).collect::<Result<Vec<_>, _>>()?.concat();
    if yes.is_empty() || no.is_empty() {
        return Err("give examples that must match (--match FILE) and examples that must not (--no-match FILE), at least one of each".into());
    }
    let text = match read_artifact(a.need("file")?) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let pattern = text.strip_suffix('\n').map(|t| t.strip_suffix('\r').unwrap_or(t)).unwrap_or(&text);
    if pattern.is_empty() {
        return Ok(Outcome::Fail(Fail(vec!["the pattern is empty".into()])));
    }
    if pattern.contains('\n') {
        return Ok(Outcome::Fail(Fail(vec!["the pattern must be one line: the regular expression alone, nothing else".into()])));
    }
    let lines: Vec<&str> = yes.iter().chain(&no).map(String::as_str).collect();
    let got = match matches(engine, pattern, full, &lines)? {
        Ok(g) => g,
        Err(e) => return Ok(Outcome::Fail(Fail(vec![format!("the pattern doesn't compile ({engine}): {e}")]))),
    };
    let wrong: Vec<String> = lines
        .iter()
        .zip(&got)
        .enumerate()
        .filter(|(i, (_, m))| **m != (*i < yes.len()))
        .map(|(i, (l, _))| if i < yes.len() { format!("should match: {l:?}") } else { format!("should not match: {l:?}") })
        .collect();
    let how = if full { "whole line" } else { "anywhere in the line" };
    if wrong.is_empty() {
        return Ok(Outcome::Pass(format!("{} match, {} don't ({engine}, {how})", yes.len(), no.len())));
    }
    let mut out = vec![format!("{} of {} examples wrong ({engine}, {how})", wrong.len(), lines.len())];
    out.extend(wrong.into_iter().take(SHOWN));
    Ok(Outcome::Fail(Fail(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn go(pattern: &str, engine: &str, full: bool) -> Run {
        let d = tempfile::tempdir().expect("tempdir");
        let (p, y, n) = (d.path().join("p.txt"), d.path().join("y.txt"), d.path().join("n.txt"));
        std::fs::write(&p, format!("{pattern}\n")).expect("write");
        std::fs::write(&y, "2026-10-08\n\n1999-01-31\n").expect("write");
        std::fs::write(&n, "2026-1-08\n26-10-08\nx2026-10-08\n").expect("write");
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let mut v = vec!["--file".into(), s(&p), "--match".into(), s(&y), "--no-match".into(), s(&n), "--engine".into(), engine.into()];
        v.extend(["--mode".into(), if full { "full" } else { "search" }.into()]);
        run(&v)
    }

    fn have(bin: &str) -> bool {
        Command::new(bin).arg("--version").output().is_ok()
    }

    #[test]
    fn rust_engine() {
        assert!(matches!(go(r"\d{4}-\d{2}-\d{2}", "rust", true), Ok(Outcome::Pass(_))));
        // Anywhere in the line, "x2026-10-08" matches too.
        match go(r"\d{4}-\d{2}-\d{2}", "rust", false) {
            Ok(Outcome::Fail(Fail(l))) => assert_eq!(l, ["1 of 5 examples wrong (rust, anywhere in the line)", "should not match: \"x2026-10-08\""]),
            _ => panic!("expected one wrong"),
        }
        assert!(matches!(go(r"\d{4}-(", "rust", true), Ok(Outcome::Fail(Fail(l))) if l[0].starts_with("the pattern doesn't compile (rust)")));
        assert!(matches!(go("", "rust", true), Ok(Outcome::Fail(Fail(l))) if l[0] == "the pattern is empty"));
        assert!(matches!(go("a\nb", "rust", true), Ok(Outcome::Fail(Fail(l))) if l[0].contains("one line")));
        assert!(go("x", "perl", true).is_err());
        assert!(matches!(go(r"(?x) \d{4}-\d{2}-\d{2}  # an ISO date", "rust", true), Ok(Outcome::Pass(_))), "a verbose comment can't swallow the anchor");
        assert!(run(&["--mode".into(), "exact".into()]).is_err());
    }

    #[test]
    fn interpreters() {
        // Lookbehind: Python and JavaScript have it, the rust engine doesn't.
        let p = r"(?<!x)\d{4}-\d{2}-\d{2}";
        if have("python3") {
            assert!(matches!(go(p, "python", false), Ok(Outcome::Pass(_))));
            assert!(matches!(go(r"\d{4}-(", "python", true), Ok(Outcome::Fail(Fail(l))) if l[0].starts_with("the pattern doesn't compile (python)")));
            assert!(matches!(go("a{18446744073709551616}", "python", true), Ok(Outcome::Fail(Fail(l))) if l[0].starts_with("the pattern doesn't compile (python)")), "OverflowError is the pattern's fault");
        }
        if have("node") {
            assert!(matches!(go(p, "javascript", false), Ok(Outcome::Pass(_))));
            assert!(matches!(go(r"\d{4}-\d{2}-\d{2}", "javascript", true), Ok(Outcome::Pass(_))));
        }
        assert!(matches!(go(p, "rust", false), Ok(Outcome::Fail(Fail(l))) if l[0].contains("doesn't compile")));
    }

    #[test]
    fn needs_both_kinds_of_example() {
        let d = tempfile::tempdir().expect("tempdir");
        let p = d.path().join("p.txt");
        std::fs::write(&p, "a").expect("write");
        let e = d.path().join("e.txt");
        std::fs::write(&e, "\n\n").expect("write");
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        assert!(run(&["--file".into(), s(&p), "--match".into(), s(&p), "--no-match".into(), s(&e)]).is_err());
    }
}
