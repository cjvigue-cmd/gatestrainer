//! `placeholder`: the answer is finished — no "rest of the code unchanged",
//! no "your implementation here", no `todo!()`, `raise NotImplementedError`
//! or `pass  # TODO`, no line that is only an ellipsis, no TODO/FIXME left in.
//! Hidden tests catch most unfinished code, but not a stub in a branch they
//! never reach; and an elided answer can't be applied at all. `--allow`
//! names a pattern the task really wants (a TODO the spec asks for).

use super::{read_artifact, Args, Fail, Outcome, Run};
use regex::Regex;

/// The patterns, each with what it is; matched case-insensitively per line.
const PATTERNS: &[(&str, &str)] = &[
    (r"\b(?:rest|remainder) of (?:the )?(?:code|file|function|implementation|class|module|tests?)\b", "an elision"),
    (r"\b(?:existing|previous|other|same|original) (?:code|logic|implementation|methods|functions)(?: (?:here|goes here|unchanged|remains|stays))", "an elision"),
    (r"\b(?:unchanged|omitted|elided|truncated) (?:for brevity|from (?:above|before)|here)\b", "an elision"),
    (r"\((?:implementation|code|details|logic) omitted\)", "an elision"),
    (r"\b(?:your|the) (?:code|implementation|logic) (?:goes )?here\b", "an empty slot"),
    (r"\b(?:insert|add|put|implement) (?:your |the )?(?:code|logic|implementation) here\b", "an empty slot"),
    (r"^\s*(?://|#|--|/\*)?\s*(?:\.\.\.|…)\s*(?:\*/)?\s*$", "a line that is only an ellipsis"),
    (r"\btodo!\s*\(|\bunimplemented!\s*\(", "a stub (todo!/unimplemented!)"),
    (r"\braise NotImplementedError\b", "a stub (NotImplementedError)"),
    (r#"\bthrow new Error\(\s*["'`](?:not implemented|todo|unimplemented)"#, "a stub (throws not implemented)"),
    (r"^\s*pass\s*#.*\b(?:todo|placeholder|stub|implement)\b", "a stub (pass)"),
    (r"(?://|#|--|/\*|<!--)\s*(?:TODO|FIXME|XXX)\b", "a TODO/FIXME left in"),
];

/// Lines shown before "… and N more".
const SHOWN: usize = 15;

/// Every unfinished spot in `text`: (line, the line, what it is).
pub(crate) fn placeholders(text: &str, allow: &[String]) -> Vec<(usize, String, &'static str)> {
    let res: Vec<(Regex, &str)> = PATTERNS.iter().filter_map(|(p, w)| Regex::new(&format!("(?i){p}")).ok().map(|r| (r, *w))).collect();
    let allow: Vec<String> = allow.iter().map(|a| a.to_lowercase()).collect();
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let lower = line.to_lowercase();
        if allow.iter().any(|a| lower.contains(a.as_str())) {
            continue;
        }
        if let Some((_, what)) = res.iter().find(|(r, _)| r.is_match(line)) {
            out.push((i + 1, line.trim().chars().take(100).collect(), *what));
        }
    }
    out
}

/// The report a gate gives for an unfinished answer (`file` names it), or
/// None when it's finished: what a gate's `prechecks: ["placeholder"]` runs.
pub(crate) fn report(text: &str, file: &str) -> Option<String> {
    let found = placeholders(text, &[]);
    let (l, s, w) = found.first()?;
    let more = match found.len() - 1 {
        0 => String::new(),
        n => format!(" (and {n} more)"),
    };
    let mut out = vec![format!("placeholder: FAILED — {file} line {l} is {w}{more}: \"{s}\" — give the whole answer, finished")];
    out.extend(found.iter().skip(1).take(SHOWN).map(|(l, s, w)| format!("  {file} line {l}: {w}: \"{s}\"")));
    Some(out.join("\n"))
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["file", "allow"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let allow: Vec<String> = a.all("allow").into_iter().map(str::to_string).collect();
    let mut found = Vec::new();
    for file in a.all("file") {
        let text = match read_artifact(file) {
            Ok(t) => t,
            Err(f) => return Ok(Outcome::Fail(f)),
        };
        found.extend(placeholders(&text, &allow).into_iter().map(|(l, s, w)| (file.to_string(), l, s, w)));
    }
    if a.all("file").is_empty() {
        return Err("give the answer to check: --file F (repeatable)".into());
    }
    if found.is_empty() {
        return Ok(Outcome::Pass("finished: no elisions, stubs or TODOs".into()));
    }
    let (f, l, s, w) = &found[0];
    let more = match found.len() - 1 {
        0 => String::new(),
        n => format!(" (and {n} more)"),
    };
    let mut out = vec![format!("{f} line {l} is {w}{more}: \"{s}\" — give the whole answer, finished")];
    out.extend(found.iter().skip(1).take(SHOWN).map(|(f, l, s, w)| format!("{f} line {l}: {w}: \"{s}\"")));
    if found.len() > SHOWN + 1 {
        out.push(format!("… and {} more", found.len() - SHOWN - 1));
    }
    Ok(Outcome::Fail(Fail(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<&'static str> {
        placeholders(text, &[]).into_iter().map(|(_, _, w)| w).collect()
    }

    #[test]
    fn unfinished_answers_are_caught() {
        assert_eq!(kinds("fn a() {}\n// ... rest of the code unchanged\n"), ["an elision"]);
        assert_eq!(kinds("def f(x):\n    # your implementation here\n    pass\n"), ["an empty slot"]);
        assert_eq!(kinds("class A:\n    ...\n"), ["a line that is only an ellipsis"]);
        assert_eq!(kinds("fn f() -> u32 { todo!() }"), ["a stub (todo!/unimplemented!)"]);
        assert_eq!(kinds("    raise NotImplementedError"), ["a stub (NotImplementedError)"]);
        assert_eq!(kinds("throw new Error('Not implemented');"), ["a stub (throws not implemented)"]);
        assert_eq!(kinds("    pass  # TODO handle the empty case"), ["a stub (pass)"]);
        assert_eq!(kinds("x = 1  # FIXME: wrong for negatives"), ["a TODO/FIXME left in"]);
        assert_eq!(kinds("SELECT 1; -- TODO join orders"), ["a TODO/FIXME left in"]);
        assert_eq!(kinds("// (implementation omitted)"), ["an elision"]);
    }

    #[test]
    fn finished_answers_and_allowed_patterns_pass() {
        let code = "def total(xs):\n    \"\"\"Sum of xs, or 0.\"\"\"\n    return sum(xs)  # sums all of them\n\nprint(total([1, 2]))\n";
        assert!(kinds(code).is_empty());
        assert!(kinds("let s = \"to do list\"; // a list of things to do").is_empty(), "'to do' as words isn't TODO");
        assert!(kinds("args = (a, ...rest)").is_empty(), "spread syntax isn't an elision");
        assert!(placeholders("# TODO: keep this marker (the spec asks for it)", &["todo: keep".into()]).is_empty());
    }
}
