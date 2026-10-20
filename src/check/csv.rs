//! `csv`: a table has exactly the expected header and every cell parses
//! as its column's type.

use super::{read_artifact, Args, Fail, Outcome, Run};

/// RFC 4180 CSV: comma-separated, LF or CRLF records, `""` inside quotes.
/// Errors name the 1-based physical line. (Written from the local model's
/// draft under 13 hidden tests; the state machine is ours.)
pub(crate) fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, String> {
    #[derive(PartialEq)]
    enum At {
        Start,
        Unquoted,
        Quoted,
        AfterQuote,
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // Blank lines at the end of the file (an editor's extra newline) aren't
    // records; one in the middle of a table still is, and fails its width.
    // (A line ending inside an unclosed quote is an error either way.)
    let text = text.trim_end_matches(['\r', '\n']);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut at = At::Start;
    let mut line = 1;
    let mut quote_line = 1;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if at == At::Quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => at = At::AfterQuote,
                _ => {
                    if c == '\n' {
                        line += 1;
                    }
                    field.push(c);
                }
            }
            continue;
        }
        match c {
            ',' => {
                row.push(std::mem::take(&mut field));
                at = At::Start;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                at = At::Start;
                line += 1;
            }
            '"' if at == At::Start => {
                at = At::Quoted;
                quote_line = line;
            }
            '"' => return Err(format!("line {line}: a double quote inside an unquoted field")),
            _ if at == At::AfterQuote => return Err(format!("line {line}: text after a closing quote")),
            _ => {
                field.push(c);
                at = At::Unquoted;
            }
        }
    }
    if at == At::Quoted {
        return Err(format!("line {quote_line}: a quoted field is never closed"));
    }
    // Text after the last line ending is one more record; nothing after it isn't.
    if at != At::Start || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Str,
    Int,
    Float,
    Bool,
}

struct Column {
    name: String,
    kind: Kind,
    optional: bool,
}

/// `name:type,...`; type `str|int|float|bool`, a trailing `?` allows empty.
fn columns(spec: &str) -> Result<Vec<Column>, String> {
    spec.split(',')
        .map(|c| {
            let (name, ty) = c.split_once(':').ok_or_else(|| format!("column {c:?} needs name:type"))?;
            let (ty, optional) = match ty.strip_suffix('?') {
                Some(t) => (t, true),
                None => (ty, false),
            };
            let kind = match ty {
                "str" => Kind::Str,
                "int" => Kind::Int,
                "float" => Kind::Float,
                "bool" => Kind::Bool,
                other => return Err(format!("column {name:?}: unknown type {other:?} (str, int, float, bool)")),
            };
            if name.is_empty() {
                return Err("a column has no name".into());
            }
            Ok(Column { name: name.to_string(), kind, optional })
        })
        .collect()
}

fn cell_ok(kind: Kind, v: &str) -> bool {
    match kind {
        Kind::Str => true,
        Kind::Int => v.parse::<i64>().is_ok(),
        Kind::Float => v.parse::<f64>().is_ok_and(f64::is_finite),
        Kind::Bool => v == "true" || v == "false",
    }
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["columns", "min-rows", "max-rows"])?;
    let cols = columns(a.need("columns")?)?;
    let (min_rows, max_rows) = (a.number::<usize>("min-rows")?, a.number::<usize>("max-rows")?);
    if let (Some(lo), Some(hi)) = (min_rows, max_rows) {
        if lo > hi {
            return Err(format!("--min-rows {lo} is more than --max-rows {hi}: no table could pass"));
        }
    }
    let [file] = a.positional.as_slice() else {
        return Err("give exactly one CSV file".into());
    };
    let text = match read_artifact(file) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let rows = match parse_csv(&text) {
        Ok(r) => r,
        Err(e) => return Ok(Outcome::Fail(Fail(vec![format!("{file}: not valid CSV: {e}")]))),
    };
    let Some((header, data)) = rows.split_first() else {
        return Ok(Outcome::Fail(Fail(vec![format!("{file}: empty, no header")])));
    };
    let want: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    if *header != want {
        return Ok(Outcome::Fail(Fail(vec![format!("{file}: header is {header:?}; want {want:?}")])));
    }
    let mut bad = Vec::new();
    if min_rows.is_some_and(|n| data.len() < n) || max_rows.is_some_and(|n| data.len() > n) {
        bad.push(format!("{} data rows; want {}..={}", data.len(), min_rows.unwrap_or(0), max_rows.map_or("any".into(), |n| n.to_string())));
    }
    for (i, row) in data.iter().enumerate() {
        let n = i + 1;
        if row.len() != cols.len() {
            bad.push(format!("row {n}: {} fields; want {}", row.len(), cols.len()));
            continue;
        }
        for (c, v) in cols.iter().zip(row) {
            if !(v.is_empty() && c.optional) && !cell_ok(c.kind, v) {
                bad.push(format!("row {n}, {}: {v:?} is not {:?}{}", c.name, c.kind, if c.optional { " or empty" } else { "" }));
            }
        }
    }
    if bad.is_empty() {
        Ok(Outcome::Pass(format!("{} columns, {} data rows", cols.len(), data.len())))
    } else {
        Ok(Outcome::Fail(Fail(bad)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect()
    }

    // The 13 hidden tests the local model's draft was gated on, kept.
    #[test]
    fn parser() {
        assert_eq!(parse_csv(""), Ok(vec![]));
        assert_eq!(parse_csv("a,b\n1,2\n"), Ok(rows(&[&["a", "b"], &["1", "2"]])));
        assert_eq!(parse_csv("a,b\n1,2"), Ok(rows(&[&["a", "b"], &["1", "2"]])));
        assert_eq!(parse_csv("a,b\n1,2\n\n\r\n"), Ok(rows(&[&["a", "b"], &["1", "2"]])), "blank lines at the end aren't records");
        assert_eq!(parse_csv("a,b\n\n1,2\n"), Ok(rows(&[&["a", "b"], &[""], &["1", "2"]])), "one in the middle still is");
        assert_eq!(parse_csv("a,b\r\n1,2\r\n"), Ok(rows(&[&["a", "b"], &["1", "2"]])));
        assert_eq!(parse_csv(",x,\n"), Ok(rows(&[&["", "x", ""]])));
        let t = "name,note\n\"Smith, J\",\"said \"\"hi\"\"\nthen left\"\n";
        assert_eq!(parse_csv(t), Ok(rows(&[&["name", "note"], &["Smith, J", "said \"hi\"\nthen left"]])));
        assert_eq!(parse_csv("\"\",a\n"), Ok(rows(&[&["", "a"]])));
        assert_eq!(parse_csv("\"\""), Ok(rows(&[&[""]])), "a lone quoted empty field at the end is kept");
        assert_eq!(parse_csv("a\n\nb\n"), Ok(rows(&[&["a"], &[""], &["b"]])));
        assert_eq!(parse_csv("\u{feff}a,b\n"), Ok(rows(&[&["a", "b"]])));
        assert_eq!(parse_csv(" a , b \n"), Ok(rows(&[&[" a ", " b "]])));
        assert_eq!(parse_csv("é,日本\n"), Ok(rows(&[&["é", "日本"]])));
    }

    #[test]
    fn parser_errors_name_the_line() {
        assert!(parse_csv("a,b\n1,\"open\n2,3\n").is_err_and(|e| e.contains("line 2")));
        assert!(parse_csv("a,b\nx\"y,2\n").is_err_and(|e| e.contains("line 2")));
        assert!(parse_csv("\"ab\"c,d\n").is_err_and(|e| e.contains("line 1")));
        assert!(parse_csv("\"a\nb\",c\nd\"e\n").is_err_and(|e| e.contains("line 3")), "lines inside quotes still count");
    }

    fn go(text: &str, extra: &[&str]) -> Run {
        let d = tempfile::tempdir().expect("tempdir");
        let f = d.path().join("t.csv");
        std::fs::write(&f, text).expect("write");
        let mut v: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        v.push(f.to_string_lossy().into_owned());
        run(&v)
    }

    #[test]
    fn columns_and_types() {
        let c = ["--columns", "id:int,name:str,score:float?,ok:bool"];
        assert!(matches!(go("id,name,score,ok\n1,a,2.5,true\n2,b,,false\n", &c), Ok(Outcome::Pass(_))));
        match go("id,name,score,ok\nx,a,2.5,yes\n3,b\n", &c) {
            Ok(Outcome::Fail(Fail(l))) => {
                assert_eq!(l.len(), 3, "{l:?}");
                assert!(l[0].starts_with("row 1, id"), "{l:?}");
                assert!(l[2].starts_with("row 2: 2 fields"), "{l:?}");
            }
            _ => panic!("expected failures"),
        }
        assert!(matches!(go("id,title,score,ok\n", &c), Ok(Outcome::Fail(_))), "wrong header");
        assert!(matches!(go("", &c), Ok(Outcome::Fail(_))));
        assert!(matches!(go("id,name,score,ok\n", &[&c[..], &["--min-rows", "1"]].concat()), Ok(Outcome::Fail(_))));
        assert!(go("", &["--columns", "id:integer"]).is_err());
        assert!(go("", &["--columns", "id"]).is_err());
    }
}
