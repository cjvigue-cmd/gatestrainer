//! `claims`: an answer doesn't claim work that didn't happen — "I ran the
//! tests and they pass", "this has been tested", "verified to work",
//! "production-ready". A model writing an answer runs nothing: the gate does
//! the running, after. So the claim is false whatever the code is like, and
//! a reader (or a later model) might take it as evidence. `--allow` names a
//! phrase the task really wants (a README that quotes a CI badge).

use super::{read_artifact, Args, Fail, Outcome, Run};
use regex::Regex;

/// Claims of work done, matched case-insensitively.
const PATTERNS: &[&str] = &[
    r"\bi(?:'ve| have)? (?:tested|ran|run|executed|verified|compiled|checked|confirmed|validated)\b",
    r"\b(?:all |the )?tests? (?:all )?(?:pass(?:es|ed)?|are passing|ran successfully|succeeded)\b",
    r"\b(?:has|have) been (?:fully |thoroughly )?(?:tested|verified|validated)\b",
    r"\b(?:fully|thoroughly|well|properly) tested\b",
    r"\bverified (?:to work|that it works|working)\b",
    r"\b(?:i can )?confirm(?:ed)? (?:that )?(?:it|this|the code) works\b",
    r"\bworks (?:perfectly|correctly|as expected) (?:now|in (?:my|all) tests?)\b",
    r"\bproduction[- ]ready\b",
    r"\bbattle[- ]tested\b",
];

/// Every claim of work done in `text` that `allow` doesn't excuse: (line, phrase).
pub(crate) fn claims(text: &str, allow: &[String]) -> Vec<(usize, String)> {
    let res: Vec<Regex> = PATTERNS.iter().filter_map(|p| Regex::new(&format!("(?i){p}")).ok()).collect();
    let allow: Vec<String> = allow.iter().map(|a| a.to_lowercase()).collect();
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.replace('’', "'");
        if let Some(m) = res.iter().filter_map(|r| r.find(&line)).min_by_key(|m| m.start()) {
            let phrase = m.as_str().to_string();
            if !allow.iter().any(|a| phrase.to_lowercase().contains(a.as_str()) || line.to_lowercase().contains(a.as_str())) {
                out.push((i + 1, phrase));
            }
        }
    }
    out
}

/// The report a gate gives for an answer that claims work done, or None:
/// what a gate's `prechecks: ["claims"]` runs.
pub(crate) fn report(text: &str, file: &str) -> Option<String> {
    let found = claims(text, &[]);
    let (l, p) = found.first()?;
    let more = match found.len() - 1 {
        0 => String::new(),
        n => format!(" (and {n} more)"),
    };
    let mut out = vec![format!(
        "claims: FAILED — {file} line {l} says \"{p}\"{more}, but nothing was run while the answer was written: the gate runs it after. Leave out claims of testing or verification"
    )];
    out.extend(found.iter().skip(1).take(10).map(|(l, p)| format!("  {file} line {l}: \"{p}\"")));
    Some(out.join("\n"))
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["file", "allow"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let files = a.all("file");
    if files.is_empty() {
        return Err("give the answer to check: --file F (repeatable)".into());
    }
    let allow: Vec<String> = a.all("allow").into_iter().map(str::to_string).collect();
    let mut lines = Vec::new();
    for file in files {
        let text = match read_artifact(file) {
            Ok(t) => t,
            Err(f) => return Ok(Outcome::Fail(f)),
        };
        lines.extend(claims(&text, &allow).into_iter().map(|(l, p)| format!("{file} line {l}: \"{p}\"")));
    }
    if lines.is_empty() {
        return Ok(Outcome::Pass("no claims of work done".into()));
    }
    let mut out = vec![format!("{} — nothing was run while the answer was written; leave out claims of testing or verification", lines[0])];
    out.extend(lines.into_iter().skip(1).take(10));
    Ok(Outcome::Fail(Fail(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrases(text: &str) -> Vec<String> {
        claims(text, &[]).into_iter().map(|(_, p)| p).collect()
    }

    #[test]
    fn claims_of_work_done_are_caught() {
        assert_eq!(phrases("I ran the tests and they all pass."), ["I ran"]);
        assert_eq!(phrases("# Tested: all tests pass"), ["all tests pass"]);
        assert_eq!(phrases("This function has been thoroughly tested."), ["has been thoroughly tested"]);
        assert_eq!(phrases("// verified to work on edge cases"), ["verified to work"]);
        assert_eq!(phrases("The code is production-ready."), ["production-ready"]);
        assert_eq!(phrases("I’ve verified the output."), ["I've verified"]);
        assert_eq!(claims("a\nb\nI tested it", &[]), [(3, "I tested".to_string())]);
    }

    #[test]
    fn plain_answers_and_allowed_phrases_pass() {
        assert!(phrases("def add(a, b):\n    return a + b  # sums the two\n").is_empty());
        assert!(phrases("Run the tests with `pytest`; they check the empty case.").is_empty(), "describing tests isn't claiming they passed");
        assert!(phrases("The test checks that 1900 is not a leap year.").is_empty());
        assert!(claims("CI badge: tests passing", &["tests passing".into()]).is_empty());
    }
}
