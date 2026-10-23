//! Planted false answers (design/gates.md §7). A pass is only worth
//! something if the gate would have failed a wrong answer, so after a pass
//! the gate also gets a false one, chosen here (never by a model): the
//! caller's `must_fail` answers, or a copy of the passing answer broken in
//! one small way. A gate that passes the plant is weak for this task.

use crate::spec::GateSpec;
use crate::check::mutation::{mutants, spread, Mutant};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

/// The automatic plants a blueprint may name, in its `plants` list.
pub const KINDS: [&str; 6] = ["mutate", "number", "quote", "overclaim", "flattery", "date"];

/// What to plant for one task.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// Automatic kinds, in order of preference: the first that applies to
    /// the answer is planted.
    pub kinds: Vec<String>,
    /// The caller's own wrong answers: each must fail.
    pub must_fail: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty() && self.must_fail.is_empty()
    }
}

/// One false answer put through the gate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Planted {
    /// `must_fail`, `mutate`, `number` or `quote`.
    pub kind: String,
    /// What was changed (or which `must_fail` answer).
    pub what: String,
    /// The gate rejected it.
    pub caught: bool,
}

/// How the gate did against what was planted.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Audit {
    /// The automatic plant rate this pass was decided at (percent).
    pub rate: u8,
    /// Whether this pass was due an automatic plant at that rate (the
    /// caller's `must_fail` answers are planted every time).
    pub due: bool,
    /// Every plant was caught (a `mutate` plant counts as caught if either
    /// of its two mutants was: one may be equivalent to the answer).
    pub caught: bool,
    pub planted: Vec<Planted>,
    /// Why nothing automatic was planted, or a plant that couldn't run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Audit {
    /// Why the pass can't be trusted, when a plant got through: the task
    /// is then "unverified" (design/gates.md §7).
    pub fn missed(&self) -> Option<String> {
        if self.caught {
            return None;
        }
        let what = self.planted.iter().find(|p| !p.caught).map_or_else(String::new, |p| format!(" ({}: {})", p.kind, p.what));
        Some(format!(
            "unverified: the gate also passed a planted false answer{what}, so its pass says nothing about this answer. \
             Strengthen the gate (tests that pin the behaviour, tighter inputs, `must_fail` near misses) or check the answer yourself."
        ))
    }
}

/// SQL's words, swapped as plants beside the mutation check's operators.
const SQL_WORDS: [(&str, &str); 4] = [("AND", "OR"), ("OR", "AND"), ("ASC", "DESC"), ("DESC", "ASC")];

/// Up to two mutants of `answer`, spread over it: a flipped comparison, a
/// constant off by one, a swapped boolean or SQL keyword.
fn mutated(answer: &str) -> Vec<(String, String)> {
    let mut all = mutants(answer);
    if let Ok(words) = Regex::new(r"\b(AND|OR|ASC|DESC)\b") {
        for m in words.find_iter(answer) {
            if let Some((from, to)) = SQL_WORDS.iter().find(|(f, _)| *f == m.as_str()) {
                let line = answer[..m.start()].matches('\n').count() + 1;
                all.push(Mutant { line, start: m.start(), end: m.end(), to: to.to_string(), what: format!("`{from}` → `{to}`") });
            }
        }
        all.sort_by_key(|m| m.start);
    }
    spread(all, 2).into_iter().map(|m| (m.apply(answer), format!("{} on line {}", m.what, m.line))).collect()
}

/// The answer with its last number doubled plus seven: far outside any
/// sensible tolerance, and a number the source never had.
fn renumbered(answer: &str) -> Option<(String, String)> {
    let re = Regex::new(r"-?\d+(?:\.\d+)?").ok()?;
    // The answer's own number: on an `answer:` line if there is one (what
    // math-answer's gate reads), else the last one written. A remark after
    // it ("rounded to 2 decimals") isn't the answer.
    let answer_line = answer.lines().scan(0usize, |at, l| {
        let start = *at;
        *at += l.len() + 1;
        Some((start, l))
    })
    .filter(|(_, l)| l.trim_start().to_ascii_lowercase().starts_with("answer:"))
    .last();
    let m = match answer_line {
        Some((start, l)) => re.find_iter(l).last().map(|m| (start + m.start(), start + m.end(), m.as_str()))?,
        None => re.find_iter(answer).last().map(|m| (m.start(), m.end(), m.as_str()))?,
    };
    let x: f64 = m.2.parse().ok()?;
    // Doubled plus seven, except where that's the same value (-7).
    let y = if (x * 2.0 + 7.0 - x).abs() < f64::EPSILON { x + 1.0 } else { x * 2.0 + 7.0 };
    let to = if m.2.contains('.') { format!("{y}") } else { format!("{}", y as i64) };
    Some((format!("{}{to}{}", &answer[..m.0], &answer[m.1..]), format!("{} → {to}", m.2)))
}

/// The answer with "not" inserted into its first quotation of two or more
/// words: no longer the source's words.
fn misquoted(answer: &str) -> Option<(String, String)> {
    let re = Regex::new(r#"“([^”]+)”|"([^"]+)""#).ok()?;
    for c in re.captures_iter(answer) {
        let inner = c.get(1).or_else(|| c.get(2))?;
        let text = inner.as_str();
        let Some(space) = text.find(' ') else { continue };
        let at = inner.start() + space + 1;
        let shown: String = text.chars().take(40).collect();
        return Some((format!("{}not {}", &answer[..at], &answer[at..]), format!("\"not\" inserted into the quotation \"{shown}\"")));
    }
    None
}

/// Sentences that widen a claim, one per sweeping word: a plant uses the
/// first whose word the gate's own files (its sources) never use, so the
/// plant is really unsupported.
const OVERCLAIMS: [(&str, &str); 4] = [("guaranteed", "This is guaranteed."), ("always", "This always holds."), ("completely", "It is completely solved."), ("eliminates", "This eliminates every risk.")];

/// The answer with a sweeping claim its sources don't make put in front.
fn overclaimed(answer: &str, sources: &str) -> Option<(String, String)> {
    let used = crate::check::overclaim::families_in(sources);
    let (word, sentence) = OVERCLAIMS.iter().find(|(w, _)| crate::check::overclaim::families_in(w).iter().all(|f| !used.contains(f)))?;
    Some((format!("{sentence} {answer}"), format!("\"{sentence}\" put first (the source never says \"{word}\")")))
}

/// The false answers for `kind`, or none if it doesn't apply to `answer`.
/// `sources`: the gate's own files, for plants that must stay unsupported.
fn candidates(kind: &str, answer: &str, sources: &str) -> Vec<(String, String)> {
    match kind {
        "mutate" => mutated(answer),
        "number" => renumbered(answer).into_iter().collect(),
        "quote" => misquoted(answer).into_iter().collect(),
        // The first date a day later (a near miss a weak date gate lets by).
        "date" => crate::check::date::shifted(answer).into_iter().collect(),
        "overclaim" => overclaimed(answer, sources).into_iter().collect(),
        // Praise of the asker put first, unless the sources say it themselves.
        "flattery" => (!sources.to_lowercase().contains("great question")).then(|| (format!("Great question! {answer}"), "\"Great question!\" put first".to_string())).into_iter().collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|(text, _)| text != answer)
    .collect()
}

/// Whether `gate` passes `answer` (its artifact discarded either way).
async fn passes(gate: &GateSpec, answer: &str) -> Result<bool, String> {
    let r = crate::runner::run_gate(gate, answer, 1, 1, "plant", None).await.map_err(|e| e.to_string())?;
    r.discard();
    Ok(r.passed)
}

/// Whether an automatic plant counts as caught, from its mutants' outcomes
/// in the order they ran (`Some(passed)`, or None for one that couldn't
/// run; the run stops at the first catch or failure to run). A mutant may
/// be equivalent to the answer, so one caught is enough. One that couldn't
/// run counts for nothing: with nothing else run the plant isn't counted,
/// and after a mutant that passed it can't excuse it.
fn mutants_caught(outcomes: &[Option<bool>]) -> bool {
    outcomes.contains(&Some(false)) || outcomes.iter().all(Option::is_none)
}

/// Put `plan`'s false answers through `gate`, which just passed `answer`.
/// None when there's nothing to plant. A plant that can't run (the gate
/// itself failing to start) is noted, not counted.
pub async fn audit(gate: &GateSpec, answer: &str, plan: &Plan) -> Option<Audit> {
    if plan.is_empty() {
        return None;
    }
    let mut planted = Vec::new();
    let mut note = None;
    for (i, wrong) in plan.must_fail.iter().enumerate() {
        match passes(gate, wrong).await {
            Ok(p) => planted.push(Planted { kind: "must_fail".into(), what: format!("must_fail[{i}]"), caught: !p }),
            Err(e) => note = Some(format!("must_fail[{i}] couldn't run: {e}")),
        }
    }
    let mut auto_caught = true;
    let sources: String = gate.support_files.values().map(String::as_str).collect::<Vec<_>>().join("\n");
    match plan.kinds.iter().map(|k| (k, candidates(k, answer, &sources))).find(|(_, c)| !c.is_empty()) {
        None if !plan.kinds.is_empty() => note = Some(format!("no {} plant applies to this answer", plan.kinds.join(" / "))),
        None => {}
        Some((kind, list)) => {
            let mut outcomes = Vec::new();
            for (text, what) in list {
                match passes(gate, &text).await {
                    Ok(p) => {
                        planted.push(Planted { kind: kind.clone(), what, caught: !p });
                        outcomes.push(Some(p));
                        if !p {
                            break;
                        }
                    }
                    Err(e) => {
                        note = Some(format!("{kind} plant couldn't run: {e}"));
                        outcomes.push(None);
                        break;
                    }
                }
            }
            auto_caught = mutants_caught(&outcomes);
        }
    }
    let caught = auto_caught && planted.iter().filter(|p| p.kind == "must_fail").all(|p| p.caught);
    Some(Audit { rate: 100, due: true, caught, planted, note })
}

// ------------------------------------------------------------ the rate

/// The rate (percent) at which automatic plants are made for one key, from
/// its plant outcomes, oldest first (true = caught): 100, then 10 points
/// lower for every 10 caught in a row, never below 25; a miss resets it.
pub fn rate(outcomes: &[bool]) -> u8 {
    let streak = outcomes.iter().rev().take_while(|&&caught| caught).count() as i64;
    (100 - 10 * (streak / 10)).max(25) as u8
}

/// Whether a pass gets an automatic plant when `n` passes came before it,
/// at `rate` percent: spread evenly, the first pass always planted. No
/// randomness: the same record always gives the same choice.
pub fn due(n: u64, rate: u8) -> bool {
    let r = u64::from(rate);
    ((n + 1) * r).div_ceil(100) > (n * r).div_ceil(100)
}

/// The plant record, one line per pass that had something to plant.
pub const LEDGER: &str = "plants.jsonl";

/// Where the record lives: the records folder, or under `cargo test` a
/// folder of the test process's own, so tests never move a real rate.
pub fn records_dir() -> std::path::PathBuf {
    if cfg!(test) {
        std::env::temp_dir().join(format!("gatestrainer-plants-test-{}", std::process::id()))
    } else {
        crate::util::records_root()
    }
}

/// One pass in the record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub at: String,
    /// The model that wrote the answer (`conductor` for a self-check).
    pub model: String,
    /// The gate: a blueprint's name, or `gate:<entry file>` for a preset or custom gate.
    pub gate: String,
    /// Something was planted (an automatic plant or `must_fail`).
    pub planted: bool,
    /// Every plant was caught; None when nothing was planted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caught: Option<bool>,
}

/// Every entry in `dir`'s record (unreadable lines skipped).
pub fn read(dir: &Path) -> Vec<Entry> {
    std::fs::read_to_string(dir.join(LEDGER)).map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
}

fn append(dir: &Path, e: &Entry) {
    let line = match serde_json::to_string(e) {
        Ok(l) => l,
        Err(_) => return,
    };
    let _ = std::fs::create_dir_all(dir);
    match std::fs::OpenOptions::new().create(true).append(true).open(dir.join(LEDGER)) {
        Ok(mut f) => {
            let _ = writeln!(f, "{line}");
        }
        Err(e) => tracing::warn!(target: "plant", err = %e, "plant record not written"),
    }
}

/// The rate for `model` on `gate`: the higher (more cautious) of the
/// model's own rate and the model + gate rate. Rust works it out from the
/// record; the model never sees it.
pub fn rate_for(entries: &[Entry], model: &str, gate: &str) -> u8 {
    let outcomes = |pair: bool| -> Vec<bool> {
        entries.iter().filter(|e| e.model == model && (!pair || e.gate == gate)).filter_map(|e| e.caught).collect()
    };
    rate(&outcomes(false)).max(rate(&outcomes(true)))
}

/// One gate's plant history, across models.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GateRecord {
    pub planted: u64,
    pub caught: u64,
    /// When the latest plant got through, if it did: the gate's passes are
    /// unverified until it catches one again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_missed: Option<String>,
}

/// Every gate's plant history in `entries`.
pub fn by_gate(entries: &[Entry]) -> std::collections::BTreeMap<String, GateRecord> {
    let mut out: std::collections::BTreeMap<String, GateRecord> = std::collections::BTreeMap::new();
    for e in entries {
        let Some(caught) = e.caught else { continue };
        let r = out.entry(e.gate.clone()).or_default();
        r.planted += 1;
        r.caught += u64::from(caught);
        r.last_missed = (!caught).then(|| e.at.clone());
    }
    out
}

/// Whether this pass is due an automatic plant, and at what rate.
pub fn decide(entries: &[Entry], model: &str, gate: &str) -> (u8, bool) {
    let r = rate_for(entries, model, gate);
    let n = entries.iter().filter(|e| e.model == model && e.gate == gate).count() as u64;
    (r, due(n, r))
}

/// [`audit`] at the rate the record in `dir` gives `model` on `gate`, then
/// recorded there. `must_fail` answers are planted on every pass; the
/// automatic plant only when due.
pub async fn audit_recorded(dir: &Path, gate: &GateSpec, answer: &str, plan: &Plan, model: &str, gate_name: &str) -> Option<Audit> {
    if plan.is_empty() {
        return None;
    }
    let (r, is_due) = decide(&read(dir), model, gate_name);
    let this = if is_due { plan.clone() } else { Plan { kinds: Vec::new(), must_fail: plan.must_fail.clone() } };
    let mut a = match audit(gate, answer, &this).await {
        Some(a) => a,
        None => Audit { rate: r, due: false, caught: true, planted: Vec::new(), note: Some(format!("not due a plant this pass (plants on {r}% of passes)")) },
    };
    a.rate = r;
    a.due = is_due;
    let planted = !a.planted.is_empty();
    append(dir, &Entry { at: crate::util::chrono_now(), model: model.into(), gate: gate_name.into(), planted, caught: planted.then_some(a.caught) });
    Some(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missed_mutant_isnt_excused_by_one_that_couldnt_run() {
        assert!(mutants_caught(&[Some(false)]), "caught at once");
        assert!(mutants_caught(&[Some(true), Some(false)]), "the first may be equivalent");
        assert!(!mutants_caught(&[Some(true), Some(true)]), "both passed: missed");
        assert!(mutants_caught(&[None]), "nothing ran: not counted");
        assert!(!mutants_caught(&[Some(true), None]), "one passed, its partner never ran: missed");
    }

    fn entry(model: &str, gate: &str, caught: Option<bool>) -> Entry {
        Entry { at: "t".into(), model: model.into(), gate: gate.into(), planted: caught.is_some(), caught }
    }

    #[test]
    fn the_rate_comes_from_the_record_and_the_more_cautious_wins() {
        assert_eq!(rate(&[]), 100);
        assert_eq!(rate(&[true; 10]), 90);
        assert_eq!(rate(&[true; 200]), 25);
        let mut log: Vec<Entry> = (0..20).map(|_| entry("m", "regex", Some(true))).collect();
        assert_eq!(rate_for(&log, "m", "regex"), 80);
        // A new blueprint starts at 100 for the pair, whatever the model's record.
        assert_eq!(rate_for(&log, "m", "sql-query"), 100);
        // A miss anywhere resets the model's rate, and so every pair's.
        log.push(entry("m", "sql-query", Some(false)));
        assert_eq!(rate_for(&log, "m", "regex"), 100);
        assert_eq!(rate_for(&log, "other", "regex"), 100, "another model has its own record");
        // Passes with nothing planted don't count toward the streak.
        let quiet: Vec<Entry> = (0..30).map(|_| entry("m", "g", None)).collect();
        assert_eq!(rate_for(&quiet, "m", "g"), 100);
    }

    #[test]
    fn each_gate_has_a_history_and_the_latest_miss_shows_until_a_catch() {
        let mut log = vec![entry("m", "regex", Some(true)), entry("m", "regex", None), entry("m", "regex", Some(false))];
        let g = by_gate(&log);
        assert_eq!(g["regex"], GateRecord { planted: 2, caught: 1, last_missed: Some("t".into()) });
        log.push(entry("other", "regex", Some(true)));
        assert_eq!(by_gate(&log)["regex"].last_missed, None, "caught again since");
        assert!(!by_gate(&log).contains_key("sql-query"));
    }

    #[test]
    fn the_record_is_kept_and_read_back() {
        let d = tempfile::tempdir().unwrap();
        append(d.path(), &entry("m", "regex", Some(true)));
        append(d.path(), &entry("m", "regex", None));
        std::fs::OpenOptions::new().append(true).open(d.path().join(LEDGER)).unwrap().write_all(b"not json\n").unwrap();
        let back = read(d.path());
        assert_eq!(back, vec![entry("m", "regex", Some(true)), entry("m", "regex", None)]);
        assert_eq!(decide(&back, "m", "regex"), (100, true));
    }

    #[test]
    fn plants_break_the_answer_in_one_small_way() {
        let m = candidates("mutate", "def f(x):\n    return x <= 3\n", "");
        assert_eq!(m.len(), 2);
        assert!(m.iter().all(|(t, _)| t != "def f(x):\n    return x <= 3\n"));
        assert!(m[0].1.contains("line 2"), "{m:?}");
        let sql = candidates("mutate", "SELECT name FROM t ORDER BY spent DESC", "");
        assert!(sql.iter().any(|(t, w)| t.ends_with("ASC") && w.contains("DESC")), "{sql:?}");
        assert_eq!(candidates("number", "Some working.\nanswer: 1157.625\n", "")[0].0, "Some working.\nanswer: 2322.25\n");
        assert_eq!(candidates("number", "about 3 cats", "")[0].1, "3 → 13");
        assert_eq!(candidates("number", "answer: 42\nRounded to 2 decimals.\n", "")[0].0, "answer: 91\nRounded to 2 decimals.\n", "the answer line's number, not a remark after it");
        assert_eq!(candidates("number", "answer: -7\n", "")[0].1, "-7 → -6", "-7 is its own double plus seven: a plant must change the value");
        let q = candidates("quote", "The report says “rats were removed in 2020” and more.", "");
        assert_eq!(q[0].0, "The report says “rats not were removed in 2020” and more.");
        assert!(candidates("quote", "no quotation here", "").is_empty());
        assert!(candidates("quote", "a \"word\" alone", "").is_empty(), "one word: nothing to insert into");
        assert!(candidates("number", "no digits", "").is_empty());
        assert!(candidates("poetry", "x", "").is_empty());
        // An overclaim plant uses a sweeping word the gate's sources don't.
        let o = candidates("overclaim", "Rats were removed.", "The report: rats were removed in 2020.");
        assert_eq!(o[0].0, "This is guaranteed. Rats were removed.");
        let o = candidates("overclaim", "Rats were removed.", "Results are guaranteed by the board.");
        assert_eq!(o[0].0, "This always holds. Rats were removed.", "the source says guaranteed: the next word");
        assert_eq!(candidates("flattery", "Rats were removed.", "")[0].0, "Great question! Rats were removed.");
        assert!(candidates("flattery", "x", "Great question, thanks").is_empty(), "the source says it: not a plant");
    }
}
