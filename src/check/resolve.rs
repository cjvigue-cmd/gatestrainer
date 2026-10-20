//! `resolve`: what a document points at exists — relative links and images
//! name files under the root, `#anchors` name headings, and (when asked)
//! code names in inline code exist in the given sources and the names the
//! document must cover are there. Offline: web links are counted, not
//! fetched (design/gates.md, decided 2026-10-08).

use super::{read_artifact, Args, Fail, Outcome, Run};
use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// GitHub's heading anchor: lowercase, punctuation dropped (except `-` and
/// `_`), spaces to `-`.
pub(crate) fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// A document split into prose lines (fences removed) and its headings.
struct Doc {
    /// (line number, text) outside fenced code blocks.
    prose: Vec<(usize, String)>,
    /// The lines inside fenced code blocks.
    code: Vec<String>,
    anchors: BTreeSet<String>,
}

fn parse(text: &str) -> Doc {
    let mut prose = Vec::new();
    let mut code = Vec::new();
    let mut anchors = BTreeSet::new();
    let mut fence: Option<String> = None;
    let link = regex::Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").ok();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim_start();
        let marker: String = t.chars().take_while(|&c| c == '`' || c == '~').collect();
        if marker.len() >= 3 && marker.chars().all(|c| c == marker.chars().next().unwrap_or('`')) {
            match &fence {
                None => fence = Some(marker),
                Some(open) if marker.starts_with(open.as_str()) && t.trim() == marker => fence = None,
                Some(_) => {}
            }
            continue;
        }
        if fence.is_some() {
            code.push(line.to_string());
            continue;
        }
        if let Some(h) = t.strip_prefix('#') {
            let h = h.trim_start_matches('#');
            if h.starts_with(' ') || h.is_empty() {
                // Inline code and link syntax read as their text in an anchor:
                // `[the API](api.md)` is "the API"; closing `##` aren't text.
                let h = h.trim_end();
                let h = match h.trim_end_matches('#') {
                    t if t.len() < h.len() && (t.is_empty() || t.ends_with(' ')) => t,
                    _ => h,
                };
                let text = link.as_ref().map_or_else(|| h.to_string(), |r| r.replace_all(h, "$1").into_owned()).replace('`', "");
                let mut s = slug(&text);
                // Repeated headings get -1, -2 … like GitHub's.
                let base = s.clone();
                let mut n = 1;
                while anchors.contains(&s) {
                    s = format!("{base}-{n}");
                    n += 1;
                }
                anchors.insert(s);
            }
        }
        prose.push((i + 1, line.to_string()));
    }
    Doc { prose, code, anchors }
}

/// The identifiers in `text`.
fn idents(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| w.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_'))
}

/// Is `span` clearly a code name (not an English word)?
fn code_name(span: &str) -> Option<String> {
    let s = span.trim().trim_end_matches("()");
    let ok = !s.is_empty()
        && s.split("::").all(|p| {
            let mut cs = p.chars();
            cs.next().is_some_and(|c| c.is_alphabetic() || c == '_') && cs.all(|c| c.is_alphanumeric() || c == '_')
        });
    if !ok {
        return None;
    }
    let last = s.rsplit("::").next().unwrap_or(s);
    // CamelCase needs a lower-case letter too: `README` is a word.
    let camel = last.chars().any(char::is_lowercase) && last.chars().skip(1).any(char::is_uppercase);
    let codey = s.contains("::") || span.trim().ends_with("()") || last.contains('_') || camel;
    codey.then(|| last.to_string())
}

/// `target` (a link's path part, decoded) from the document's folder,
/// kept under `root`.
fn locate(doc_dir: &Path, root: &Path, target: &str) -> Result<PathBuf, String> {
    let decoded = target.replace("%20", " ");
    let p = Path::new(&decoded);
    if p.is_absolute() {
        return Err("an absolute path".into());
    }
    let mut out = doc_dir.to_path_buf();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    return Err("goes above the root".into());
                }
            }
            Component::Normal(n) => out.push(n),
            _ => {}
        }
    }
    if !out.starts_with(root) {
        return Err("points outside the root".into());
    }
    Ok(out)
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["root", "symbols-in", "mention"])?;
    if a.positional.is_empty() {
        return Err("give the document(s) to check".into());
    }
    let root = Path::new(a.one("root")?.unwrap_or("."));
    let root = std::fs::canonicalize(root).map_err(|e| format!("--root {}: {e}", root.display()))?;
    let mut sources = String::new();
    for f in a.all("symbols-in") {
        sources.push_str(&std::fs::read_to_string(f).map_err(|e| format!("--symbols-in {f}: {e}"))?);
        sources.push('\n');
    }
    let words: BTreeSet<&str> = sources.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).collect();
    let mention: Vec<String> = a.one("mention")?.map(|m| m.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    let link = Regex::new(r"!?\[[^\]]*\]\(\s*<?([^)\s>]+)>?(?:\s+[^)]*)?\)").map_err(|e| e.to_string())?;
    let refdef = Regex::new(r"^\s{0,3}\[[^\]]+\]:\s*<?(\S+?)>?(?:\s+.*)?$").map_err(|e| e.to_string())?;
    let code = Regex::new(r"`([^`\n]+)`").map_err(|e| e.to_string())?;
    // A name the document's own examples define (`def helper`, `let x =`)
    // is the document's, not a claim about the code.
    let defines = Regex::new(r"(?:\b(?:def|fn|function|class|struct|enum|const|let|var|type|interface)\s+([A-Za-z_][A-Za-z0-9_]*))|(?:^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=[^=])").map_err(|e| e.to_string())?;

    let mut bad = Vec::new();
    let (mut links, mut web, mut names) = (0usize, 0usize, 0usize);
    let mut mentioned: BTreeSet<String> = BTreeSet::new();
    for doc_path in &a.positional {
        let text = match read_artifact(doc_path) {
            Ok(t) => t,
            Err(f) => return Ok(Outcome::Fail(f)),
        };
        let doc = parse(&text);
        let mut own: BTreeSet<String> = BTreeSet::new();
        for line in &doc.code {
            mentioned.extend(idents(line).map(str::to_string));
            for cap in defines.captures_iter(line) {
                if let Some(m) = cap.get(1).or_else(|| cap.get(2)) {
                    own.insert(m.as_str().to_string());
                }
            }
        }
        let doc_file = std::fs::canonicalize(doc_path).map_err(|e| format!("{doc_path}: {e}"))?;
        let doc_dir = doc_file.parent().map(Path::to_path_buf).unwrap_or_else(|| root.clone());
        for (n, line) in &doc.prose {
            // Code spans first: a link inside backticks is an example, not a link.
            let mut prose = line.clone();
            for cap in code.captures_iter(line) {
                let span = &cap[1];
                mentioned.extend(idents(span).map(str::to_string));
                if let Some(name) = code_name(span) {
                    if !sources.is_empty() && !own.contains(&name) {
                        names += 1;
                        if !words.contains(name.as_str()) {
                            bad.push(format!("{doc_path}:{n}: `{}` isn't in the sources", span.trim()));
                        }
                    }
                }
                prose = prose.replacen(&cap[0], "", 1);
            }
            let targets: Vec<String> = link.captures_iter(&prose).chain(refdef.captures_iter(&prose)).map(|c| c[1].to_string()).collect();
            for target in targets {
                links += 1;
                let lower = target.to_lowercase();
                if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:") {
                    web += 1;
                    continue;
                }
                let (path, anchor) = match target.split_once('#') {
                    Some((p, a)) => (p.split('?').next().unwrap_or(p), Some(a)),
                    None => (target.split('?').next().unwrap_or(&target), None),
                };
                let file = if path.is_empty() {
                    doc_file.clone()
                } else {
                    match locate(&doc_dir, &root, path) {
                        Ok(f) => f,
                        Err(why) => {
                            bad.push(format!("{doc_path}:{n}: link {target:?} {why}"));
                            continue;
                        }
                    }
                };
                if !file.exists() {
                    bad.push(format!("{doc_path}:{n}: link {target:?}: no such file"));
                    continue;
                }
                if let Some(anchor) = anchor.filter(|a| !a.is_empty()) {
                    let anchors = if file == doc_file {
                        doc.anchors.clone()
                    } else if file.extension().is_some_and(|x| x == "md" || x == "markdown") {
                        match std::fs::read_to_string(&file) {
                            Ok(t) => parse(&t).anchors,
                            Err(e) => {
                                bad.push(format!("{doc_path}:{n}: link {target:?}: can't read it ({e})"));
                                continue;
                            }
                        }
                    } else {
                        continue;
                    };
                    if !anchors.contains(&anchor.to_lowercase()) {
                        bad.push(format!("{doc_path}:{n}: link {target:?}: no heading #{anchor} (there are: {})", anchors.iter().take(8).cloned().collect::<Vec<_>>().join(", ")));
                    }
                }
            }
        }
    }
    for m in &mention {
        if !mentioned.contains(m) {
            bad.push(format!("`{m}` is never mentioned (in inline code or a code block)"));
        }
    }
    if bad.is_empty() {
        let mut s = format!("{links} links resolve ({web} web links not checked offline)");
        if !sources.is_empty() {
            s.push_str(&format!("; {names} code names found in the sources"));
        }
        if !mention.is_empty() {
            s.push_str(&format!("; all {} required names mentioned", mention.len()));
        }
        return Ok(Outcome::Pass(s));
    }
    Ok(Outcome::Fail(Fail(bad)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_like_github() {
        assert_eq!(slug("Gate Primitives"), "gate-primitives");
        assert_eq!(slug(" 3. Key Features!"), "3-key-features");
        assert_eq!(slug("`--check` & friends"), "--check--friends");
        assert_eq!(slug("snake_case"), "snake_case");
    }

    #[test]
    fn code_names_only() {
        assert_eq!(code_name("parse_csv"), Some("parse_csv".into()));
        assert_eq!(code_name("GateSpec"), Some("GateSpec".into()));
        assert_eq!(code_name("check::cli"), Some("cli".into()));
        assert_eq!(code_name("run()"), Some("run".into()));
        for not in ["cargo test", "--check", "src/lib.rs", "true", "exit", "README", "a-b"] {
            assert_eq!(code_name(not), None, "{not}");
        }
    }

    fn setup() -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(d.path().join("docs")).expect("mkdir");
        std::fs::write(d.path().join("docs").join("guide.md"), "# Guide\n## Install Steps\n").expect("write");
        std::fs::write(d.path().join("lib.rs"), "pub fn parse_csv() {}\npub struct GateSpec;\n").expect("write");
        d
    }

    fn go(d: &Path, doc: &str, extra: &[&str]) -> Run {
        let f = d.join("README.md");
        std::fs::write(&f, doc).expect("write");
        let mut v: Vec<String> = vec!["--root".into(), d.to_string_lossy().into_owned()];
        v.extend(extra.iter().map(|s| s.to_string()));
        v.push(f.to_string_lossy().into_owned());
        run(&v)
    }

    #[test]
    fn links_and_anchors() {
        let d = setup();
        let good = "# Title\n## Usage\nSee [the guide](docs/guide.md#install-steps), [usage](#usage), [web](https://example.com) and ![img](docs/guide.md).\n\n[ref]: docs/guide.md\n";
        match go(d.path(), good, &[]) {
            Ok(Outcome::Pass(s)) => assert!(s.starts_with("5 links resolve (1 web"), "{s}"),
            _ => panic!("good links"),
        }
        let bad = "# Title\n[a](missing.md) [b](docs/guide.md#nope) [c](#nowhere) [d](../../outside.md)\n";
        match go(d.path(), bad, &[]) {
            Ok(Outcome::Fail(Fail(l))) => {
                assert_eq!(l.len(), 4, "{l:?}");
                assert!(l[0].contains("no such file") && l[1].contains("no heading #nope") && l[2].contains("#nowhere"), "{l:?}");
                assert!(l[3].contains("outside the root") || l[3].contains("above the root"), "{l:?}");
            }
            _ => panic!("bad links"),
        }
    }

    #[test]
    fn fences_and_code_spans_are_not_links() {
        let d = setup();
        let doc = "# T\n```md\n[x](missing.md)\n```\nWrite `[x](missing.md)` like this.\n";
        assert!(matches!(go(d.path(), doc, &[]), Ok(Outcome::Pass(_))));
    }

    #[test]
    fn symbols_and_mentions() {
        let d = setup();
        let src = d.path().join("lib.rs").to_string_lossy().into_owned();
        let doc = "# T\nCall `parse_csv()` with a `GateSpec`; run `cargo test`.\n";
        assert!(matches!(go(d.path(), doc, &["--symbols-in", &src, "--mention", "parse_csv,GateSpec"]), Ok(Outcome::Pass(_))));
        // Defined by the document's own example: fine. Only called there: not a definition.
        let example = "# T\n```python\ndef my_helper():\n    return parse_csv()\n```\nThen `my_helper()` and `GateSpec.new`.\n";
        assert!(matches!(go(d.path(), example, &["--symbols-in", &src, "--mention", "parse_csv,GateSpec"]), Ok(Outcome::Pass(_))), "mentions count in code blocks and dotted spans");
        let invented = "# T\n```python\nparse_tsv()\n```\nThen `parse_tsv()`.\n";
        assert!(matches!(go(d.path(), invented, &["--symbols-in", &src]), Ok(Outcome::Fail(_))));
        match go(d.path(), "# T\nCall `parse_tsv()`.\n", &["--symbols-in", &src, "--mention", "parse_csv"]) {
            Ok(Outcome::Fail(Fail(l))) => {
                assert!(l[0].contains("`parse_tsv()` isn't in the sources"), "{l:?}");
                assert!(l[1].contains("`parse_csv` is never mentioned"), "{l:?}");
            }
            _ => panic!("invented name"),
        }
    }

    #[test]
    fn repeated_headings_get_numbered_anchors() {
        let d = setup();
        assert!(matches!(go(d.path(), "# A\n## Notes\n## Notes\n[x](#notes-1)\n", &[]), Ok(Outcome::Pass(_))));
    }

    #[test]
    fn closing_hashes_and_links_in_headings_read_as_github_does() {
        let d = setup();
        assert!(matches!(go(d.path(), "# A\n## Install ##\n## See [the API](https://x.example/api)\n## C# notes\n[a](#install) [b](#see-the-api) [c](#c-notes)\n", &[]), Ok(Outcome::Pass(_))));
    }
}
