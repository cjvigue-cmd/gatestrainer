//! Text matching shared by the quote-aware checks.

/// Lowercase, one kind of quote / dash, single spaces: formatting noise
/// shouldn't fail a faithful quote, and nothing else is forgiven.
pub(crate) fn norm(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201B}' | '`' => '\'',
            '\u{201C}' | '\u{201D}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            c => c,
        })
        .collect::<String>()
        .to_lowercase();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `value` occurs in `text` with no letter or digit glued to either end:
/// "effective" isn't in "ineffective". Both already `norm`ed.
pub(crate) fn has_words(text: &str, value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let glued = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let first_glues = value.chars().next().is_some_and(char::is_alphanumeric);
    let last_glues = value.chars().last().is_some_and(char::is_alphanumeric);
    text.match_indices(value).any(|(at, m)| {
        let before = text[..at].chars().last();
        let after = text[at + m.len()..].chars().next();
        !(first_glues && glued(before)) && !(last_glues && glued(after))
    })
}

/// A quote passes when it's in the text verbatim (after `norm`), at word
/// boundaries, and long enough to mean something: four words or more.
pub(crate) fn quote_in(text: &str, quote: &str) -> bool {
    let q = norm(quote.trim().trim_matches(|c| c == '"' || c == '\u{201C}' || c == '\u{201D}'));
    q.split_whitespace().count() >= 4 && has_words(&norm(text), &q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_match_through_formatting_noise_only() {
        let src = "The gate decides by exit code — nothing else.";
        assert!(quote_in(src, "decides by exit code — nothing"));
        assert!(quote_in(src, "DECIDES BY EXIT   CODE - nothing"), "case, spacing and dash kind are forgiven");
        assert!(quote_in(src, "“decides by exit code — nothing”"), "the quotation marks around a quote are not part of it");
        assert!(!quote_in(src, "decides by exit codes"), "a changed word fails");
        assert!(!quote_in(src, "exit code"), "under four words isn't a quotation");
        let t = "The drug is ineffective for most patients in the trial.";
        assert!(!quote_in(t, "effective for most patients in the trial"), "a quote cut from inside a word reverses it");
        assert!(quote_in(t, "ineffective for most patients in the"));
    }
}
