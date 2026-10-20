//! `overclaim`: an answer claims no more than its source — no sweeping word
//! ("all", "never", "completely", "eliminates", "free" …) that the source
//! doesn't use itself. The pattern it catches: a model reads a narrow fact
//! ("$0.0000 spent today") and states a wide one ("circumvents all API
//! cost"). It reads wording, not meaning, so a pass is a necessary
//! condition only: no unsupported sweeping words, never "the answer is true".
//!
//! Not counted: a word the source uses (in any of its forms), one inside a
//! quotation that is in the source word for word, one just after a negation
//! ("not all", "isn't free": those narrow a claim), and `--allow`ed words.

use super::{read_artifact, Args, Fail, Outcome, Run};
use super::text::quote_in;
use regex::Regex;

/// Words that widen a claim, each with the forms that count as the same
/// word. A family is supported if the source uses any of its forms.
pub(crate) const FAMILIES: &[&[&str]] = &[
    &["all"],
    &["every"],
    &["everything"],
    &["everyone", "everybody"],
    &["entire", "entirely"],
    &["whole", "wholly"],
    &["always"],
    &["never"],
    &["none"],
    &["nothing"],
    &["nobody", "no one"],
    &["zero"],
    &["complete", "completely"],
    &["total", "totally"],
    &["fully"],
    &["absolute", "absolutely"],
    &["permanent", "permanently"],
    &["100%"],
    &["eliminate", "eliminates", "eliminated", "eliminating", "elimination"],
    &["circumvent", "circumvents", "circumvented", "circumventing"],
    &["bypass", "bypasses", "bypassed", "bypassing"],
    &["guarantee", "guarantees", "guaranteed", "guaranteeing"],
    &["prove", "proves", "proved", "proven", "proving"],
    &["solve", "solves", "solved", "solving"],
    &["unlimited"],
    &["infinite", "infinitely"],
    &["free"],
];

/// Lines shown before "… and N more".
const SHOWN: usize = 15;

/// The words of `text` (lowercase, apostrophes and `%` kept), each with its
/// byte span.
fn words(text: &str) -> Vec<(usize, usize, String)> {
    let Ok(re) = Regex::new(r"[\p{L}\p{N}][\p{L}\p{N}'’%]*") else { return Vec::new() };
    re.find_iter(text).map(|m| (m.start(), m.end(), m.as_str().to_lowercase().replace('’', "'"))).collect()
}

/// The family a word (or the two-word phrase it starts) belongs to.
fn family_at(ws: &[(usize, usize, String)], i: usize) -> Option<(usize, usize)> {
    let one = ws[i].2.as_str();
    let two = ws.get(i + 1).map(|n| format!("{one} {}", n.2));
    FAMILIES.iter().enumerate().find_map(|(f, forms)| {
        if two.as_deref().is_some_and(|t| forms.contains(&t)) {
            Some((f, 2))
        } else if forms.contains(&one) {
            Some((f, 1))
        } else {
            None
        }
    })
}

/// The families a text uses anywhere.
pub(crate) fn families_in(text: &str) -> Vec<usize> {
    let ws = words(text);
    let mut out: Vec<usize> = (0..ws.len()).filter_map(|i| family_at(&ws, i).map(|(f, _)| f)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// A negation just before word `i` ("not all", "isn't free", "cannot
/// always"), in the same sentence: "Setup isn't slow. Everything works"
/// negates nothing about everything.
fn negated(text: &str, ws: &[(usize, usize, String)], i: usize) -> bool {
    let neg = |w: &str| matches!(w, "not" | "no" | "cannot" | "without") || w.ends_with("n't");
    (1..=2).any(|back| i >= back && neg(&ws[i - back].2) && !text[ws[i - back].1..ws[i].0].contains(['.', ';', ':', '!', '?']))
}

/// Byte spans of quotations in `text` that are in a source word for word.
fn sourced_quotes(text: &str, sources: &[String]) -> Vec<(usize, usize)> {
    let Ok(re) = Regex::new(super::quote::INLINE) else { return Vec::new() };
    re.captures_iter(text)
        .filter_map(|c| c.get(1).or_else(|| c.get(2)))
        .filter(|m| sources.iter().any(|s| quote_in(s, m.as_str())))
        .map(|m| (m.start(), m.end()))
        .collect()
}

/// Every sweeping word in `answer` its sources don't support: (line, word,
/// context). The second value is how many listed words were checked.
pub(crate) fn overclaims(answer: &str, sources: &[String], allow: &[String]) -> (Vec<(usize, String, String)>, usize) {
    let supported: Vec<usize> = sources.iter().flat_map(|s| families_in(s)).collect();
    let allowed: Vec<usize> = allow.iter().flat_map(|w| families_in(w)).collect();
    let quotes = sourced_quotes(answer, sources);
    let ws = words(answer);
    let (mut found, mut checked) = (Vec::new(), 0);
    let mut i = 0;
    while i < ws.len() {
        let Some((f, len)) = family_at(&ws, i) else {
            i += 1;
            continue;
        };
        checked += 1;
        let (start, end) = (ws[i].0, ws[i + len - 1].1);
        let quoted = quotes.iter().any(|&(a, b)| start >= a && end <= b);
        if !supported.contains(&f) && !allowed.contains(&f) && !quoted && !negated(answer, &ws, i) {
            let line = answer[..start].matches('\n').count() + 1;
            let from = answer[..start].char_indices().rev().nth(30).map_or(0, |(p, _)| p);
            let to = answer[end..].char_indices().nth(30).map_or(answer.len(), |(p, _)| end + p);
            let context = answer[from..to].replace('\n', " ");
            found.push((line, answer[start..end].to_string(), format!("{}{}{}", if from > 0 { "…" } else { "" }, context.trim(), if to < answer.len() { "…" } else { "" })));
        }
        i += len;
    }
    (found, checked)
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["file", "source", "allow"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let paths = a.all("source");
    if paths.is_empty() {
        return Err("give the source the answer must stay within (--source FILE, repeatable)".into());
    }
    let sources = paths.iter().map(|p| std::fs::read_to_string(p).map_err(|e| format!("--source {p}: {e}"))).collect::<Result<Vec<_>, _>>()?;
    let allow: Vec<String> = a.all("allow").into_iter().map(str::to_string).collect();
    let text = match read_artifact(a.need("file")?) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let (found, checked) = overclaims(&text, &sources, &allow);
    if found.is_empty() {
        return Ok(Outcome::Pass(format!("no sweeping words beyond the source ({checked} checked)")));
    }
    // The first finding leads, so a one-line report names the word.
    let (l, w, c) = &found[0];
    let more = match found.len() - 1 {
        0 => String::new(),
        n => format!(" (and {n} more)"),
    };
    let mut out = vec![format!("\"{w}\" on line {l} claims more than the source says{more}: \"{c}\" — say only what the source supports, or narrow the claim")];
    out.extend(found.iter().skip(1).take(SHOWN).map(|(l, w, c)| format!("line {l}: \"{w}\" — \"{c}\"")));
    if found.len() > SHOWN {
        out.push(format!("… and {} more", found.len() - SHOWN));
    }
    Ok(Outcome::Fail(Fail(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flagged(answer: &str, source: &str) -> Vec<String> {
        overclaims(answer, &[source.to_string()], &[]).0.into_iter().map(|(_, w, _)| w).collect()
    }

    #[test]
    fn the_screenshot_case_fails_on_both_words() {
        let source = "COST · TODAY  $0.0000 spent today on paid seats";
        assert_eq!(flagged("You made a program that circumvents all API cost.", source), ["circumvents", "all"]);
        let (found, checked) = overclaims("You made a program that circumvents all API cost.", &[source.into()], &[]);
        assert_eq!((found[0].0, checked), (1, 2));
        assert!(found[0].2.contains("circumvents all API cost"), "{found:?}");
    }

    #[test]
    fn what_the_source_says_negations_quotes_and_allowed_words_pass() {
        assert!(flagged("Not all API cost goes away; the local model isn't free to run.", "$0 spent").is_empty(), "negated");
        assert!(flagged("All tests passed, and the build completed.", "all 12 tests passed").is_empty(), "the source says all");
        assert!(flagged("It eliminated the bug.", "this eliminates the bug").is_empty(), "another form of the same word");
        let q = "The report says \"every site was checked twice in June\".";
        assert!(flagged(q, "Every site was checked twice in June, by two teams.").is_empty(), "a sourced quotation");
        assert_eq!(flagged("It says \"every site is perfect and fine\".", "some sites were checked"), ["every"], "a quotation not in the source counts");
        assert!(overclaims("This is always on.", &["x".into()], &["always".into()]).0.is_empty(), "--allow");
        assert_eq!(flagged("No one noticed; nothing broke.", "it worked"), ["No one", "nothing"]);
        assert_eq!(flagged("Coverage is 100% now.", "coverage rose to 90%"), ["100%"]);
        assert_eq!(flagged("Setup isn't slow. Everything is supported.", "setup takes a minute; most things work"), ["Everything"], "a negation doesn't cross a sentence end");
    }
}
