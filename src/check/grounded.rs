//! `grounded`: every value extracted into a JSON answer is in the source —
//! nothing invented. A text value must appear in a source word for word
//! (case, curly quotes, dashes and spacing aside, as `quote` reads them),
//! at word boundaries; a number must equal one written there (`1,249.50`
//! in the source grounds `1249.5`); a `YYYY-MM-DD` value must be a date
//! written there in any form `date` reads (`November 20, 2026`). True,
//! false and null are skipped, and so is any field named in `--except`
//! (a category the caller defined, a currency code): those are derived,
//! not copied. A pass says the values are real, not that each sits in
//! the right field.

use super::{read_artifact, Args, Fail, Outcome, Run};
use super::text::norm;
use serde_json::Value;

/// Problems listed after the first.
const SHOWN: usize = 20;

/// What the sources hold, read once.
struct Sources {
    /// `text::norm` of every source, joined.
    text: String,
    numbers: Vec<f64>,
    dates: Vec<(i32, u32, u32)>,
}

impl Sources {
    fn read(texts: &[String]) -> Result<Self, String> {
        let joined = texts.join("\n");
        let number = regex::Regex::new(r"\d[\d,]*(?:\.\d+)?").map_err(|e| e.to_string())?;
        let numbers = number.find_iter(&joined).filter_map(|m| m.as_str().replace(',', "").parse::<f64>().ok()).collect();
        let mut dates = Vec::new();
        // A slash date reads two ways: either reading grounds a value.
        for order in [None, Some(super::date::Order::Mdy), Some(super::date::Order::Dmy)] {
            dates.extend(super::date::dates(&joined, order)?.into_iter().filter_map(|f| f.ymd));
        }
        Ok(Self { text: norm(&joined), numbers, dates })
    }

    /// `value` (already `norm`ed) occurs with no letter or digit glued to
    /// either end.
    fn has_words(&self, value: &str) -> bool {
        if value.is_empty() {
            return true;
        }
        let glued = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
        let first_glues = value.chars().next().is_some_and(char::is_alphanumeric);
        let last_glues = value.chars().last().is_some_and(char::is_alphanumeric);
        self.text.match_indices(value).any(|(at, m)| {
            let before = self.text[..at].chars().last();
            let after = self.text[at + m.len()..].chars().next();
            !(first_glues && glued(before)) && !(last_glues && glued(after))
        })
    }

    fn has_number(&self, n: f64) -> bool {
        self.numbers.iter().any(|x| (x - n).abs() <= f64::EPSILON * n.abs().max(1.0))
    }
}

/// `YYYY-MM-DD` as (year, month, day).
fn iso(s: &str) -> Option<(i32, u32, u32)> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    Some((s[..4].parse().ok()?, s[5..7].parse().ok()?, s[8..].parse().ok()?))
}

/// Why `v` at `path` isn't grounded, or None.
fn ungrounded(path: &str, v: &Value, src: &Sources) -> Option<String> {
    match v {
        Value::Number(n) => {
            let f = n.as_f64()?;
            (!src.has_number(f)).then(|| format!("{path}: {n} isn't a number written in the source"))
        }
        Value::String(s) => {
            let t = s.trim();
            if src.has_words(&norm(t)) {
                return None;
            }
            if let Some(d) = iso(t) {
                return (!src.dates.contains(&d)).then(|| format!("{path}: {t} isn't a date written in the source"));
            }
            if let Ok(f) = t.replace(',', "").parse::<f64>() {
                return (!src.has_number(f)).then(|| format!("{path}: {t:?} isn't a number written in the source"));
            }
            let shown: String = if t.chars().count() > 80 { format!("{}…", t.chars().take(80).collect::<String>()) } else { t.to_string() };
            Some(format!("{path}: {shown:?} isn't in the source word for word"))
        }
        _ => None,
    }
}

/// Walk every leaf; `except` names fields skipped at any depth.
fn walk(path: &str, key: Option<&str>, v: &Value, src: &Sources, except: &[&str], counts: &mut (usize, usize), out: &mut Vec<String>) {
    if key.is_some_and(|k| except.contains(&k)) {
        counts.1 += 1;
        return;
    }
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                walk(&format!("{path}.{k}"), Some(k), x, src, except, counts, out);
            }
        }
        Value::Array(items) => {
            for (i, x) in items.iter().enumerate() {
                walk(&format!("{path}[{i}]"), key, x, src, except, counts, out);
            }
        }
        Value::Bool(_) | Value::Null => counts.1 += 1,
        leaf => {
            counts.0 += 1;
            out.extend(ungrounded(path, leaf, src));
        }
    }
}

/// Every ungrounded value in `answer`, and (checked, skipped) counts.
pub(crate) fn problems(answer: &Value, sources: &[String], except: &[&str]) -> Result<(Vec<String>, (usize, usize)), String> {
    let src = Sources::read(sources)?;
    let mut out = Vec::new();
    let mut counts = (0, 0);
    walk("$", None, answer, &src, except, &mut counts, &mut out);
    Ok((out, counts))
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["file", "source", "except"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let sources = a.all("source").into_iter().map(|p| std::fs::read_to_string(p).map_err(|e| format!("--source {p}: {e}"))).collect::<Result<Vec<_>, _>>()?;
    if sources.is_empty() {
        return Err("give at least one --source".into());
    }
    let except: Vec<&str> = a.one("except")?.unwrap_or_default().split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    let file = a.need("file")?;
    let text = match read_artifact(file) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let answer: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return Ok(Outcome::Fail(Fail(vec![format!("{file} isn't JSON: {e}")]))),
    };
    let (found, (checked, skipped)) = problems(&answer, &sources, &except)?;
    if found.is_empty() {
        return Ok(Outcome::Pass(format!("{checked} value(s), all found in the source ({skipped} skipped: true/false/null or --except)")));
    }
    let n = found.len();
    let mut lines: Vec<String> = found.into_iter().take(SHOWN + 1).collect();
    lines[0] = format!("{} — copy values as the source writes them{}", lines[0], if n > 1 { format!(" ({n} values not found)") } else { String::new() });
    Ok(Outcome::Fail(Fail(lines)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SRC: &str = "Invoice INV-2026-0417 from Acme Supply Co., dated October 20, 2026.\nTotal due: $1,249.50 by 11/20/2026. 3 brass desk lamps at $83.30 each.";

    fn found(v: Value, except: &[&str]) -> Vec<String> {
        problems(&v, &[SRC.to_string()], except).expect("reads").0
    }

    #[test]
    fn copied_values_numbers_and_dates_are_grounded() {
        let v = json!({"invoice": "INV-2026-0417", "vendor": "Acme Supply Co.", "date": "2026-10-20", "due": "2026-11-20",
                       "total": 1249.5, "items": [{"description": "brass desk lamps", "quantity": 3, "unit": 83.3}], "paid": false, "note": null});
        assert!(found(v, &[]).is_empty(), "{:?}", found(json!({}), &[]));
        assert!(found(json!({"total": "1,249.50", "vendor": "ACME SUPPLY CO."}), &[]).is_empty(), "as written; case aside");
    }

    #[test]
    fn invented_values_are_named_with_their_path() {
        assert_eq!(found(json!({"total": 1250}), &[]), ["$.total: 1250 isn't a number written in the source"]);
        assert_eq!(found(json!({"vendor": "Acme Supplies"}), &[]), ["$.vendor: \"Acme Supplies\" isn't in the source word for word"]);
        assert_eq!(found(json!({"due": "2026-11-30"}), &[]), ["$.due: 2026-11-30 isn't a date written in the source"]);
        assert_eq!(found(json!({"items": [{"quantity": 4}]}), &[]), ["$.items[0].quantity: 4 isn't a number written in the source"]);
        assert_eq!(found(json!({"vendor": "Acm"}), &[]).len(), 1, "a piece of a word isn't the word");
        assert_eq!(found(json!({"currency": "USD", "vendor": "Acme"}), &["currency"]), Vec::<String>::new(), "--except skips derived fields");
    }

    #[test]
    fn cli_exit_codes() {
        let d = tempfile::tempdir().expect("tmp");
        let p = |n: &str, t: &str| {
            let f = d.path().join(n);
            std::fs::write(&f, t).expect("write");
            f.to_string_lossy().into_owned()
        };
        let (src, good, bad, broken) = (p("s.txt", SRC), p("g.json", r#"{"total": 1249.5}"#), p("b.json", r#"{"total": 1250}"#), p("x.json", "{"));
        let run_ = |args: &[&str]| run(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(matches!(run_(&["--file", &good, "--source", &src, "--except", ""]), Ok(Outcome::Pass(_))));
        assert!(matches!(run_(&["--file", &bad, "--source", &src]), Ok(Outcome::Fail(_))));
        assert!(matches!(run_(&["--file", &broken, "--source", &src]), Ok(Outcome::Fail(_))));
        assert!(run_(&["--file", &good]).is_err(), "no source");
    }
}
