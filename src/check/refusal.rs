//! `refusal`: the answer does the task instead of deflecting it — no "As an
//! AI…", "I'm sorry, but I can't…", "I'm unable to…", "I don't have access
//! to…". A deflection uses no numbers, no quotations and few words, so it
//! can slip through checks built for real answers (a summary's quote,
//! number and length checks all pass an apology). Phrases the source itself
//! has pass (a rewrite of a message that says "I'm sorry, I can't attend").

use super::{read_artifact, Args, Fail, Outcome, Run};
use regex::Regex;

const PATTERNS: &[&str] = &[
    r"\bas an ai(?: language model| assistant| model)?\b",
    r"\bi(?:'m| am) (?:just |only )?an? (?:ai|language model|large language model)\b",
    r"\bi(?:'m| am)? ?(?:sorry|afraid),? (?:but )?i (?:can't|cannot|can not|won't|am unable to|'m unable to|am not able to)\b",
    r"\bi (?:can't|cannot|can not|won't|am unable to|'m unable to|am not able to|'m not able to) (?:help|assist|provide|do|write|create|comply|complete|summari[sz]e|answer|fulfil+)\b",
    r"\bi (?:don't|do not) have (?:the ability|access|the capability) to\b",
    r"\bi must (?:decline|refuse)\b",
    r"\bit (?:would|wouldn't) be (?:inappropriate|unethical|irresponsible) (?:for me )?to\b",
];

fn norm(s: &str) -> String {
    s.to_lowercase().replace('’', "'")
}

/// Every deflection in `text` no source has: (line, phrase).
pub(crate) fn refusals(text: &str, sources: &[String]) -> Vec<(usize, String)> {
    let res: Vec<Regex> = PATTERNS.iter().filter_map(|p| Regex::new(p).ok()).collect();
    let sources: Vec<String> = sources.iter().map(|s| norm(s)).collect();
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let l = norm(line);
        if let Some(m) = res.iter().filter_map(|r| r.find(&l)).min_by_key(|m| m.start()) {
            if !sources.iter().any(|s| s.contains(m.as_str())) {
                out.push((i + 1, m.as_str().to_string()));
            }
        }
    }
    out
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["file", "source"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let sources = a.all("source").into_iter().map(|p| std::fs::read_to_string(p).map_err(|e| format!("--source {p}: {e}"))).collect::<Result<Vec<_>, _>>()?;
    let file = a.need("file")?;
    let text = match read_artifact(file) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let found = refusals(&text, &sources);
    let Some((l, p)) = found.first() else { return Ok(Outcome::Pass("does the task: no deflections".into())) };
    let mut out = vec![format!("line {l} deflects the task (\"{p}\") — do the task as asked; if something stops you, say exactly what")];
    out.extend(found.iter().skip(1).take(10).map(|(l, p)| format!("line {l}: \"{p}\"")));
    Ok(Outcome::Fail(Fail(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrases(text: &str, source: &str) -> Vec<String> {
        refusals(text, &[source.to_string()]).into_iter().map(|(_, p)| p).collect()
    }

    #[test]
    fn deflections_are_caught() {
        assert_eq!(phrases("As an AI language model, I cannot summarize documents.", ""), ["as an ai language model"]);
        assert_eq!(phrases("I'm sorry, but I can't help with that.", ""), ["i'm sorry, but i can't"]);
        assert_eq!(phrases("I am unable to provide a summary of this file.", ""), ["i am unable to provide"]);
        assert_eq!(phrases("I don't have access to the attached report.", ""), ["i don't have access to"]);
        assert_eq!(phrases("I must decline.", ""), ["i must decline"]);
    }

    #[test]
    fn real_answers_and_the_sources_own_words_pass() {
        assert!(phrases("The colony grew to 412 pairs after rats were removed.", "").is_empty());
        assert!(phrases("Customers can't log in when the token expires.", "").is_empty(), "describing a can't isn't refusing");
        assert!(phrases("Hi Sam, I'm sorry, but I can't attend on Friday.", "hi sam, im sorry but i cant attend. I'm sorry, but I can't attend").is_empty());
    }
}
