//! `count`: how often a pattern occurs across files — a required item is
//! present, a forbidden one absent, a purge complete.

use super::{read_artifact, Args, Fail, Outcome, Run};
use regex::Regex;

/// Locations listed when the count is wrong.
const SHOWN: usize = 10;

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &["fixed"])?;
    a.only(&["pattern", "fixed", "eq", "min", "max"])?;
    let pattern = a.need("pattern")?;
    let re = if a.flag("fixed") { Regex::new(&regex::escape(pattern)) } else { Regex::new(pattern) }
        .map_err(|e| format!("--pattern: {e}"))?;
    let (eq, min, max) = (a.number::<usize>("eq")?, a.number::<usize>("min")?, a.number::<usize>("max")?);
    if eq.is_some() && (min.is_some() || max.is_some()) {
        return Err("--eq can't be combined with --min/--max".into());
    }
    if eq.is_none() && min.is_none() && max.is_none() {
        return Err("give --eq N, or --min N and/or --max N".into());
    }
    if a.positional.is_empty() {
        return Err("no files given".into());
    }
    let mut total = 0;
    let mut found = Vec::new();
    for file in &a.positional {
        let text = match read_artifact(file) {
            Ok(t) => t,
            Err(f) => return Ok(Outcome::Fail(f)),
        };
        for m in re.find_iter(&text) {
            total += 1;
            if found.len() < SHOWN {
                let line = text[..m.start()].matches('\n').count() + 1;
                found.push(format!("{file}:{line}: {:?}", m.as_str()));
            }
        }
    }
    let want = match (eq, min, max) {
        (Some(n), _, _) => format!("exactly {n}"),
        (None, Some(lo), Some(hi)) => format!("{lo} to {hi}"),
        (None, Some(lo), None) => format!("at least {lo}"),
        (None, _, hi) => format!("at most {}", hi.unwrap_or(usize::MAX)),
    };
    let ok = eq.is_none_or(|n| total == n) && min.is_none_or(|n| total >= n) && max.is_none_or(|n| total <= n);
    if ok {
        return Ok(Outcome::Pass(format!("{pattern:?} found {total} times (want {want})")));
    }
    let mut lines = vec![format!("{pattern:?} found {total} times in {} file(s); want {want}", a.positional.len())];
    lines.extend(found);
    Ok(Outcome::Fail(Fail(lines)))
}

#[cfg(test)]
mod tests {
    use super::super::Outcome;
    use super::*;

    fn go(dir: &std::path::Path, extra: &[&str]) -> Run {
        let mut v: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        v.push(dir.join("a.txt").to_string_lossy().into_owned());
        v.push(dir.join("b.txt").to_string_lossy().into_owned());
        run(&v)
    }

    #[test]
    fn counts_across_files_with_bounds() {
        let d = tempfile::tempdir().expect("tempdir");
        std::fs::write(d.path().join("a.txt"), "pyg.init()\nlet x = 1;\npyg.quit()\n").expect("write");
        std::fs::write(d.path().join("b.txt"), "pygame.init()\n").expect("write");
        assert!(matches!(go(d.path(), &["--pattern", r"\bpyg\.", "--eq", "2"]), Ok(Outcome::Pass(_))));
        match go(d.path(), &["--pattern", r"\bpyg\.", "--eq", "0"]) {
            Ok(Outcome::Fail(Fail(lines))) => {
                assert!(lines[0].contains("found 2 times"), "{lines:?}");
                assert!(lines[1].ends_with("a.txt:1: \"pyg.\""), "{lines:?}");
                assert!(lines[2].contains("a.txt:3"), "{lines:?}");
            }
            _ => panic!("expected a failure"),
        }
        assert!(matches!(go(d.path(), &["--pattern", "init", "--min", "2", "--max", "2"]), Ok(Outcome::Pass(_))));
        assert!(matches!(go(d.path(), &["--pattern", "init", "--max", "1"]), Ok(Outcome::Fail(_))));
    }

    #[test]
    fn fixed_strings_are_not_patterns() {
        let d = tempfile::tempdir().expect("tempdir");
        std::fs::write(d.path().join("a.txt"), "a.b axb").expect("write");
        std::fs::write(d.path().join("b.txt"), "").expect("write");
        assert!(matches!(go(d.path(), &["--pattern", "a.b", "--eq", "2"]), Ok(Outcome::Pass(_))));
        assert!(matches!(go(d.path(), &["--pattern", "a.b", "--fixed", "--eq", "1"]), Ok(Outcome::Pass(_))));
    }

    #[test]
    fn missing_file_fails_the_artifact_bad_setup_is_refused() {
        let d = tempfile::tempdir().expect("tempdir");
        std::fs::write(d.path().join("a.txt"), "x").expect("write");
        assert!(matches!(go(d.path(), &["--pattern", "x", "--eq", "1"]), Ok(Outcome::Fail(_))), "b.txt is missing");
        assert!(go(d.path(), &["--pattern", "(", "--eq", "1"]).is_err());
        assert!(go(d.path(), &["--pattern", "x"]).is_err());
        assert!(go(d.path(), &["--pattern", "x", "--eq", "1", "--min", "1"]).is_err());
    }
}
