//! `ran`: a test command exited 0 *and* its runner reported finishing.
//! The answer's code runs inside the test process, so an exit status of 0
//! alone proves nothing: `os._exit(0)`, `process.exit(0)` or
//! `std::process::exit(0)` in the answer ends the run "successfully" before
//! a single assertion. The runner's own report says it got to the end:
//! unittest's `Ran N tests` and `OK`; no file reported as a test by itself
//! (what `node --test` prints when a file exits before its tests do); a
//! `test result:` for every test binary cargo starts.
//!
//! `gatestrainer --check ran [--match RE]... [--forbid RE]... [--each START --done END] -- CMD [ARG]...`
//!
//! The command's output is passed through, so a failing test reads as it
//! always did. (Code that prints a forged report on purpose can still pass;
//! this stops the accidental and the lazy exit, not a determined forger.)

use super::{Args, Fail, Outcome, Run};
use regex::Regex;
use std::io::Write;
use std::process::{Command, Stdio};

/// Every `--match` matches, no `--forbid` does, and each `--each` line is
/// followed by a `--done` line before the next `--each`: why not, or None.
pub(crate) fn unfinished(output: &str, matches: &[Regex], forbids: &[Regex], each: Option<(&Regex, &Regex)>) -> Option<String> {
    if let Some(r) = matches.iter().find(|r| !r.is_match(output)) {
        return Some(format!("the test run ended before the runner reported its result (no line matching {:?}): did the code exit the process?", r.as_str()));
    }
    if let Some(m) = forbids.iter().find_map(|r| r.find(output)) {
        return Some(format!("the runner reported {:?} instead of its tests: the code ended the run early (did it exit the process?)", m.as_str().trim()));
    }
    if let Some((start, done)) = each {
        let mut open: Option<&str> = None;
        for line in output.lines() {
            if start.is_match(line) {
                if let Some(o) = open {
                    return Some(format!("{:?} started but never reported a result: the code ended it early (did it exit the process?)", o.trim()));
                }
                open = Some(line);
            } else if done.is_match(line) {
                open = None;
            }
        }
        if let Some(o) = open {
            return Some(format!("{:?} started but never reported a result: the code ended it early (did it exit the process?)", o.trim()));
        }
    }
    None
}

pub(crate) fn run(args: &[String]) -> Run {
    let Some(split) = args.iter().position(|a| a == "--") else { return Err("give the command after `--`".into()) };
    let (opts, cmd) = (&args[..split], &args[split + 1..]);
    let Some((program, rest)) = cmd.split_first() else { return Err("no command after `--`".into()) };
    let a = Args::parse(opts, &[])?;
    a.only(&["match", "forbid", "each", "done"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?} (the command goes after `--`)"));
    }
    let compile = |s: &str| Regex::new(&format!("(?m){s}")).map_err(|e| format!("{s:?}: {e}"));
    let matches = a.all("match").into_iter().map(compile).collect::<Result<Vec<_>, _>>()?;
    let forbids = a.all("forbid").into_iter().map(compile).collect::<Result<Vec<_>, _>>()?;
    let each = match (a.one("each")?, a.one("done")?) {
        (Some(s), Some(d)) => Some((compile(s)?, compile(d)?)),
        (None, None) => None,
        _ => return Err("--each and --done go together".into()),
    };
    if matches.is_empty() && forbids.is_empty() && each.is_none() {
        return Err("give at least one --match, --forbid or --each/--done: else this is only the exit status".into());
    }
    // The program not starting is the gate's problem (a missing toolchain).
    let out = Command::new(program).args(rest).stdin(Stdio::null()).output().map_err(|e| format!("{program}: {e}"))?;
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    print!("{stdout}");
    eprint!("{stderr}");
    let _ = std::io::stdout().flush();
    if !out.status.success() {
        return Ok(Outcome::Fail(Fail(vec![format!("the tests failed ({})", out.status)])));
    }
    let both = format!("{stdout}\n{stderr}");
    match unfinished(&both, &matches, &forbids, each.as_ref().map(|(s, d)| (s, d))) {
        Some(why) => Ok(Outcome::Fail(Fail(vec![why]))),
        None => Ok(Outcome::Pass("the runner reported finishing".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn re(s: &str) -> Regex {
        Regex::new(&format!("(?m){s}")).expect("regex")
    }

    #[test]
    fn unittest_must_say_it_ran_and_passed() {
        let (ran, ok) = (re(r"^Ran [1-9][0-9]* tests? in "), re("^OK"));
        assert!(unfinished("..\n----\nRan 2 tests in 0.001s\n\nOK\n", &[ran.clone(), ok.clone()], &[], None).is_none());
        assert!(unfinished("", &[ran.clone(), ok.clone()], &[], None).unwrap_or_default().contains("did the code exit"), "os._exit(0): nothing printed");
        assert!(unfinished("Ran 0 tests in 0.000s\n\nOK\n", &[ran, ok], &[], None).is_some(), "no tests isn't a pass");
    }

    #[test]
    fn node_must_not_report_the_file_as_its_test() {
        let file = re(r"^✔ \S+\.test\.[cm]?[jt]s\b");
        assert!(unfinished("✔ plain units (0.8ms)\nℹ tests 3\n", &[], std::slice::from_ref(&file), None).is_none());
        assert!(unfinished("✔ solution.test.mjs (65ms)\nℹ tests 1\nℹ pass 1\n", &[], &[file], None).unwrap_or_default().contains("solution.test.mjs"));
    }

    #[test]
    fn every_cargo_test_binary_must_report() {
        let (start, done) = (re(r"^\s*(?:Running|Doc-tests) "), re(r"^test result: "));
        let good = "     Running unittests src/lib.rs (x)\ntest result: ok. 1 passed\n     Running tests/hidden.rs (y)\ntest result: ok. 2 passed\n   Doc-tests task\ntest result: ok. 0 passed\n";
        assert!(unfinished(good, &[], &[], Some((&start, &done))).is_none());
        let cut = "     Running unittests src/lib.rs (x)\ntest result: ok. 1 passed\n     Running tests/hidden.rs (y)\n   Doc-tests task\ntest result: ok. 0 passed\n";
        assert!(unfinished(cut, &[], &[], Some((&start, &done))).unwrap_or_default().contains("tests/hidden.rs"));
        assert!(unfinished("     Running tests/hidden.rs (y)\n", &[], &[], Some((&start, &done))).is_some(), "cut off at the end");
    }

    #[cfg(unix)]
    #[test]
    fn cli() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(matches!(run(&s(&["--match", "^hello", "--", "echo", "hello"])), Ok(Outcome::Pass(_))));
        assert!(matches!(run(&s(&["--match", "^Ran", "--", "true"])), Ok(Outcome::Fail(_))), "exit 0, no report");
        assert!(matches!(run(&s(&["--match", "^x", "--", "false"])), Ok(Outcome::Fail(_))));
        assert!(run(&s(&["--", "true"])).is_err(), "only the exit status");
        assert!(run(&s(&["--match", "x"])).is_err(), "no command");
        assert!(run(&s(&["--each", "x", "--", "true"])).is_err());
        assert!(run(&s(&["--match", "x", "--", "/no/such/program"])).is_err());
    }
}
