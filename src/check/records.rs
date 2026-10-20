//! `records`: two data files hold the same records — a conversion between
//! CSV, TSV, JSON (an array of objects) and JSON Lines lost, added or
//! changed nothing. Each record is a set of named fields; values match when
//! their text is the same, when both are numbers of equal value (`3.50` and
//! `3.5`), when both say true or false, or when both are empty (an empty
//! cell, `null`, or a field left out). A nested JSON value matches the same
//! value written as JSON text in a cell. Field names must match exactly.

use super::{read_artifact, Args, Fail, Outcome, Run};
use serde_json::Value;
use std::collections::BTreeMap;

/// Differences listed after the first.
const SHOWN: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Format {
    Csv,
    Tsv,
    Json,
    Jsonl,
}

impl Format {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "csv" => Ok(Self::Csv),
            "tsv" => Ok(Self::Tsv),
            "json" => Ok(Self::Json),
            "jsonl" | "ndjson" => Ok(Self::Jsonl),
            other => Err(format!("format {other:?}: one of csv, tsv, json, jsonl")),
        }
    }

    /// From the file's extension, when no format is given.
    fn of(path: &str) -> Result<Self, String> {
        let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        Self::parse(&ext).map_err(|_| format!("{path}: say its format (csv, tsv, json, jsonl); the extension doesn't"))
    }
}

/// One record: field name → value. JSON keeps its types; a table's cells
/// are strings.
type Record = BTreeMap<String, Value>;

/// The records in `text`, or why it isn't that format (the answer's fault).
fn records(text: &str, format: Format) -> Result<Vec<Record>, String> {
    match format {
        Format::Csv | Format::Tsv => {
            let rows = if format == Format::Csv {
                super::csv::parse_csv(text)?
            } else {
                let text = text.strip_prefix('\u{feff}').unwrap_or(text);
                text.lines().filter(|l| !l.is_empty()).map(|l| l.trim_end_matches('\r').split('\t').map(str::to_string).collect()).collect()
            };
            let mut rows = rows.into_iter();
            let header = rows.next().ok_or("no header row")?;
            if let Some(dup) = header.iter().enumerate().find(|(i, h)| header[..*i].contains(h)).map(|(_, h)| h) {
                return Err(format!("the header names {dup:?} twice"));
            }
            rows.enumerate()
                .map(|(i, row)| {
                    if row.len() != header.len() {
                        return Err(format!("row {} has {} fields; the header has {}", i + 1, row.len(), header.len()));
                    }
                    Ok(header.iter().cloned().zip(row.into_iter().map(Value::String)).collect())
                })
                .collect()
        }
        Format::Json => {
            let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
            let Value::Array(items) = v else { return Err("not a JSON array of objects".into()) };
            items.into_iter().enumerate().map(|(i, item)| object(item).ok_or_else(|| format!("item {} isn't an object", i + 1))).collect()
        }
        Format::Jsonl => text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| {
                let v: Value = serde_json::from_str(l).map_err(|e| format!("line {}: not JSON: {e}", i + 1))?;
                object(v).ok_or_else(|| format!("line {} isn't a JSON object", i + 1))
            })
            .collect(),
    }
}

fn object(v: Value) -> Option<Record> {
    match v {
        Value::Object(m) => Some(m.into_iter().collect()),
        _ => None,
    }
}

/// No value: an empty cell, `null`, an empty string, or a missing field.
fn empty(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    }
}

/// The value as text, the way a table cell would hold it.
fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The same value, written either way (see the module docs).
fn same(a: Option<&Value>, b: Option<&Value>) -> bool {
    if empty(a) || empty(b) {
        return empty(a) && empty(b);
    }
    let (Some(a), Some(b)) = (a, b) else { return false };
    if a == b {
        return true;
    }
    let (ta, tb) = (text_of(a), text_of(b));
    if ta == tb {
        return true;
    }
    // `02134` is a code, not a number: a leading zero keeps it text, so a
    // ZIP code turned into 2134 is a loss.
    let number = |t: &str| {
        let t = t.trim();
        let digits = t.trim_start_matches(['-', '+']);
        let coded = digits.len() > 1 && digits.starts_with('0') && digits.as_bytes().get(1).is_some_and(u8::is_ascii_digit);
        t.parse::<f64>().ok().filter(|f| f.is_finite() && !coded)
    };
    // Whole numbers compare exactly: an f64 can't tell 9007199254740993
    // from 9007199254740992, and an ID off by one is another record.
    let whole = |t: &str| {
        let t = t.trim();
        let t = t.strip_suffix(".0").unwrap_or(t);
        let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
        let coded = digits.len() > 1 && digits.starts_with('0');
        (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && !coded).then(|| t.parse::<i128>().ok()).flatten()
    };
    if let (Some(x), Some(y)) = (whole(&ta), whole(&tb)) {
        return x == y;
    }
    if let (Some(x), Some(y)) = (number(&ta), number(&tb)) {
        return x == y;
    }
    let boolean = |t: &str| match t.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };
    if let (Some(x), Some(y)) = (boolean(&ta), boolean(&tb)) {
        return x == y;
    }
    // A nested value against the same value as JSON text in a cell.
    let parsed = |v: &Value| match v {
        Value::String(s) => serde_json::from_str::<Value>(s).ok(),
        other => Some(other.clone()),
    };
    matches!((parsed(a), parsed(b)), (Some(x), Some(y)) if (x.is_object() || x.is_array()) && x == y)
}

/// Why record `b` isn't record `a`, or None when they match.
fn differs(a: &Record, b: &Record) -> Option<String> {
    let extra: Vec<&String> = b.keys().filter(|k| !a.contains_key(*k)).collect();
    if let Some(k) = extra.first() {
        return Some(format!("field {k:?} isn't in the source"));
    }
    a.iter().find(|(k, v)| !same(Some(v), b.get(*k))).map(|(k, v)| {
        let shown = |v: Option<&Value>| match v {
            None => "missing".to_string(),
            Some(v) => format!("{:?}", text_of(v)),
        };
        format!("field {k:?}: {} in the source, {} in the result", shown(Some(v)), shown(b.get(k)))
    })
}

/// A record as one short line, for reports.
fn brief(r: &Record) -> String {
    let s = serde_json::to_string(r).unwrap_or_default();
    if s.chars().count() > 120 {
        format!("{}…", s.chars().take(120).collect::<String>())
    } else {
        s
    }
}

/// Every difference: in order, record by record; in any order, the
/// source's records with no match and the result's left over.
pub(crate) fn compare(want: &[Record], got: &[Record], keep_order: bool) -> Vec<String> {
    let mut out = Vec::new();
    if keep_order {
        if want.len() == got.len() && want.iter().zip(got).any(|(a, b)| differs(a, b).is_some()) && compare(want, got, false).is_empty() {
            let first = want.iter().zip(got).position(|(a, b)| differs(a, b).is_some()).unwrap_or(0);
            let from = want.iter().position(|a| differs(a, &got[first]).is_none()).map_or(0, |i| i + 1);
            return vec![format!("every record is there, but not in the source's order: record {} of the result is record {from} of the source", first + 1)];
        }
        for (i, (a, b)) in want.iter().zip(got).enumerate() {
            if let Some(why) = differs(a, b) {
                out.push(format!("record {}: {why}", i + 1));
            }
        }
        if want.len() != got.len() {
            let (n, which, rest) = if got.len() < want.len() { (got.len(), "missing", &want[got.len()..]) } else { (want.len(), "extra", &got[want.len()..]) };
            out.insert(0, format!("the source has {} records, the result {}; first {which} (record {}): {}", want.len(), got.len(), n + 1, rest.first().map(brief).unwrap_or_default()));
        }
        return out;
    }
    let mut left: Vec<Option<&Record>> = got.iter().map(Some).collect();
    for (i, a) in want.iter().enumerate() {
        match left.iter_mut().find(|b| b.is_some_and(|b| differs(a, b).is_none())) {
            Some(slot) => *slot = None,
            None => out.push(format!("source record {} has no match in the result: {}", i + 1, brief(a))),
        }
    }
    out.extend(left.iter().enumerate().filter_map(|(i, b)| b.map(|b| format!("result record {} isn't in the source: {}", i + 1, brief(b)))));
    out
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["source", "source-format", "file", "format", "order"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let (source, file) = (a.need("source")?, a.need("file")?);
    let source_format = a.one("source-format")?.map_or_else(|| Format::of(source), Format::parse)?;
    let format = a.one("format")?.map_or_else(|| Format::of(file), Format::parse)?;
    let keep_order = match a.one("order")?.unwrap_or("keep") {
        "keep" => true,
        "any" => false,
        other => return Err(format!("--order {other:?}: keep or any")),
    };
    let src = std::fs::read_to_string(source).map_err(|e| format!("--source {source}: {e}"))?;
    let want = records(&src, source_format).map_err(|e| format!("--source {source}: {e}"))?;
    if want.is_empty() {
        return Err(format!("--source {source}: no records, so an empty answer would pass"));
    }
    let text = match read_artifact(file) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let got = match records(&text, format) {
        Ok(r) => r,
        Err(why) => return Ok(Outcome::Fail(Fail(vec![format!("{file} isn't {}: {why}", name(format))]))),
    };
    let diffs = compare(&want, &got, keep_order);
    if diffs.is_empty() {
        return Ok(Outcome::Pass(format!("{} records, every field the same ({} → {})", want.len(), name(source_format), name(format))));
    }
    let n = diffs.len();
    let mut lines: Vec<String> = diffs.into_iter().take(SHOWN + 1).collect();
    if n > 1 {
        lines[0] = format!("{} ({n} differences)", lines[0]);
    }
    Ok(Outcome::Fail(Fail(lines)))
}

fn name(f: Format) -> &'static str {
    match f {
        Format::Csv => "CSV",
        Format::Tsv => "TSV",
        Format::Json => "a JSON array of objects",
        Format::Jsonl => "JSON Lines",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(text: &str, f: Format) -> Vec<Record> {
        records(text, f).expect("parses")
    }

    const CSV: &str = "id,name,price,in_stock,tags\n1,\"Lamp, brass\",3.50,true,\n2,Desk,120,false,\"[\"\"oak\"\"]\"\n";

    #[test]
    fn the_same_table_in_every_format_matches() {
        let want = r(CSV, Format::Csv);
        let json = r#"[{"id": 1, "name": "Lamp, brass", "price": 3.5, "in_stock": true, "tags": null},
                       {"id": 2, "name": "Desk", "price": 120, "in_stock": false, "tags": ["oak"]}]"#;
        assert!(compare(&want, &r(json, Format::Json), true).is_empty(), "{:?}", compare(&want, &r(json, Format::Json), true));
        let jsonl = "{\"id\":\"1\",\"name\":\"Lamp, brass\",\"price\":\"3.50\",\"in_stock\":\"true\"}\n{\"id\":2,\"name\":\"Desk\",\"price\":120.0,\"in_stock\":false,\"tags\":\"[\\\"oak\\\"]\"}\n";
        assert!(compare(&want, &r(jsonl, Format::Jsonl), true).is_empty(), "a left-out field is an empty one");
        let tsv = "id\tname\tprice\tin_stock\ttags\n1\tLamp, brass\t3.5\tTRUE\t\n2\tDesk\t120\tfalse\t[\"oak\"]\n";
        assert!(compare(&want, &r(tsv, Format::Tsv), true).is_empty());
    }

    #[test]
    fn changed_lost_extra_and_renamed_fields_are_named() {
        let want = r(CSV, Format::Csv);
        let changed = r("id,name,price,in_stock,tags\n1,\"Lamp, brass\",3.05,true,\n2,Desk,120,false,\"[\"\"oak\"\"]\"\n", Format::Csv);
        assert_eq!(compare(&want, &changed, true), ["record 1: field \"price\": \"3.50\" in the source, \"3.05\" in the result"]);
        let lost = r(r#"[{"id":1,"name":"Lamp, brass","price":3.5,"in_stock":true}]"#, Format::Json);
        assert!(compare(&want, &lost, true)[0].starts_with("the source has 2 records, the result 1; first missing (record 2)"), "{:?}", compare(&want, &lost, true));
        let renamed = r(r#"[{"id":1,"title":"Lamp, brass","price":3.5,"in_stock":true},{"id":2,"title":"Desk","price":120,"in_stock":false,"tags":["oak"]}]"#, Format::Json);
        assert_eq!(compare(&want, &renamed, true)[0], "record 1: field \"title\" isn't in the source");
        let emptied = r(r#"[{"id":1,"name":"","price":3.5,"in_stock":true},{"id":2,"name":"Desk","price":120,"in_stock":false,"tags":["oak"]}]"#, Format::Json);
        assert_eq!(compare(&want, &emptied, true), ["record 1: field \"name\": \"Lamp, brass\" in the source, \"\" in the result"]);
        let truthy = r(r#"[{"id":1,"name":"Lamp, brass","price":3.5,"in_stock":1},{"id":2,"name":"Desk","price":120,"in_stock":false,"tags":["oak"]}]"#, Format::Json);
        assert_eq!(compare(&want, &truthy, true).len(), 1, "1 isn't true");
        let zip = r("zip\n02134\n", Format::Csv);
        assert_eq!(compare(&zip, &r(r#"[{"zip": 2134}]"#, Format::Json), true).len(), 1, "a leading zero lost");
        assert!(compare(&zip, &r(r#"[{"zip": "02134"}]"#, Format::Json), true).is_empty());
        let big = r("id\n12345678901234567891\n9007199254740993\n", Format::Csv);
        assert_eq!(compare(&big, &r(r#"[{"id": "12345678901234567890"}, {"id": 9007199254740992}]"#, Format::Json), true).len(), 2, "big whole numbers compare exactly");
        assert!(compare(&big, &r(r#"[{"id": "12345678901234567891"}, {"id": 9007199254740993}]"#, Format::Json), true).is_empty());
        assert!(compare(&r("n\n120\n", Format::Csv), &r(r#"[{"n": 120.0}]"#, Format::Json), true).is_empty(), "120 and 120.0 are the same");
        assert!(compare(&r("x\n0.5\n", Format::Csv), &r(r#"[{"x": 0.50}]"#, Format::Json), true).is_empty(), "0.5 is a number");
    }

    #[test]
    fn order_matters_unless_any() {
        let want = r(CSV, Format::Csv);
        let swapped = r(r#"[{"id":2,"name":"Desk","price":120,"in_stock":false,"tags":["oak"]},{"id":1,"name":"Lamp, brass","price":3.5,"in_stock":true}]"#, Format::Json);
        assert_eq!(compare(&want, &swapped, true), ["every record is there, but not in the source's order: record 1 of the result is record 2 of the source"]);
        assert!(compare(&want, &swapped, false).is_empty());
        let dup = r(r#"[{"id":2,"name":"Desk","price":120,"in_stock":false,"tags":["oak"]},{"id":2,"name":"Desk","price":120,"in_stock":false,"tags":["oak"]}]"#, Format::Json);
        let d = compare(&want, &dup, false);
        assert!(d[0].starts_with("source record 1 has no match") && d[1].starts_with("result record 2 isn't in the source"), "{d:?}");
    }

    #[test]
    fn malformed_data_is_the_answers_fault() {
        assert!(records("[{\"a\":1}, 2]", Format::Json).unwrap_err().contains("item 2"));
        assert!(records("{\"a\":1}", Format::Json).unwrap_err().contains("array"));
        assert!(records("a,b\n1\n", Format::Csv).unwrap_err().contains("row 1 has 1 fields"));
        assert!(records("a,a\n1,2\n", Format::Csv).unwrap_err().contains("twice"));
        assert!(records("{\"a\":1}\nnope\n", Format::Jsonl).unwrap_err().contains("line 2"));
    }

    #[test]
    fn cli_exit_codes() {
        let d = tempfile::tempdir().expect("tmp");
        let p = |n: &str, t: &str| {
            let f = d.path().join(n);
            std::fs::write(&f, t).expect("write");
            f.to_string_lossy().into_owned()
        };
        let (src, good, bad, empty_src) = (p("s.csv", CSV), p("o.data", r#"[{"id":1,"name":"Lamp, brass","price":3.5,"in_stock":true},{"id":2,"name":"Desk","price":120,"in_stock":false,"tags":["oak"]}]"#), p("b.json", "[]"), p("e.csv", "id,name\n"));
        let run_ = |args: &[&str]| run(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(matches!(run_(&["--source", &src, "--file", &good, "--format", "json"]), Ok(Outcome::Pass(_))));
        assert!(matches!(run_(&["--source", &src, "--file", &bad]), Ok(Outcome::Fail(_))));
        assert!(run_(&["--source", &src, "--file", &good]).is_err(), ".data says no format");
        assert!(matches!(run_(&["--source", &empty_src, "--file", &bad]), Err(e) if e.contains("no records")));
        assert!(run_(&["--source", &src, "--file", &bad, "--order", "sorted"]).is_err());
    }
}
