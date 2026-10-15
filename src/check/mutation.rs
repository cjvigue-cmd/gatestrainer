//! `mutation`: tests that can fail. The code under test is changed one
//! small way at a time (a comparison flipped, a constant off by one …);
//! good tests fail on most of those mutants. Tests that pass on all of them
//! prove nothing, whoever wrote them.
//!
//! Mutants come from the source text alone (comments and strings skipped),
//! so the check works for any language the tests run in. Some mutants are
//! equivalent to the original and can't be caught: hence a threshold, not
//! all of them.

use super::{Args, Fail, Outcome, Run};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

/// One small change to the source.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Mutant {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub to: String,
    pub what: String,
}

impl Mutant {
    pub fn apply(&self, src: &str) -> String {
        format!("{}{}{}", &src[..self.start], self.to, &src[self.end..])
    }
}

/// Operators swapped wherever they appear in code (longest first).
const SWAPS: [(&str, &str); 8] = [("===", "!=="), ("!==", "==="), ("<=", "<"), (">=", ">"), ("==", "!="), ("!=", "=="), ("&&", "||"), ("||", "&&")];
/// Operators swapped only between spaces (`a < b`), so generics, arrows
/// and unary signs are left alone.
const SPACED: [(&str, &str); 4] = [("<", "<="), (">", ">="), ("+", "-"), ("-", "+")];
/// Whole words swapped.
const WORDS: [(&str, &str); 6] = [("and", "or"), ("or", "and"), ("True", "False"), ("False", "True"), ("true", "false"), ("false", "true")];

fn ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Every mutant of `src`, in source order.
pub(crate) fn mutants(src: &str) -> Vec<Mutant> {
    #[derive(PartialEq)]
    enum At {
        Code,
        Block,
        Str(char, bool),
    }
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let ch = |i: usize| chars.get(i).map(|&(_, c)| c);
    let off = |i: usize| chars.get(i).map_or(src.len(), |&(o, _)| o);
    let starts = |i: usize, s: &str| s.chars().enumerate().all(|(k, c)| ch(i + k) == Some(c));
    let mut out = Vec::new();
    let mut at = At::Code;
    let mut line = 1;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i].1;
        if c == '\n' {
            line += 1;
            // Only triple-quoted strings and block comments span lines.
            if matches!(at, At::Str(_, false)) {
                at = At::Code;
            }
            i += 1;
            continue;
        }
        match at {
            At::Block => {
                if starts(i, "*/") {
                    at = At::Code;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            At::Str(q, triple) => {
                if c == '\\' {
                    i += 2;
                } else if triple && starts(i, &q.to_string().repeat(3)) {
                    at = At::Code;
                    i += 3;
                } else {
                    if !triple && c == q {
                        at = At::Code;
                    }
                    i += 1;
                }
                continue;
            }
            At::Code => {}
        }
        // Comments to the end of the line: `#` and `//` (losing Python's
        // floor division as a site is harmless).
        if c == '#' || starts(i, "//") {
            while i < chars.len() && chars[i].1 != '\n' {
                i += 1;
            }
            continue;
        }
        if starts(i, "/*") {
            at = At::Block;
            i += 2;
            continue;
        }
        if c == '"' || c == '\'' || c == '`' {
            let triple = c != '`' && starts(i, &c.to_string().repeat(3));
            at = At::Str(c, triple);
            i += if triple { 3 } else { 1 };
            continue;
        }
        let prev = if i == 0 { None } else { ch(i - 1) };
        let mut push = |len: usize, to: &str, what: String| {
            out.push(Mutant { line, start: off(i), end: off(i + len), to: to.to_string(), what });
        };
        if let Some((from, to)) = SWAPS.iter().find(|(f, _)| starts(i, f)) {
            let n = from.chars().count();
            // Not part of a longer operator (`<<=`, `>>=`, `<=>`, `===`).
            let longer = matches!(prev, Some('<' | '>' | '=' | '!')) || matches!(ch(i + n), Some('=' | '>'));
            if !longer {
                push(n, to, format!("`{from}` → `{to}`"));
            }
            i += n;
            continue;
        }
        if let Some((from, to)) = SPACED.iter().find(|(f, _)| starts(i, f)) {
            if prev == Some(' ') && ch(i + 1) == Some(' ') {
                push(1, to, format!("`{from}` → `{to}`"));
            }
            i += 1;
            continue;
        }
        if c == '!' && ch(i + 1) != Some('=') && !prev.is_some_and(ident) && ch(i + 1).is_some_and(|n| ident(n) || n == '(') {
            push(1, "", "`!` removed".into());
            i += 1;
            continue;
        }
        if ident(c) && !prev.is_some_and(|p| ident(p) || p == '.') {
            let mut j = i;
            while ch(j).is_some_and(ident) {
                j += 1;
            }
            let word: String = chars[i..j].iter().map(|&(_, c)| c).collect();
            if let Some((_, to)) = WORDS.iter().find(|(f, _)| *f == word) {
                push(j - i, to, format!("`{word}` → `{to}`"));
            } else if word == "not" && ch(j) == Some(' ') {
                push(j - i + 1, "", "`not` removed".into());
            } else if word.chars().all(|d| d.is_ascii_digit()) && word.len() <= 18 && !(ch(j) == Some('.') && ch(j + 1).is_some_and(|d| d.is_ascii_digit())) {
                if let Ok(n) = word.parse::<u64>() {
                    let to = (n + 1).to_string();
                    push(j - i, &to, format!("`{word}` → `{to}`"));
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// `max` mutants spread evenly over `all` (deterministic).
pub(crate) fn spread(all: Vec<Mutant>, max: usize) -> Vec<Mutant> {
    if all.len() <= max {
        return all;
    }
    let n = all.len();
    (0..max).filter_map(|k| all.get(k * n / max).cloned()).collect()
}

/// Write `text` with its own modification time. Build tools and caches
/// decide from the time (and size) whether a file changed: Python reuses
/// bytecode for a same-size edit within the same second, and cargo skips a
/// rebuild for a source older than its last build. Every version written
/// here gets a later time than anything before it, so each one is rebuilt.
fn write_at(path: &Path, text: &str, at: SystemTime) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    let f = std::fs::OpenOptions::new().write(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.set_modified(at).map_err(|e| format!("{}: set time: {e}", path.display()))
}

/// Puts the original file back however the check ends, newer than every mutant.
struct Restore<'a> {
    path: &'a Path,
    text: &'a str,
    at: SystemTime,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        if let Err(e) = write_at(self.path, self.text, self.at) {
            eprintln!("mutation: couldn't restore {}: {e}", self.path.display());
        }
    }
}

/// Exit status of `cmd`; output kept only when asked for.
fn status(cmd: &[String], keep: bool) -> Result<(bool, String), String> {
    let out = Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .stdout(if keep { Stdio::piped() } else { Stdio::null() })
        .stderr(if keep { Stdio::piped() } else { Stdio::null() })
        .output()
        .map_err(|e| format!("{}: {e}", cmd[0]))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
    Ok((out.status.success(), text))
}

fn split(name: &str, cmd: &str) -> Result<Vec<String>, String> {
    shlex::split(cmd).filter(|a| !a.is_empty()).ok_or_else(|| format!("--{name}: can't split {cmd:?} into a command"))
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["file", "run", "build", "min-killed", "max-mutants"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let file = Path::new(a.need("file")?);
    let tests = split("run", a.need("run")?)?;
    let build = a.one("build")?.map(|b| split("build", b)).transpose()?;
    let min: f64 = a.number("min-killed")?.unwrap_or(0.8);
    let max: usize = a.number("max-mutants")?.unwrap_or(40);
    if !(0.0..=1.0).contains(&min) || max == 0 {
        return Err("--min-killed is 0..=1 and --max-mutants at least 1".into());
    }
    let original = std::fs::read_to_string(file).map_err(|e| format!("--file {}: {e}", file.display()))?;
    let all = mutants(&original);
    if all.is_empty() {
        return Err(format!("{}: nothing to mutate", file.display()));
    }
    let found = all.len();
    let chosen = spread(all, max);
    let start = SystemTime::now();
    let tick = |k: usize| start + Duration::from_secs(k as u64 + 1);
    let _restore = Restore { path: file, text: &original, at: tick(chosen.len() + 1) };
    // The tests must pass on the real code first: tests that always fail
    // would "catch" every mutant.
    let (ok, text) = status(&tests, true)?;
    if !ok {
        let tail: Vec<&str> = text.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect();
        let mut lines = vec!["the tests fail on the code itself, before any mutation".to_string()];
        lines.extend(tail.into_iter().map(str::to_string));
        return Ok(Outcome::Fail(Fail(lines)));
    }
    let (mut killed, mut invalid, mut survived) = (0usize, 0usize, Vec::new());
    for (k, m) in chosen.iter().enumerate() {
        write_at(file, &m.apply(&original), tick(k))?;
        if let Some(b) = &build {
            if !status(b, false)?.0 {
                invalid += 1;
                continue;
            }
        }
        if status(&tests, false)?.0 {
            survived.push(format!("line {}: {} — the tests still pass", m.line, m.what));
        } else {
            killed += 1;
        }
    }
    let valid = chosen.len() - invalid;
    let score = if valid == 0 { 0.0 } else { killed as f64 / valid as f64 };
    let summary = format!(
        "caught {killed} of {valid} mutants ({:.0}%; need {:.0}%){}{}",
        score * 100.0,
        min * 100.0,
        if invalid > 0 { format!("; {invalid} didn't build") } else { String::new() },
        if found > chosen.len() { format!("; {} of {found} sites tried", chosen.len()) } else { String::new() },
    );
    if valid > 0 && score >= min {
        return Ok(Outcome::Pass(summary));
    }
    let mut lines = vec![summary];
    lines.extend(survived);
    lines.push("(a mutant that no input can tell apart from the original can't be caught; add tests for the others)".into());
    Ok(Outcome::Fail(Fail(lines)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whats(src: &str) -> Vec<String> {
        mutants(src).into_iter().map(|m| m.what).collect()
    }

    #[test]
    fn operators_words_and_numbers() {
        assert_eq!(whats("if a <= b and not done: return 0"), ["`<=` → `<`", "`and` → `or`", "`not` removed", "`0` → `1`"]);
        assert_eq!(whats("x = a < b || c == d"), ["`<` → `<=`", "`||` → `&&`", "`==` → `!=`"]);
        assert_eq!(whats("if (x === 1 && !ok) {}"), ["`===` → `!==`", "`1` → `2`", "`&&` → `||`", "`!` removed"]);
        assert_eq!(whats("return a + b - 1"), ["`+` → `-`", "`-` → `+`", "`1` → `2`"]);
        assert_eq!(whats("flag = True"), ["`True` → `False`"]);
    }

    #[test]
    fn leaves_alone_what_isnt_a_site() {
        for src in [
            "fn f<T>(v: Vec<T>) -> Option<T> {}",
            "let s = x as f64; let y = 1.5; let z = x2 + 0;",
            "a <<= 1; b >>= 2; c => d; e <=> f",
            "println!(\"x\"); a -1; f(-1)",
            "x.0; t.1",
        ] {
            let w = whats(src);
            assert!(!w.iter().any(|m| m.contains('<') && !m.starts_with("`0`")), "{src}: {w:?}");
        }
        assert_eq!(whats("let z = x2 + 0;"), ["`+` → `-`", "`0` → `1`"]);
        assert!(whats("println!(\"x\")").is_empty(), "a macro's ! isn't negation");
        assert!(whats("let y = 1.5").is_empty(), "floats aren't integers");
    }

    #[test]
    fn comments_and_strings_are_skipped() {
        assert!(whats("# if a < b and 1\n// x == y\n/* 3 < 4\n and */").is_empty());
        assert!(whats("s = \"a < b and 1\"; t = 'x == 2'; u = `3 < 4`").is_empty());
        assert!(whats("\"\"\"doc\n a < b and 1\n\"\"\"").is_empty(), "triple-quoted strings span lines");
        assert_eq!(whats("s = \"say \\\"hi\\\"\" == t"), ["`==` → `!=`"], "escaped quotes");
        assert_eq!(whats("s = 'unclosed\nx == y"), ["`==` → `!=`"], "a single-line string ends at the newline");
        let m = mutants("a\nb == c");
        assert_eq!(m[0].line, 2);
    }

    #[test]
    fn apply_and_spread() {
        let src = "if a <= b: return 10";
        let m = mutants(src);
        assert_eq!(m[0].apply(src), "if a < b: return 10");
        assert_eq!(m[1].apply(src), "if a <= b: return 11");
        assert_eq!(mutants("é == ü")[0].apply("é == ü"), "é != ü", "byte offsets on a char boundary");
        let many: Vec<Mutant> = (0..10).map(|k| Mutant { line: k, start: 0, end: 0, to: String::new(), what: k.to_string() }).collect();
        let picked: Vec<usize> = spread(many, 3).iter().map(|m| m.line).collect();
        assert_eq!(picked, [0, 3, 6]);
    }

    /// Both mutants of `x == 3` keep the file's size and are written within
    /// the second the original ran in: Python's bytecode cache would hide
    /// them if the file's time didn't move.
    #[cfg(unix)]
    #[test]
    fn same_size_mutants_are_not_hidden_by_caches() {
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            eprintln!("skipping: python3 missing");
            return;
        }
        let d = tempfile::tempdir().expect("tempdir");
        let code = d.path().join("solution.py");
        std::fs::write(&code, "def f(x):\n    return x == 3\n").expect("write");
        let t = d.path().join("t.py");
        std::fs::write(&t, format!("import sys\nsys.path.insert(0, {:?})\nfrom solution import f\nassert f(3)\nassert not f(4)\n", d.path().display().to_string())).expect("write");
        let r = run(&["--file".into(), code.to_string_lossy().into_owned(), "--run".into(), format!("python3 {}", shlex::try_quote(&t.to_string_lossy()).expect("quotable")), "--min-killed".into(), "1".into()]);
        match r {
            Ok(Outcome::Pass(s)) => assert!(s.starts_with("caught 2 of 2"), "{s}"),
            Ok(Outcome::Fail(Fail(l))) => panic!("{l:?}"),
            Err(e) => panic!("{e}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn scores_tests_against_mutants_and_restores_the_file() {
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            eprintln!("skipping: python3 missing");
            return;
        }
        let d = tempfile::tempdir().expect("tempdir");
        let code = d.path().join("solution.py");
        let src = "def is_adult(age):\n    return age >= 18\n";
        std::fs::write(&code, src).expect("write");
        let script = |body: &str| {
            let p = d.path().join("t.py");
            std::fs::write(&p, format!("import sys\nsys.path.insert(0, {:?})\nfrom solution import is_adult\n{body}\n", d.path().display().to_string())).expect("write");
            format!("python3 {}", shlex::try_quote(&p.to_string_lossy()).expect("quotable"))
        };
        let go = |run: &str, min: &str| {
            super::run(&["--file".into(), code.to_string_lossy().into_owned(), "--run".into(), run.into(), "--min-killed".into(), min.into()])
        };
        // Strong tests: the boundary on both sides.
        let strong = script("assert is_adult(18)\nassert not is_adult(17)");
        match go(&strong, "1.0") {
            Ok(Outcome::Pass(s)) => assert!(s.starts_with("caught 2 of 2"), "{s}"),
            other => panic!("strong tests: {:?}", other.map(|o| matches!(o, Outcome::Pass(_)))),
        }
        assert_eq!(std::fs::read_to_string(&code).expect("read"), src, "the file is restored");
        // Weak tests: far from the boundary.
        let weak = script("assert is_adult(30)");
        match go(&weak, "0.5") {
            Ok(Outcome::Fail(Fail(l))) => {
                assert!(l[0].starts_with("caught 0 of 2"), "{l:?}");
                assert!(l[1].starts_with("line 2: `>=` → `>`"), "{l:?}");
            }
            _ => panic!("weak tests must fail"),
        }
        let wrong = script("assert is_adult(5)");
        assert!(matches!(go(&wrong, "0.5"), Ok(Outcome::Fail(Fail(l))) if l[0].contains("fail on the code itself")));
        assert_eq!(std::fs::read_to_string(&code).expect("read"), src);
    }
}
