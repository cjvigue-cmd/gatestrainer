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

/// A quote passes when it's in the text verbatim (after `norm`) and long
/// enough to mean something: four words or more.
pub(crate) fn quote_in(text: &str, quote: &str) -> bool {
    let q = norm(quote.trim().trim_matches(|c| c == '"' || c == '\u{201C}' || c == '\u{201D}'));
    q.split_whitespace().count() >= 4 && norm(text).contains(&q)
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
    }
}
