//! `differential`: two programs agree on every case — the refactor's
//! behaviour is the original's. Each command gets the whole cases file on
//! stdin and prints one line per case (non-empty input line); the outputs
//! are compared line by line.

use super::{Args, Fail, Outcome, Run};
use std::io::Write;
use std::process::{Command, Stdio};

/// Differing cases listed.
const SHOWN: usize = 10;
/// Characters of a case or an output shown per line.
const CLIP: usize = 120;

/// What a command printed, or why it didn't run cleanly — including a
/// program that can't start (missing, not executable): for the answer's
/// side that's the answer's failure, for the reference the check's.
fn run_side(cmd: &str, input: &str) -> Result<Result<String, String>, String> {
    let argv = shlex::split(cmd).filter(|a| !a.is_empty()).ok_or_else(|| format!("can't split {cmd:?} into a command"))?;
    let mut child = match Command::new(&argv[0]).args(&argv[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => return Ok(Err(format!("`{cmd}` can't start: {}: {e}", argv[0]))),
    };
    let mut stdin = child.stdin.take().ok_or("no stdin pipe")?;
    let text = input.to_string();
    // Written beside the read, so a large input can't deadlock on full pipes.
    let writer = std::thread::spawn(move || stdin.write_all(text.as_bytes()));
    let out = child.wait_with_output().map_err(|e| format!("{}: {e}", argv[0]))?;
    // A program that exits without reading all its input is its own failure.
    let _ = writer.join();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
        return Ok(Err(format!("`{cmd}` exited with {}: {}", out.status, tail.join(" | "))));
    }
    Ok(Ok(String::from_utf8_lossy(&out.stdout).into_owned()))
}

fn clip(s: &str) -> String {
    if s.chars().count() <= CLIP {
        return s.to_string();
    }
    format!("{}…", s.chars().take(CLIP).collect::<String>())
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["cases", "left", "right"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let cases_path = a.need("cases")?;
    let (left, right) = (a.need("left")?, a.need("right")?);
    let input = std::fs::read_to_string(cases_path).map_err(|e| format!("--cases {cases_path}: {e}"))?;
    let cases: Vec<&str> = input.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if cases.is_empty() {
        return Err(format!("--cases {cases_path} has no cases"));
    }
    // The left side is the reference (the gate author's): if it can't run,
    // the check is wrong, not the answer.
    let expected = run_side(left, &input)?.map_err(|e| format!("the reference failed: {e}"))?;
    let got = match run_side(right, &input)? {
        Ok(out) => out,
        Err(e) => return Ok(Outcome::Fail(Fail(vec![e]))),
    };
    let (exp, got): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), got.lines().collect());
    if exp.len() != cases.len() {
        return Err(format!("the reference printed {} lines for {} cases (one line per case)", exp.len(), cases.len()));
    }
    let mut bad: Vec<String> = cases
        .iter()
        .enumerate()
        .filter(|(i, _)| exp.get(*i) != got.get(*i))
        .map(|(i, case)| format!("case {} {}: expected {}, got {}", i + 1, clip(case), clip(exp[i]), got.get(i).map_or("nothing".into(), |g| clip(g))))
        .collect();
    if got.len() > cases.len() {
        bad.push(format!("{} extra output lines (one line per case)", got.len() - cases.len()));
    }
    if bad.is_empty() {
        return Ok(Outcome::Pass(format!("{} cases agree", cases.len())));
    }
    let total = bad.len();
    let mut lines = vec![format!("{total} of {} cases differ from the original", cases.len())];
    lines.extend(bad.into_iter().take(SHOWN));
    Ok(Outcome::Fail(Fail(lines)))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn go(dir: &std::path::Path, left: &str, right: &str) -> Run {
        let cases = dir.join("cases.txt");
        run(&[
            "--cases".into(),
            cases.to_string_lossy().into_owned(),
            "--left".into(),
            left.into(),
            "--right".into(),
            right.into(),
        ])
    }

    #[test]
    fn agreeing_and_differing_programs() {
        let d = tempfile::tempdir().expect("tempdir");
        std::fs::write(d.path().join("cases.txt"), "a\n\nb\nc\n").expect("write");
        let up = d.path().join("up.txt");
        std::fs::write(&up, "A\nB\nC\n").expect("write");
        let bad = d.path().join("bad.txt");
        std::fs::write(&bad, "A\nX\nC\n").expect("write");
        // `cat FILE` ignores stdin: a stand-in for a program's answers.
        // Quoted: temp folders can sit under a path with spaces.
        let cat = |p: &std::path::Path| format!("cat {}", shlex::try_quote(&p.to_string_lossy()).expect("quotable"));
        assert!(matches!(go(d.path(), &cat(&up), &cat(&up)), Ok(Outcome::Pass(_))));
        assert!(matches!(go(d.path(), &cat(&up), "./no-such-answer-program"), Ok(Outcome::Fail(_))), "an answer that can't start is the answer's failure");
        assert!(go(d.path(), "./no-such-reference-program", &cat(&up)).is_err(), "a reference that can't start is the check's");
        match go(d.path(), &cat(&up), &cat(&bad)) {
            Ok(Outcome::Fail(Fail(l))) => {
                assert!(l[0].starts_with("1 of 3 cases differ"), "{l:?}");
                assert_eq!(l[1], "case 2 b: expected B, got X", "blank lines aren't cases: {l:?}");
            }
            _ => panic!("expected a difference"),
        }
        assert!(matches!(go(d.path(), &cat(&up), "false"), Ok(Outcome::Fail(_))), "the answer failing is the answer's fault");
        assert!(go(d.path(), "false", &cat(&up)).is_err(), "the reference failing is the check's fault");
        assert!(go(d.path(), "cat", "cat").is_err(), "the reference printed 4 lines (incl. the blank) for 3 cases");
        assert!(go(d.path(), "gatestrainer-no-such-program", "cat").is_err());
    }
}
