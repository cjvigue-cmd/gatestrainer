//! `quote`: every quotation in an answer is in the source, word for word —
//! the same rule `research` holds its claims to (`research::quote_in`:
//! case, curly quotes, dashes and spacing aside; at least four words).
//! Quoted passages are "…", “…” and Markdown block quotes; shorter ones
//! (a scare-quoted word) aren't quotations and are skipped.

use super::{read_artifact, Args, Fail, Outcome, Run};
use super::text::quote_in;
use regex::Regex;

/// An inline quotation, "…" or “…”, on one line or wrapped onto the next
/// (a line break inside, never a blank line).
pub(crate) const INLINE: &str = "\"((?:[^\"\\n]|\\n[^\"\\n])+)\"|\u{201C}((?:[^\u{201D}\\n]|\\n[^\u{201D}\\n])+)\u{201D}";

/// Below this many words a quoted span isn't treated as a quotation.
const MIN_WORDS: usize = 4;

/// The quotations in `text`: inline quoted spans and block quotes.
fn quotations(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut block: Vec<String> = Vec::new();
    let flush = |block: &mut Vec<String>, out: &mut Vec<String>| {
        if !block.is_empty() {
            out.push(block.join(" "));
            block.clear();
        }
    };
    // A quotation may wrap onto the next line, never across a blank one.
    let Ok(inline) = Regex::new(INLINE) else { return out };
    let mut run: Vec<&str> = Vec::new();
    let take = |run: &mut Vec<&str>, out: &mut Vec<String>| {
        let joined = run.join("\n");
        for c in inline.captures_iter(&joined) {
            if let Some(m) = c.get(1).or_else(|| c.get(2)) {
                out.push(m.as_str().split_whitespace().collect::<Vec<_>>().join(" "));
            }
        }
        run.clear();
    };
    for line in text.lines() {
        if let Some(q) = line.trim_start().strip_prefix('>') {
            take(&mut run, &mut out);
            block.push(q.trim().to_string());
            continue;
        }
        flush(&mut block, &mut out);
        if line.trim().is_empty() {
            take(&mut run, &mut out);
        } else {
            run.push(line);
        }
    }
    take(&mut run, &mut out);
    flush(&mut block, &mut out);
    // Trailing punctuation inside the quote marks is the writer's, not the source's.
    out.into_iter()
        .map(|q| q.trim().trim_end_matches(['.', ',', ';', ':']).to_string())
        .filter(|q| q.split_whitespace().count() >= MIN_WORDS)
        .collect()
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["source", "file", "min-quotes"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let source_path = a.need("source")?;
    let source = std::fs::read_to_string(source_path).map_err(|e| format!("--source {source_path}: {e}"))?;
    let min: usize = a.number("min-quotes")?.unwrap_or(0);
    let file = a.need("file")?;
    let text = match read_artifact(file) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let quotes = quotations(&text);
    let mut bad: Vec<String> = quotes
        .iter()
        .filter(|q| !quote_in(&source, q))
        .map(|q| {
            let shown: String = if q.chars().count() > 100 { format!("{}…", q.chars().take(100).collect::<String>()) } else { q.clone() };
            format!("not in the source word for word: {shown:?}")
        })
        .collect();
    if quotes.len() < min {
        bad.insert(0, format!("{} quotation(s) of {MIN_WORDS}+ words; at least {min} required", quotes.len()));
    }
    if bad.is_empty() {
        Ok(Outcome::Pass(format!("{} quotation(s), all verbatim", quotes.len())))
    } else {
        Ok(Outcome::Fail(Fail(bad)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "The committee found that the bridge was structurally sound.\nRepairs will begin in March and last six weeks.";

    #[test]
    fn finds_quotations() {
        let t = "It said \"the bridge was structurally sound.\" Some call it \"safe\".\n> Repairs will begin\n> in March\nand “last six weeks” too.";
        assert_eq!(quotations(t), ["the bridge was structurally sound", "Repairs will begin in March"]);
        assert_eq!(quotations("It says \"the drug cures every\nknown cancer in humans\"."), ["the drug cures every known cancer in humans"], "a quotation wrapped onto the next line");
        assert!(quotations("A \"stray mark in one paragraph\n\nand another\" in the next.").is_empty(), "never across a blank line");
    }

    fn go(text: &str, extra: &[&str]) -> Run {
        let d = tempfile::tempdir().expect("tempdir");
        let (s, f) = (d.path().join("source.txt"), d.path().join("answer.md"));
        std::fs::write(&s, SOURCE).expect("write");
        std::fs::write(&f, text).expect("write");
        let mut v: Vec<String> = vec!["--source".into(), s.to_string_lossy().into_owned(), "--file".into(), f.to_string_lossy().into_owned()];
        v.extend(extra.iter().map(|s| s.to_string()));
        run(&v)
    }

    #[test]
    fn verbatim_quotes_pass_altered_ones_fail() {
        assert!(matches!(go("Per the report, “The Bridge was  structurally sound.”", &["--min-quotes", "1"]), Ok(Outcome::Pass(_))), "case and spacing aside");
        match go("It said \"the bridge was completely sound\" and \"repairs will begin in March\".", &[]) {
            Ok(Outcome::Fail(Fail(l))) => {
                assert_eq!(l.len(), 1, "{l:?}");
                assert!(l[0].contains("completely sound"), "{l:?}");
            }
            _ => panic!("an altered quote must fail"),
        }
        assert!(matches!(go("No quotes here.", &["--min-quotes", "1"]), Ok(Outcome::Fail(Fail(l))) if l[0].starts_with("0 quotation")));
        assert!(matches!(go("No quotes here.", &[]), Ok(Outcome::Pass(_))));
    }
}
