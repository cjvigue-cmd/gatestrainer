//! `scope`: a change stayed where it was allowed — every file under ROOT
//! that differs from the baseline copy (changed, added or removed) is at an
//! allowed path. A bug fix that also rewrites an unrelated module fails.

use super::{Args, Fail, Outcome, Run};
use std::collections::BTreeMap;
use std::path::{Component, Path};

/// Files the walk will list before refusing (a runaway tree, not a repo).
const MAX_FILES: usize = 50_000;

/// What a path holds, compared by value.
#[derive(PartialEq)]
enum Entry {
    File(Vec<u8>),
    Link(std::path::PathBuf),
}

/// `rel` with `/` separators, whatever the OS.
fn rel_key(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// `path` equals `rule`, or lies under it when `rule` ends in `/`.
fn covered(path: &str, rule: &str) -> bool {
    match rule.strip_suffix('/') {
        Some(dir) => path.starts_with(&format!("{dir}/")) || dir.is_empty(),
        None => path == rule,
    }
}

/// Every file and symlink under `root` (symlinks are not followed), keyed by
/// relative path; `skip` is checked against each relative path.
fn walk(root: &Path, skip: &dyn Fn(&str) -> bool) -> Result<BTreeMap<String, Entry>, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = entry.path();
            let rel = path.strip_prefix(root).map_err(|e| format!("{}: {e}", path.display()))?;
            let key = rel_key(rel);
            if skip(&key) {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                out.insert(key, Entry::Link(target));
            } else if meta.is_dir() {
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                out.insert(key, Entry::File(bytes));
            }
            if out.len() > MAX_FILES {
                return Err(format!("{}: more than {MAX_FILES} files", root.display()));
            }
        }
    }
    Ok(out)
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["baseline", "allow", "ignore"])?;
    let baseline = Path::new(a.need("baseline")?);
    let root = match a.positional.as_slice() {
        [] => Path::new("."),
        [r] => Path::new(r.as_str()),
        _ => return Err("give at most one ROOT".into()),
    };
    let allow = a.all("allow");
    let ignore = a.all("ignore");
    if allow.is_empty() {
        return Err("give at least one --allow PATH (nothing may change otherwise; say so with a count check instead)".into());
    }
    if !baseline.is_dir() {
        return Err(format!("--baseline {} is not a folder", baseline.display()));
    }
    // The baseline usually sits inside ROOT (a gate's support files); never
    // compare it with itself.
    let base_canon = std::fs::canonicalize(baseline).map_err(|e| format!("--baseline: {e}"))?;
    let root_canon = std::fs::canonicalize(root).map_err(|e| format!("ROOT {}: {e}", root.display()))?;
    let base_in_root = base_canon.strip_prefix(&root_canon).ok().map(rel_key);
    let skip = |key: &str| {
        ignore.iter().any(|r| covered(key, r)) || base_in_root.as_deref().is_some_and(|b| !b.is_empty() && (key == b || key.starts_with(&format!("{b}/"))))
    };
    let before = walk(baseline, &skip)?;
    // ROOT holds the answer's work: a file it left unreadable is its failure.
    let after = match walk(root, &skip) {
        Ok(a) => a,
        Err(e) => return Ok(Outcome::Fail(Fail(vec![format!("the result can't be read: {e}")]))),
    };
    let mut changed = Vec::new();
    for (key, entry) in &after {
        match before.get(key) {
            Some(old) if old == entry => {}
            Some(_) => changed.push((key.as_str(), "modified")),
            None => changed.push((key.as_str(), "added")),
        }
    }
    for key in before.keys().filter(|k| !after.contains_key(*k)) {
        changed.push((key.as_str(), "removed"));
    }
    changed.sort();
    let outside: Vec<String> = changed
        .iter()
        .filter(|(k, _)| !allow.iter().any(|r| covered(k, r)))
        .map(|(k, how)| format!("{k} ({how}) is outside the allowed scope"))
        .collect();
    if outside.is_empty() {
        let inside: Vec<String> = changed.iter().map(|(k, how)| format!("{k} {how}")).collect();
        Ok(Outcome::Pass(format!("{} change(s), all in scope{}{}", changed.len(), if inside.is_empty() { "" } else { ": " }, inside.join(", "))))
    } else {
        let mut lines = outside;
        lines.push(format!("allowed: {}", allow.join(", ")));
        Ok(Outcome::Fail(Fail(lines)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).expect("mkdir");
        }
        std::fs::write(p, text).expect("write");
    }

    fn go(root: &Path, extra: &[&str]) -> Run {
        let mut v: Vec<String> = vec!["--baseline".into(), root.join(".baseline").to_string_lossy().into_owned()];
        v.extend(extra.iter().map(|s| s.to_string()));
        v.push(root.to_string_lossy().into_owned());
        run(&v)
    }

    /// A repo with its baseline copy inside it, as a gate's sandbox has.
    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        for base in ["", ".baseline/"] {
            write(d.path(), &format!("{base}src/lib.rs"), "fn a() {}\n");
            write(d.path(), &format!("{base}src/util.rs"), "fn b() {}\n");
            write(d.path(), &format!("{base}README.md"), "hi\n");
        }
        d
    }

    #[test]
    fn unchanged_and_in_scope_pass() {
        let d = repo();
        assert!(matches!(go(d.path(), &["--allow", "src/lib.rs"]), Ok(Outcome::Pass(_))), "no change at all");
        write(d.path(), "src/lib.rs", "fn a() { fixed() }\n");
        match go(d.path(), &["--allow", "src/lib.rs"]) {
            Ok(Outcome::Pass(s)) => assert!(s.contains("src/lib.rs modified"), "{s}"),
            _ => panic!("in scope"),
        }
        write(d.path(), "src/new.rs", "");
        assert!(matches!(go(d.path(), &["--allow", "src/"]), Ok(Outcome::Pass(_))), "a folder rule covers additions");
    }

    #[test]
    fn out_of_scope_changes_fail_with_how() {
        let d = repo();
        write(d.path(), "src/lib.rs", "fn a() { fixed() }\n");
        write(d.path(), "src/util.rs", "fn b() { rewritten() }\n");
        std::fs::remove_file(d.path().join("README.md")).expect("rm");
        write(d.path(), "notes.txt", "x");
        match go(d.path(), &["--allow", "src/lib.rs"]) {
            Ok(Outcome::Fail(Fail(l))) => {
                assert_eq!(l.len(), 4, "{l:?}");
                assert!(l[0].starts_with("README.md (removed)"), "{l:?}");
                assert!(l[1].starts_with("notes.txt (added)"), "{l:?}");
                assert!(l[2].starts_with("src/util.rs (modified)"), "{l:?}");
            }
            _ => panic!("out of scope"),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = d.path().join("src/locked.rs");
            std::fs::write(&locked, "x").expect("write");
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
            let r = go(d.path(), &["--allow", "src/"]);
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).expect("chmod back");
            // Root reads anything, so only a non-root run can see the refusal.
            if std::fs::metadata("/root").is_ok_and(|m| m.permissions().mode() & 0o004 == 0) && std::fs::read_dir("/root").is_err() {
                assert!(matches!(r, Ok(Outcome::Fail(_))), "an unreadable result file is the answer's failure");
            }
        }
    }

    #[test]
    fn ignored_paths_and_build_output() {
        let d = repo();
        write(d.path(), "target/debug/x", "bin");
        write(d.path(), "Cargo.lock", "lock");
        assert!(matches!(go(d.path(), &["--allow", "src/lib.rs", "--ignore", "target/", "--ignore", "Cargo.lock"]), Ok(Outcome::Pass(_))));
        assert!(matches!(go(d.path(), &["--allow", "src/lib.rs", "--ignore", "target/"]), Ok(Outcome::Fail(_))));
        assert!(!covered("targets/x", "target/"), "a folder rule is not a prefix of a longer name");
    }

    #[test]
    fn misconfigured() {
        let d = repo();
        assert!(go(d.path(), &[]).is_err(), "no --allow");
        assert!(run(&["--baseline".into(), d.path().join("nope").to_string_lossy().into_owned(), "--allow".into(), "x".into()]).is_err());
    }
}
