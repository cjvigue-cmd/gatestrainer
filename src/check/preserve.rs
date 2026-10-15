//! `preserve`: what must survive an edit did — every number, URL, code
//! span, placeholder and named term in FROM appears in TO at least as often,
//! and (with `headings`) the outline's levels are unchanged. The reverse
//! direction (FROM = a summary, TO = its source) says the summary invented
//! no number.

use super::{read_artifact, Args, Fail, Outcome, Run};
use regex::Regex;
use std::collections::BTreeMap;

const KINDS: [&str; 5] = ["numbers", "urls", "code", "placeholders", "headings"];

fn count<'a>(items: impl Iterator<Item = &'a str>) -> BTreeMap<&'a str, usize> {
    let mut m = BTreeMap::new();
    for i in items {
        *m.entry(i).or_insert(0) += 1;
    }
    m
}

/// Numbers standing alone (not part of `x2` or `v1_2`): `42`, `3.14`, `1,000`.
fn numbers(text: &str) -> Vec<&str> {
    let re = match Regex::new(r"\d+(?:[.,]\d+)*") {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    re.find_iter(text)
        .filter(|m| !word(text[..m.start()].chars().next_back()) && !word(text[m.end()..].chars().next()))
        .map(|m| m.as_str())
        .collect()
}

fn urls(text: &str) -> Vec<&str> {
    let Ok(re) = Regex::new(r#"https?://[^\s<>"'`)\]]+"#) else { return Vec::new() };
    re.find_iter(text).map(|m| m.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?'])).collect()
}

/// Inline code spans and fenced blocks, each as written.
fn code(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(span) = Regex::new(r"`([^`\n]+)`") else { return out };
    let mut block: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match block.take() {
                Some(lines) => out.push(lines.join("\n")),
                None => block = Some(Vec::new()),
            }
            continue;
        }
        match &mut block {
            Some(lines) => lines.push(line),
            None => out.extend(span.captures_iter(line).map(|c| c[1].to_string())),
        }
    }
    out
}

fn placeholders(text: &str) -> Vec<&str> {
    let Ok(re) = Regex::new(r"\{\{\s*[A-Za-z0-9_.]*\s*\}\}|\{[A-Za-z0-9_.]*\}|\$\{[^}\n]*\}|%\([A-Za-z_]+\)[sdif]|%[sdif]") else {
        return Vec::new();
    };
    re.find_iter(text).map(|m| m.as_str()).collect()
}

/// The levels of the Markdown headings outside fences, in order.
fn outline(text: &str) -> Vec<usize> {
    let mut fenced = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let level = t.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&level) && t[level..].starts_with(' ') {
            out.push(level);
        }
    }
    out
}

fn missing(kind: &str, from: BTreeMap<&str, usize>, to: &BTreeMap<&str, usize>, bad: &mut Vec<String>) {
    for (item, n) in from {
        let m = to.get(item).copied().unwrap_or(0);
        if m < n {
            let shown: String = if item.chars().count() > 60 { format!("{}…", item.chars().take(60).collect::<String>()) } else { item.to_string() };
            bad.push(format!("{kind} {shown:?}: {n} in the original, {m} in the result"));
        }
    }
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["from", "to", "keep", "terms"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let keep: Vec<&str> = a.need("keep")?.split(',').map(str::trim).filter(|k| !k.is_empty()).collect();
    if let Some(k) = keep.iter().find(|k| !KINDS.contains(k)) {
        return Err(format!("--keep {k:?}: one of {}", KINDS.join(", ")));
    }
    let terms: Vec<&str> = a.one("terms")?.map(|t| t.split(',').map(str::trim).filter(|t| !t.is_empty()).collect()).unwrap_or_default();
    if keep.is_empty() && terms.is_empty() {
        return Err("--keep needs at least one kind (or give --terms)".into());
    }
    let (from_path, to_path) = (a.need("from")?, a.need("to")?);
    let from = match read_artifact(from_path) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let to = match read_artifact(to_path) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let mut bad = Vec::new();
    let mut checked = 0usize;
    for kind in &keep {
        match *kind {
            "numbers" => {
                let f = count(numbers(&from).into_iter());
                checked += f.values().sum::<usize>();
                missing("number", f, &count(numbers(&to).into_iter()), &mut bad);
            }
            "urls" => {
                let f = count(urls(&from).into_iter());
                checked += f.values().sum::<usize>();
                missing("URL", f, &count(urls(&to).into_iter()), &mut bad);
            }
            "code" => {
                let (cf, ct) = (code(&from), code(&to));
                let f = count(cf.iter().map(String::as_str));
                checked += f.values().sum::<usize>();
                missing("code", f, &count(ct.iter().map(String::as_str)), &mut bad);
            }
            "placeholders" => {
                let f = count(placeholders(&from).into_iter());
                checked += f.values().sum::<usize>();
                missing("placeholder", f, &count(placeholders(&to).into_iter()), &mut bad);
            }
            _ => {
                let (of, ot) = (outline(&from), outline(&to));
                checked += of.len();
                if of != ot {
                    let show = |o: &[usize]| o.iter().map(|l| "#".repeat(*l)).collect::<Vec<_>>().join(" ");
                    bad.push(format!("headings changed: {} in the original, {} in the result", show(&of), show(&ot)));
                }
            }
        }
    }
    for t in &terms {
        let n = from.matches(t).count();
        checked += n;
        let m = to.matches(t).count();
        if m < n {
            bad.push(format!("term {t:?}: {n} in the original, {m} in the result"));
        }
    }
    if bad.is_empty() {
        Ok(Outcome::Pass(format!("{checked} items kept ({}{})", keep.join(", "), if terms.is_empty() { "" } else { ", terms" })))
    } else {
        Ok(Outcome::Fail(Fail(bad)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extractors() {
        assert_eq!(numbers("v2 has 3 parts, 4.25% and 1,000 items; x_9 no"), ["3", "4.25", "1,000"]);
        assert_eq!(urls("see https://a.b/c?d=1. and (http://x.y/z)"), ["https://a.b/c?d=1", "http://x.y/z"]);
        assert_eq!(code("use `f()` here\n```sh\nrun it\n```\n"), ["f()", "run it"]);
        assert_eq!(placeholders("Hi {name}, {{ count }} %s %(user)s ${x}"), ["{name}", "{{ count }}", "%s", "%(user)s", "${x}"]);
        assert_eq!(outline("# A\ntext\n## B\n```\n# not\n```\n### C\n#tag"), [1, 2, 3]);
    }

    fn go(from: &str, to: &str, extra: &[&str]) -> Run {
        let d = tempfile::tempdir().expect("tempdir");
        let (f, t) = (d.path().join("from.md"), d.path().join("to.md"));
        std::fs::write(&f, from).expect("write");
        std::fs::write(&t, to).expect("write");
        let mut v: Vec<String> = vec!["--from".into(), f.to_string_lossy().into_owned(), "--to".into(), t.to_string_lossy().into_owned()];
        v.extend(extra.iter().map(|s| s.to_string()));
        run(&v)
    }

    #[test]
    fn an_edit_that_keeps_everything_passes() {
        let from = "# Plan\nWe ship 3 builds by 2026-11-01 for {team}. Run `make all`. See https://x.y/plan.\n";
        let to = "# Plan\nBy 2026-11-01 we will ship 3 builds for {team}: run `make all` (details: https://x.y/plan).\n";
        assert!(matches!(go(from, to, &["--keep", "numbers,urls,code,placeholders,headings"]), Ok(Outcome::Pass(_))));
    }

    #[test]
    fn a_changed_number_or_lost_item_fails() {
        let from = "# Plan\n## Dates\nShip 3 builds for {team} via `make all`.\n";
        let to = "# Plan\nShip 4 builds for the team via make.\n";
        match go(from, to, &["--keep", "numbers,code,placeholders,headings", "--terms", "Plan,builds"]) {
            Ok(Outcome::Fail(Fail(l))) => {
                assert!(l.iter().any(|x| x.starts_with("number \"3\": 1 in the original, 0")), "{l:?}");
                assert!(l.iter().any(|x| x.starts_with("code \"make all\"")), "{l:?}");
                assert!(l.iter().any(|x| x.starts_with("placeholder \"{team}\"")), "{l:?}");
                assert!(l.iter().any(|x| x.starts_with("headings changed: # ## in the original, # in the result")), "{l:?}");
                assert!(!l.iter().any(|x| x.contains("term")), "terms kept: {l:?}");
            }
            _ => panic!("expected failures"),
        }
    }

    #[test]
    fn reverse_direction_catches_invented_numbers() {
        let source = "Revenue rose 12% to 4.2 million in 2025.";
        assert!(matches!(go("Revenue grew 12% to 4.2 million (2025).", source, &["--keep", "numbers"]), Ok(Outcome::Pass(_))));
        assert!(matches!(go("Revenue grew 15% to 4.2 million.", source, &["--keep", "numbers"]), Ok(Outcome::Fail(_))));
    }

    #[test]
    fn misconfigured() {
        assert!(go("", "", &["--keep", "colours"]).is_err());
        assert!(go("", "", &["--keep", ""]).is_err());
    }
}
