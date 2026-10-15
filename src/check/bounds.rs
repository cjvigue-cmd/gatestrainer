//! `bounds`: a text's size is within limits — words, lines, characters,
//! and (against another file) the ratio of words, so an edit can't quietly
//! drop half the text or pad it out.

use super::{read_artifact, Args, Fail, Outcome, Run};

/// A minimum, a maximum, or both.
type Range = (Option<f64>, Option<f64>);

/// `MIN:MAX`, either side optional (`50:`, `:200`, `10:20`).
fn range(name: &str, spec: &str) -> Result<Range, String> {
    let (lo, hi) = spec.split_once(':').ok_or_else(|| format!("--{name} {spec:?}: use MIN:MAX (either side may be empty)"))?;
    let num = |s: &str| -> Result<Option<f64>, String> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        s.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0).map(Some).ok_or_else(|| format!("--{name}: {s:?} is not a number ≥ 0"))
    };
    let (lo, hi) = (num(lo)?, num(hi)?);
    if lo.is_none() && hi.is_none() {
        return Err(format!("--{name} {spec:?}: give a minimum, a maximum or both"));
    }
    if let (Some(l), Some(h)) = (lo, hi) {
        if l > h {
            return Err(format!("--{name} {spec:?}: the minimum is above the maximum"));
        }
    }
    Ok((lo, hi))
}

fn fmt(v: f64) -> String {
    ((v * 100.0).round() / 100.0).to_string()
}

fn within(what: &str, v: f64, (lo, hi): Range, bad: &mut Vec<String>) {
    if lo.is_some_and(|l| v < l) || hi.is_some_and(|h| v > h) {
        let want = match (lo, hi) {
            (Some(l), Some(h)) => format!("{l}–{h}"),
            (Some(l), None) => format!("at least {l}"),
            (None, Some(h)) => format!("at most {h}"),
            (None, None) => String::new(),
        };
        // Models count badly: say how far off, not just the limit.
        let by = match (lo, hi) {
            (Some(l), _) if v < l => format!(" ({} short)", fmt(l - v)),
            (_, Some(h)) if v > h => format!(" ({} over)", fmt(v - h)),
            _ => String::new(),
        };
        bad.push(format!("{what} is {}; want {want}{by}", fmt(v)));
    }
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["file", "words", "lines", "chars", "ratio-of", "ratio"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let file = a.need("file")?;
    let limits: Vec<(&str, Range)> = ["words", "lines", "chars"]
        .into_iter()
        .filter_map(|k| a.one(k).transpose().map(|v| v.and_then(|s| range(k, s)).map(|r| (k, r))))
        .collect::<Result<_, _>>()?;
    let ratio = match (a.one("ratio-of")?, a.one("ratio")?) {
        (Some(of), Some(r)) => Some((of, range("ratio", r)?)),
        (None, None) => None,
        _ => return Err("--ratio-of and --ratio go together".into()),
    };
    if limits.is_empty() && ratio.is_none() {
        return Err("give at least one of --words, --lines, --chars, --ratio-of/--ratio".into());
    }
    let text = match read_artifact(file) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let words = text.split_whitespace().count();
    let mut bad = Vec::new();
    for (k, r) in &limits {
        let v = match *k {
            "words" => words,
            "lines" => text.lines().count(),
            _ => text.chars().count(),
        };
        within(k, v as f64, *r, &mut bad);
    }
    let mut summary = format!("{words} words");
    if let Some((of, r)) = ratio {
        let base = std::fs::read_to_string(of).map_err(|e| format!("--ratio-of {of}: {e}"))?.split_whitespace().count();
        if base == 0 {
            return Err(format!("--ratio-of {of} has no words"));
        }
        let q = words as f64 / base as f64;
        summary.push_str(&format!(", {:.2}× the original", q));
        within("the length against the original", q, r, &mut bad);
    }
    if bad.is_empty() {
        Ok(Outcome::Pass(summary))
    } else {
        Ok(Outcome::Fail(Fail(bad)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn go(text: &str, extra: &[&str]) -> Run {
        let d = tempfile::tempdir().expect("tempdir");
        let (f, o) = (d.path().join("out.md"), d.path().join("orig.md"));
        std::fs::write(&f, text).expect("write");
        std::fs::write(&o, "one two three four five six seven eight nine ten").expect("write");
        let mut v: Vec<String> = vec!["--file".into(), f.to_string_lossy().into_owned()];
        for e in extra {
            v.push(if *e == "ORIG" { o.to_string_lossy().into_owned() } else { e.to_string() });
        }
        run(&v)
    }

    #[test]
    fn limits_and_ratio() {
        assert!(matches!(go("a b c", &["--words", "2:5"]), Ok(Outcome::Pass(_))));
        assert!(matches!(go("a b c", &["--words", ":2"]), Ok(Outcome::Fail(Fail(l))) if l[0] == "words is 3; want at most 2 (1 over)"));
        assert!(matches!(go("a\nb", &["--lines", "3:", "--chars", ":10"]), Ok(Outcome::Fail(Fail(l))) if l.len() == 1));
        assert!(matches!(go("a b c d e", &["--ratio-of", "ORIG", "--ratio", "0.4:1.2"]), Ok(Outcome::Pass(_))));
        assert!(matches!(go("a b", &["--ratio-of", "ORIG", "--ratio", "0.4:1.2"]), Ok(Outcome::Fail(_))), "0.2× is too short");
        assert!(matches!(go("", &["--words", "1:"]), Ok(Outcome::Fail(_))), "empty fails a minimum");
    }

    #[test]
    fn misconfigured() {
        assert!(go("x", &[]).is_err());
        assert!(go("x", &["--words", "5"]).is_err());
        assert!(go("x", &["--words", ":"]).is_err());
        assert!(go("x", &["--words", "9:2"]).is_err());
        assert!(go("x", &["--ratio", "1:2"]).is_err());
    }
}
