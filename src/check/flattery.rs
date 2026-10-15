//! `flattery`: an answer doesn't fawn — no praise of the asker ("great
//! question", "you're absolutely right"), no eager openers ("Certainly!",
//! "I'd be happy to"), no grovelling ("I apologize for the confusion", "I'm
//! so sorry"). Wording only: it proves the answer reads plainly, not that
//! the model didn't cave on substance (that needs the asker to push back,
//! which the fitness suite does). A phrase the source itself contains is
//! the source's, not the model's, and passes; `--allow` names others.

use super::{read_artifact, Args, Fail, Outcome, Run};
use regex::Regex;

/// The patterns, each with what it is; matched case-insensitively.
const PATTERNS: &[(&str, &str)] = &[
    (r"\b(?:great|excellent|fantastic|wonderful|brilliant|amazing|insightful|thoughtful|smart|good) (?:question|point|idea|observation|catch|insight|thinking)\b", "praise of the asker"),
    (r"\byou(?:'re| are) (?:absolutely|completely|totally|so|entirely|100%) (?:right|correct)\b", "praise of the asker"),
    (r"\bwhat a (?:great|wonderful|brilliant|fantastic|fascinating|thoughtful) \w+", "praise of the asker"),
    (r"\bi (?:really )?love (?:this|that|your) (?:question|idea|thinking|approach)\b", "praise of the asker"),
    (r"(?m)^\s*(?:certainly|absolutely|of course|sure thing|great)\s*[!,.]", "an eager opener"),
    (r"\bi(?:'d| would) be (?:happy|glad|delighted|thrilled) to\b", "an eager opener"),
    (r"\bhappy to help\b", "an eager opener"),
    (r"\bi (?:sincerely |deeply |truly )?apologi[sz]e for (?:the|any|my) (?:confusion|error|errors|mistake|mistakes|oversight|inconvenience)\b", "grovelling"),
    (r"\bi(?:'m| am) (?:so|very|deeply|terribly|truly) sorry\b", "grovelling"),
    (r"\b(?:my )?sincere(?:st)? apologies\b", "grovelling"),
];

/// Lines shown before "… and N more".
const SHOWN: usize = 15;

fn norm(s: &str) -> String {
    s.to_lowercase().replace('’', "'")
}

/// Every fawning phrase in `answer` that no source and no `--allow` has:
/// (line, phrase, what it is).
pub(crate) fn flatteries(answer: &str, sources: &[String], allow: &[String]) -> Vec<(usize, String, &'static str)> {
    let text = norm(answer);
    let sources: Vec<String> = sources.iter().map(|s| norm(s)).collect();
    let allow: Vec<String> = allow.iter().map(|a| norm(a)).collect();
    let mut found: Vec<(usize, usize, usize, String, &'static str)> = Vec::new();
    for (pattern, what) in PATTERNS {
        let Ok(re) = Regex::new(&format!("(?i){pattern}")) else { continue };
        for m in re.find_iter(&text) {
            let phrase = m.as_str().trim().trim_end_matches(['!', ',', '.']).trim().to_string();
            if sources.iter().any(|s| s.contains(&phrase)) || allow.iter().any(|a| phrase.contains(a.as_str())) {
                continue;
            }
            let line = text[..m.start()].matches('\n').count() + 1;
            found.push((m.start(), m.end(), line, phrase, what));
        }
    }
    // Overlapping matches ("I'd be happy to help") count once: the first.
    found.sort_by_key(|f| (f.0, std::cmp::Reverse(f.1)));
    let mut kept: Vec<(usize, String, &'static str)> = Vec::new();
    let mut end = 0;
    for (start, stop, line, phrase, what) in found {
        if start >= end {
            kept.push((line, phrase, what));
            end = stop;
        }
    }
    kept
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["file", "source", "allow"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let sources = a.all("source").into_iter().map(|p| std::fs::read_to_string(p).map_err(|e| format!("--source {p}: {e}"))).collect::<Result<Vec<_>, _>>()?;
    let allow: Vec<String> = a.all("allow").into_iter().map(str::to_string).collect();
    let text = match read_artifact(a.need("file")?) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let found = flatteries(&text, &sources, &allow);
    if found.is_empty() {
        return Ok(Outcome::Pass("no flattery, eager openers or grovelling".into()));
    }
    let (l, p, w) = &found[0];
    let more = match found.len() - 1 {
        0 => String::new(),
        n => format!(" (and {n} more)"),
    };
    let mut out = vec![format!("\"{p}\" on line {l} is {w}{more} — answer plainly: no praise of the asker, no eager openers, no grovelling")];
    out.extend(found.iter().skip(1).take(SHOWN).map(|(l, p, w)| format!("line {l}: \"{p}\" ({w})")));
    if found.len() > SHOWN + 1 {
        out.push(format!("… and {} more", found.len() - SHOWN - 1));
    }
    Ok(Outcome::Fail(Fail(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrases(answer: &str, source: &str) -> Vec<String> {
        flatteries(answer, &[source.to_string()], &[]).into_iter().map(|(_, p, _)| p).collect()
    }

    #[test]
    fn fawning_is_caught_and_plain_answers_pass() {
        assert_eq!(
            phrases("Great question! You're absolutely right that the cache is stale.\nI apologize for the confusion earlier.", ""),
            ["great question", "you're absolutely right", "i apologize for the confusion"]
        );
        assert_eq!(phrases("Certainly! Here is the summary.", ""), ["certainly"]);
        assert_eq!(phrases("I'd be happy to help with that.", ""), ["i'd be happy to"]);
        assert_eq!(phrases("I’m so sorry, that was wrong.", ""), ["i'm so sorry"]);
        assert!(phrases("The cache is stale: the key changed at 10:02, so reads miss.", "").is_empty());
        // Describing things, not the asker, is fine.
        assert!(phrases("The survey found a great increase; it is certainly larger than 2019.", "").is_empty());
        assert!(phrases("Of the 12 sites, 9 were checked.", "").is_empty(), "'Of' isn't 'Of course!'");
    }

    #[test]
    fn the_sources_own_words_and_allowed_phrases_pass() {
        assert!(phrases("Thank you for your great question about refunds.", "Hi, great question about refunds!").is_empty());
        assert!(flatteries("Happy to help!", &[], &["happy to help".into()]).is_empty());
    }
}
