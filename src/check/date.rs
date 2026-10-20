//! `date`: the dates in an answer are real and say what they mean — no
//! February 30, no "Friday, October 8, 2026" when that day is a Thursday,
//! and with `--expect` the answer's date is the one the caller worked out.
//! Weekday slips and impossible dates are a pattern of models doing date
//! arithmetic in their heads; a calendar settles them. A slash date that
//! reads two ways (10/08/2026) is only read with `--order mdy|dmy`; under
//! `--expect` it fails without one, because a reader can't tell either.
//! Dates written the same way in a `--source` pass (the answer kept them).

use super::{read_artifact, Args, Fail, Outcome, Run};
use regex::Regex;

const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

const MONTH: &str = r"(jan(?:uary|\.)?|feb(?:ruary|\.)?|mar(?:ch|\.)?|apr(?:il|\.)?|may|june?|july?|aug(?:ust|\.)?|sep(?:t(?:ember)?)?\.?|oct(?:ober|\.)?|nov(?:ember|\.)?|dec(?:ember|\.)?)";
const WEEKDAY: &str = r"(mon(?:day)?|tue(?:s(?:day)?)?|wed(?:nesday)?|thu(?:r(?:s(?:day)?)?)?|fri(?:day)?|sat(?:urday)?|sun(?:day)?)\.?,?\s+";
const DAY: &str = r"(\d{1,2})(?:st|nd|rd|th)?";

fn days_in_month(y: i32, m: u32) -> u32 {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    }
}

fn valid(y: i32, m: u32, d: u32) -> bool {
    (1..=9999).contains(&y) && d >= 1 && d <= days_in_month(y, m)
}

/// Days since 1970-01-01 (Howard Hinnant's days_from_civil).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 0 = Monday … 6 = Sunday; 1970-01-01 was a Thursday.
fn weekday(y: i32, m: u32, d: u32) -> usize {
    (days_from_civil(y, m, d) + 3).rem_euclid(7) as usize
}

fn month_of(word: &str) -> u32 {
    let w = word.trim_end_matches('.').to_lowercase();
    MONTHS.iter().position(|m| m.starts_with(&w[..w.len().min(3)])).map_or(0, |i| i as u32 + 1)
}

fn weekday_of(word: &str) -> Option<usize> {
    let w = word.trim_end_matches(['.', ',']).to_lowercase();
    WEEKDAYS.iter().position(|d| d.to_lowercase().starts_with(&w[..w.len().min(3)]))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Order {
    Mdy,
    Dmy,
}

/// One date as written.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Found {
    pub line: usize,
    pub text: String,
    /// (year, month, day), or None for a slash date that reads two ways.
    pub ymd: Option<(i32, u32, u32)>,
    pub weekday: Option<usize>,
}

fn pattern() -> Result<Regex, String> {
    let p = format!(
        r"(?i)\b(?:{WEEKDAY})?(?:(\d{{4}})-(\d{{2}})-(\d{{2}})(?:\b|T\d)|{MONTH}\s+{DAY},?\s+(\d{{4}})\b|(?:the\s+)?{DAY}\s+(?:of\s+)?{MONTH},?\s+(\d{{4}})\b|(\d{{1,2}})/(\d{{1,2}})/(\d{{4}})\b)"
    );
    Regex::new(&p).map_err(|e| e.to_string())
}

/// Every date in `text`, in order.
pub(crate) fn dates(text: &str, order: Option<Order>) -> Result<Vec<Found>, String> {
    let re = pattern()?;
    let n = |c: &regex::Captures, i: usize| c.get(i).and_then(|m| m.as_str().parse::<i64>().ok());
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        for c in re.captures_iter(line) {
            let weekday = c.get(1).and_then(|m| weekday_of(m.as_str()));
            let ymd = if let (Some(y), Some(m), Some(d)) = (n(&c, 2), n(&c, 3), n(&c, 4)) {
                Some((y, m, d))
            } else if let (Some(mo), Some(d), Some(y)) = (c.get(5), n(&c, 6), n(&c, 7)) {
                Some((y, i64::from(month_of(mo.as_str())), d))
            } else if let (Some(d), Some(mo), Some(y)) = (n(&c, 8), c.get(9), n(&c, 10)) {
                Some((y, i64::from(month_of(mo.as_str())), d))
            } else if let (Some(a), Some(b), Some(y)) = (n(&c, 11), n(&c, 12), n(&c, 13)) {
                match (order, a > 12, b > 12) {
                    (_, true, false) => Some((y, b, a)),
                    (_, false, true) => Some((y, a, b)),
                    (Some(Order::Mdy), _, _) => Some((y, a, b)),
                    (Some(Order::Dmy), _, _) => Some((y, b, a)),
                    (None, _, _) if a == b => Some((y, a, b)),
                    (None, _, _) => None,
                }
            } else {
                continue;
            };
            let ymd = match ymd {
                Some((y, m, d)) => match (i32::try_from(y), u32::try_from(m), u32::try_from(d)) {
                    (Ok(y), Ok(m), Ok(d)) => Some((y, m, d)),
                    _ => Some((0, 0, 0)),
                },
                None => None,
            };
            // An ISO date-time's `T` and first hour digit aren't the date's.
            let whole = c.get(0).map_or("", |m| m.as_str());
            let whole = match whole.char_indices().rev().nth(1) {
                Some((i, 'T' | 't')) if c.get(2).is_some() => &whole[..i],
                _ => whole,
            };
            let text = whole.trim_end_matches([',', ' ']).to_string();
            out.push(Found { line: i + 1, text, ymd, weekday });
        }
    }
    Ok(out)
}

fn iso((y, m, d): (i32, u32, u32)) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

/// What's wrong with one date by itself, if anything.
fn problem(f: &Found) -> Option<String> {
    let (y, m, d) = f.ymd?;
    if !valid(y, m, d) {
        let why = match days_in_month(y, m) {
            0 => "there is no such month".to_string(),
            n if (1..=9999).contains(&y) => format!("{} {y} has {n} days", month_name(m)),
            _ => "the year is out of range".to_string(),
        };
        return Some(format!("line {}: \"{}\" isn't a date ({why})", f.line, f.text));
    }
    let wd = f.weekday?;
    let real = weekday(y, m, d);
    (wd != real).then(|| format!("line {}: \"{}\" — {} is a {}, not a {}", f.line, f.text, iso((y, m, d)), WEEKDAYS[real], WEEKDAYS[wd]))
}

fn month_name(m: u32) -> String {
    let name = MONTHS[(m as usize).saturating_sub(1).min(11)];
    format!("{}{}", name[..1].to_uppercase(), &name[1..])
}

fn parse_iso(s: &str) -> Option<(i32, u32, u32)> {
    let mut p = s.trim().splitn(3, '-');
    let ymd = (p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?);
    valid(ymd.0, ymd.1, ymd.2).then_some(ymd)
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &["all"])?;
    a.only(&["file", "source", "expect", "pattern", "order", "all"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let order = match a.one("order")? {
        None => None,
        Some("mdy") => Some(Order::Mdy),
        Some("dmy") => Some(Order::Dmy),
        Some(o) => return Err(format!("--order {o:?}: give mdy or dmy")),
    };
    let expect = match a.one("expect")? {
        None => None,
        Some(e) => Some(parse_iso(e).ok_or_else(|| format!("--expect {e:?}: give a real date as YYYY-MM-DD"))?),
    };
    let pick = match a.one("pattern")? {
        None => None,
        Some(_) if expect.is_none() => return Err("--pattern picks the date --expect compares: give --expect too".into()),
        Some(p) => {
            let re = Regex::new(p).map_err(|e| format!("--pattern: {e}"))?;
            if re.captures_len() < 2 {
                return Err("--pattern needs a capture group around the date, e.g. \"due: (.+)\"".into());
            }
            Some(re)
        }
    };
    if a.flag("all") && expect.is_none() {
        return Err("--all applies to --expect".into());
    }
    let sources = a.all("source").into_iter().map(|p| std::fs::read_to_string(p).map_err(|e| format!("--source {p}: {e}"))).collect::<Result<Vec<_>, _>>()?;
    let file = a.need("file")?;
    let text = match read_artifact(file) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let found = dates(&text, order)?;
    let kept = |f: &Found| sources.iter().any(|s| s.to_lowercase().contains(&f.text.to_lowercase()));
    let mut bad: Vec<String> = found.iter().filter(|f| !kept(f)).filter_map(problem).collect();
    if let Some(want) = expect {
        // The dates compared: those inside the pattern's capture, or all.
        let compared: Vec<Found> = match &pick {
            Some(re) => {
                let mut v = Vec::new();
                for c in re.captures_iter(&text) {
                    let Some(m) = c.get(1) else { continue };
                    let line = text[..m.start()].matches('\n').count() + 1;
                    let inner = dates(m.as_str(), order)?;
                    if inner.is_empty() {
                        bad.push(format!("line {line}: {:?} isn't a date I can read — write it as {} or {}", m.as_str().trim(), iso(want), spelled(want)));
                    }
                    v.extend(inner.into_iter().map(|f| Found { line, ..f }));
                }
                v
            }
            None => found.clone(),
        };
        if compared.is_empty() && bad.is_empty() {
            let what = pick.as_ref().map_or("no date".to_string(), |r| format!("no date matching {:?}", r.as_str()));
            return Ok(Outcome::Fail(Fail(vec![format!("{file}: {what}; expected {} ({})", iso(want), spelled(want))])));
        }
        if compared.len() > 1 && !a.flag("all") {
            let mut lines = vec![format!("{file}: {} dates; expected exactly one, {}", compared.len(), iso(want))];
            lines.extend(compared.iter().map(|f| format!("line {}: {}", f.line, f.text)));
            return Ok(Outcome::Fail(Fail(lines)));
        }
        for f in &compared {
            match f.ymd {
                None => bad.push(format!("line {}: \"{}\" reads two ways — write it as {} or {}", f.line, f.text, iso(want), spelled(want))),
                Some(got) if got != want && valid(got.0, got.1, got.2) => {
                    let off = days_from_civil(got.0, got.1, got.2) - days_from_civil(want.0, want.1, want.2);
                    let by = match off.abs() {
                        1 => "1 day".to_string(),
                        n => format!("{n} days"),
                    };
                    bad.push(format!("line {}: \"{}\" is {by} {} {} ({})", f.line, f.text, if off > 0 { "after" } else { "before" }, iso(want), spelled(want)));
                }
                _ => {}
            }
        }
    }
    if bad.is_empty() {
        return Ok(Outcome::Pass(match expect {
            Some(w) => format!("the date is {}", iso(w)),
            None => match found.iter().filter(|f| kept(f)).count() {
                0 => format!("{} date(s), all real, weekdays right", found.len()),
                k => format!("{} date(s), real with weekdays right, or ({k}) as the source has them", found.len()),
            },
        }));
    }
    let mut out = vec![bad[0].clone()];
    out.extend(bad.iter().skip(1).take(10).cloned());
    Ok(Outcome::Fail(Fail(out)))
}

/// The answer with its first real date moved a day later, written as
/// YYYY-MM-DD: a planted false answer. (text, what was changed)
pub(crate) fn shifted(answer: &str) -> Option<(String, String)> {
    let found = dates(answer, None).ok()?;
    // The answer's own date: the one on a `date:` line, else the last one
    // written — not the question's date restated at the top, which the
    // gate rightly ignores (a plant there "passes" and blames the gate).
    let lines: Vec<&str> = answer.lines().collect();
    let on_date_line = |f: &&Found| lines.get(f.line - 1).is_some_and(|l| l.trim_start().to_ascii_lowercase().starts_with("date:"));
    let usable = |f: &&Found| f.ymd.is_some_and(|(y, m, d)| valid(y, m, d));
    let f = found.iter().filter(usable).find(on_date_line).or_else(|| found.iter().rfind(usable))?;
    let (y, m, d) = f.ymd?;
    let next = if valid(y, m, d + 1) {
        (y, m, d + 1)
    } else if m < 12 {
        (y, m + 1, 1)
    } else {
        (y + 1, 1, 1)
    };
    // Replaced on its own line, so the same date written earlier stays.
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let line = out.get_mut(f.line - 1)?;
    *line = line.replacen(&f.text, &iso(next), 1);
    let mut text = out.join("\n");
    if answer.ends_with('\n') {
        text.push('\n');
    }
    Some((text, format!("{} → {}", f.text, iso(next))))
}

/// "Thursday, October 8, 2026".
fn spelled((y, m, d): (i32, u32, u32)) -> String {
    format!("{}, {} {d}, {y}", WEEKDAYS[weekday(y, m, d)], month_name(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 1, 1), 10957);
        assert_eq!(days_from_civil(2026, 10, 8), 20734);
        assert_eq!(days_from_civil(1900, 1, 1), -25567);
        assert_eq!([weekday(1970, 1, 1), weekday(2026, 10, 8), weekday(2000, 1, 1), weekday(1900, 1, 1)], [3, 3, 5, 0]);
        assert_eq!([weekday(1969, 12, 31), weekday(2026, 12, 25), weekday(1776, 7, 4)], [2, 4, 3]);
        assert!(valid(2000, 2, 29) && valid(1600, 2, 29) && valid(2024, 2, 29));
        assert!(!valid(1900, 2, 29) && !valid(2026, 2, 29) && !valid(2026, 4, 31) && !valid(2026, 13, 1) && !valid(2026, 1, 0));
    }

    fn ymds(text: &str, order: Option<Order>) -> Vec<Option<(i32, u32, u32)>> {
        dates(text, order).unwrap_or_default().into_iter().map(|f| f.ymd).collect()
    }

    #[test]
    fn dates_are_read_in_the_usual_forms() {
        assert_eq!(ymds("Due 2026-10-08.", None), [Some((2026, 10, 8))]);
        assert_eq!(ymds("on October 8, 2026 and Oct. 9th 2026", None), [Some((2026, 10, 8)), Some((2026, 10, 9))]);
        assert_eq!(ymds("the 8th of October, 2026", None), [Some((2026, 10, 8))]);
        assert_eq!(ymds("8 Sept 2026", None), [Some((2026, 9, 8))]);
        assert_eq!(ymds("13/10/2026 or 10/13/2026", None), [Some((2026, 10, 13)), Some((2026, 10, 13))]);
        assert_eq!(ymds("10/08/2026", None), [None]);
        assert_eq!(ymds("10/08/2026", Some(Order::Mdy)), [Some((2026, 10, 8))]);
        assert_eq!(ymds("10/08/2026", Some(Order::Dmy)), [Some((2026, 8, 10))]);
        assert!(ymds("version 1.2.3, 3 apples, May I ask", None).is_empty());
        let f = &dates("Fri, Oct 8, 2026", None).unwrap_or_default()[0];
        assert_eq!((f.weekday, f.text.as_str()), (Some(4), "Fri, Oct 8, 2026"));
        assert_eq!(ymds("{\"due\":\"2026-10-08T00:00:00Z\"}", None), [Some((2026, 10, 8))], "an ISO date-time");
        assert_eq!(dates("at 2026-02-30T09:00Z", None).unwrap_or_default()[0].text, "2026-02-30");
    }

    #[test]
    fn a_plant_moves_the_date_a_day() {
        assert_eq!(shifted("date: 2026-12-31").map(|s| s.0).as_deref(), Some("date: 2027-01-01"));
        assert_eq!(shifted("Due Thursday, October 8, 2026.").map(|s| s.0).as_deref(), Some("Due 2026-10-09."));
        assert_eq!(shifted("February 28, 2026").map(|s| s.0).as_deref(), Some("2026-03-01"));
        assert!(shifted("10/08/2026, no other date").is_none());
        let a = "Thirty days after Thursday, October 8, 2026 is a Saturday.\ndate: November 7, 2026\n";
        assert_eq!(shifted(a).map(|s| s.0).as_deref(), Some("Thirty days after Thursday, October 8, 2026 is a Saturday.\ndate: 2026-11-08\n"), "the answer's date, not the question's");
        assert_eq!(shifted("From 2026-10-08 to 2026-10-08.\n2026-10-08").map(|s| s.0).as_deref(), Some("From 2026-10-08 to 2026-10-08.\n2026-10-09"), "the last one, on its own line");
    }

    #[test]
    fn wrong_weekdays_and_impossible_dates_are_named() {
        let p = |t: &str| dates(t, None).unwrap_or_default().iter().find_map(problem);
        assert_eq!(p("Friday, October 8, 2026").as_deref(), Some("line 1: \"Friday, October 8, 2026\" — 2026-10-08 is a Thursday, not a Friday"));
        assert_eq!(p("February 30, 2026").as_deref(), Some("line 1: \"February 30, 2026\" isn't a date (February 2026 has 28 days)"));
        assert_eq!(p("2026-02-29").as_deref(), Some("line 1: \"2026-02-29\" isn't a date (February 2026 has 28 days)"));
        assert!(p("Thursday, October 8, 2026; Thu 2026-10-08; Monday the 29th of February 2016").is_none());
        assert!(p("2024-02-29").is_none());
    }
}
