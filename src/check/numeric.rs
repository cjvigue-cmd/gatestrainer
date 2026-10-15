//! `numeric`: a value in the output is within tolerance of one computed
//! independently (the gate author's, never the model's).

use super::{read_artifact, Args, Fail, Outcome, Run};
use regex::Regex;

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &["all"])?;
    a.only(&["file", "pattern", "expect", "abs", "rel", "all"])?;
    if !a.positional.is_empty() {
        return Err(format!("unexpected argument {:?} (the file goes in --file)", a.positional[0]));
    }
    let file = a.need("file")?;
    let re = Regex::new(a.need("pattern")?).map_err(|e| format!("--pattern: {e}"))?;
    if re.captures_len() < 2 {
        return Err("--pattern needs a capture group around the number, e.g. \"total: ([0-9.eE+-]+)\"".into());
    }
    let expect: f64 = a.number("expect")?.ok_or("--expect is required")?;
    let abs: f64 = a.number("abs")?.unwrap_or(0.0);
    let rel: f64 = a.number("rel")?.unwrap_or(0.0);
    if !expect.is_finite() || !(abs >= 0.0 && rel >= 0.0 && abs.is_finite() && rel.is_finite()) {
        return Err("--expect must be finite and --abs/--rel finite and not negative".into());
    }
    let text = match read_artifact(file) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let found: Vec<(usize, String)> = re
        .captures_iter(&text)
        .filter_map(|c| c.get(1))
        .map(|m| (text[..m.start()].matches('\n').count() + 1, m.as_str().trim().to_string()))
        .collect();
    if found.is_empty() {
        return Ok(Outcome::Fail(Fail(vec![format!("{file}: no match for {:?}", re.as_str())])));
    }
    if found.len() > 1 && !a.flag("all") {
        let mut lines = vec![format!("{file}: {} matches for {:?}; expected exactly one", found.len(), re.as_str())];
        lines.extend(found.iter().map(|(l, v)| format!("line {l}: {v}")));
        return Ok(Outcome::Fail(Fail(lines)));
    }
    let tol = abs + rel * expect.abs();
    let bad: Vec<String> = found
        .iter()
        .filter_map(|(line, raw)| match raw.parse::<f64>() {
            Ok(v) if v.is_finite() && (v - expect).abs() <= tol => None,
            Ok(v) if v.is_finite() => Some(format!("line {line}: {raw} is off by {} (allowed {}, expected {expect})", plain((v - expect).abs()), plain(tol))),
            _ => Some(format!("line {line}: {raw:?} is not a number")),
        })
        .collect();
    if bad.is_empty() {
        Ok(Outcome::Pass(format!("{} value(s) within {} of {expect}", found.len(), plain(tol))))
    } else {
        Ok(Outcome::Fail(Fail(bad)))
    }
}

/// `x` as people write it, to six significant digits: 7.625, 0.00115763,
/// 2500. Scientific notation only for the very small or very large, where
/// a row of zeros would be harder to read.
fn plain(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    let mag = x.abs().log10().floor();
    if !(-6.0..15.0).contains(&mag) {
        return format!("{x:e}");
    }
    let decimals = (5.0 - mag).max(0.0) as usize;
    let s = format!("{x:.decimals$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_as_people_write_them() {
        assert_eq!(plain(7.625), "7.625");
        assert_eq!(plain(0.001157625), "0.00115763");
        assert_eq!(plain(2500.0), "2500");
        assert_eq!(plain(1157.625), "1157.62", "a tie rounds to even");
        assert_eq!(plain(0.0), "0");
        assert_eq!(plain(0.5), "0.5");
        assert_eq!(plain(3e-9), "3e-9");
        assert_eq!(plain(4.2e20), "4.2e20");
    }

    fn go(text: &str, extra: &[&str]) -> Run {
        let d = tempfile::tempdir().expect("tempdir");
        let f = d.path().join("out.txt");
        std::fs::write(&f, text).expect("write");
        let mut v: Vec<String> = vec!["--file".into(), f.to_string_lossy().into_owned()];
        v.extend(extra.iter().map(|s| s.to_string()));
        run(&v)
    }

    #[test]
    fn tolerance_absolute_and_relative() {
        let p = ["--pattern", r"mean = (\S+)", "--expect", "3.14159"];
        assert!(matches!(go("mean = 3.1416\n", &[&p[..], &["--abs", "1e-3"]].concat()), Ok(Outcome::Pass(_))));
        assert!(matches!(go("mean = 3.15\n", &[&p[..], &["--abs", "1e-3"]].concat()), Ok(Outcome::Fail(_))));
        assert!(matches!(go("mean = 3.15\n", &[&p[..], &["--rel", "0.01"]].concat()), Ok(Outcome::Pass(_))));
        assert!(matches!(go("mean = 3.14159\n", &p), Ok(Outcome::Pass(_))), "no tolerance = exact");
    }

    #[test]
    fn exactly_one_match_unless_all() {
        let p = ["--pattern", r"x=(\d+)", "--expect", "4"];
        match go("x=4\nx=5\n", &p) {
            Ok(Outcome::Fail(Fail(l))) => assert!(l[0].contains("2 matches"), "{l:?}"),
            _ => panic!("ambiguity must fail"),
        }
        assert!(matches!(go("x=4\nx=5\n", &[&p[..], &["--all"]].concat()), Ok(Outcome::Fail(_))));
        assert!(matches!(go("x=4\nx=4\n", &[&p[..], &["--all"]].concat()), Ok(Outcome::Pass(_))));
        assert!(matches!(go("nothing\n", &p), Ok(Outcome::Fail(_))));
        match go("x=4\nv=NaN\n", &["--pattern", r"v=(\S+)", "--expect", "1"]) {
            Ok(Outcome::Fail(Fail(l))) => assert!(l[0].contains("line 2"), "{l:?}"),
            _ => panic!("NaN is not a number here"),
        }
    }

    #[test]
    fn misconfigured() {
        assert!(go("", &["--pattern", r"\d+", "--expect", "1"]).is_err(), "no group");
        assert!(go("", &["--pattern", r"(\d+)"]).is_err(), "no expect");
        assert!(go("", &["--pattern", r"(\d+)", "--expect", "1", "--abs", "-1"]).is_err());
    }
}
