//! The primitive checks: each one deterministic, reading only the files
//! it is given, run from a gate's command line like any other program
//! (`gatestrainer <primitive> …`) or through [`cli`].
//!
//! Exit codes: 0 the artifact passes, 1 it fails (the reason on stderr is
//! what a retry loop feeds back to the model), 2 the check itself is
//! wrong (bad arguments, a bad pattern or schema), so a gate author can
//! tell their mistake from the model's.

mod bounds;
mod count;
mod csv;
mod differential;
mod flattery;
pub(crate) mod mutation;
mod numeric;
pub(crate) mod overclaim;
pub(crate) mod claims;
pub(crate) mod placeholder;
mod refusal;
pub(crate) mod date;
pub(crate) mod secrets;
mod preserve;
mod quote;
mod regex;
mod resolve;
mod schema;
mod scope;
mod text;

use std::path::Path;

/// Failure lines shown per check: enough to act on, short enough for the
/// model's next prompt.
const MAX_REPORTED: usize = 20;

pub const USAGE: &str = "\
gatestrainer <primitive> [options]   (exit 0 pass, 1 fail, 2 bad check)

  count   --pattern RE [--fixed] [--eq N | --min N --max N] FILE...
          how often a pattern occurs across the files
  numeric --file F --pattern RE --expect X [--abs T] [--rel T] [--all]
          the value in RE's first group is within |v-X| <= T_abs + T_rel*|X|;
          exactly one match unless --all (then every match)
  schema  --schema S.json FILE
          FILE is JSON that fits S (a JSON Schema subset; unsupported keywords refused)
  csv     --columns name:type,... [--min-rows N] [--max-rows N] FILE
          header is exactly the names; types str|int|float|bool, `?` allows empty
  scope   --baseline DIR [--allow PATH]... [--ignore PATH]... [ROOT]
          files under ROOT (default .) differ from DIR only at allowed paths
          (a PATH ending in / covers a folder)
  differential --cases FILE --left CMD --right CMD
          both commands read FILE on stdin and print one line per case (non-empty
          input line); every line must agree. LEFT is the reference
  mutation --file F --run CMD [--build CMD] [--min-killed 0.8] [--max-mutants 40]
          CMD passes on F, then fails on at least that share of F's mutants
          (comparisons, and/or, true/false, integers +1; comments and strings
          skipped); a mutant that fails --build doesn't count
  resolve [--root DIR] [--symbols-in FILE]... [--mention a,b] DOC...
          relative links/images exist under ROOT (default .) and #anchors name
          headings (web links not fetched); code names in `inline code` exist
          in the --symbols-in files (names the doc's own examples define are
          fine); every --mention name appears in inline code or a code block
  preserve --from A --to B --keep numbers,urls,code,placeholders,headings [--terms a,b]
          every item of those kinds in A appears in B at least as often; headings
          = the same outline levels. From a summary to its source: no invented numbers
  bounds  --file F [--words MIN:MAX] [--lines MIN:MAX] [--chars MIN:MAX]
          [--ratio-of SRC --ratio MIN:MAX]   size limits; ratio = words against SRC
  quote   --source SRC --file F [--min-quotes N]
          every quotation in F (\"…\", “…”, > block quotes; 4+ words) is in SRC
          word for word (case, curly quotes, dashes and spacing aside)
  overclaim --file F --source SRC [--source …] [--allow WORD …]
          F uses no sweeping word (all, never, completely, eliminates, free …)
          that no SRC uses: an answer claims no more than its source (wording
          only; negated words and quotations from SRC don't count)
  flattery --file F [--source SRC …] [--allow PHRASE …]
          F doesn't fawn: no praise of the asker (great question, you're
          absolutely right), eager openers (Certainly!, I'd be happy to) or
          grovelling (I apologize for the confusion) — unless a SRC says it
  placeholder --file F [--file …] [--allow PATTERN …]
          F is finished: no \"rest of the code unchanged\", \"your code here\",
          todo!()/unimplemented!(), raise NotImplementedError, pass  # TODO,
          lines that are only \"...\", or TODO/FIXME left in
  claims  --file F [--file …] [--allow PHRASE …]
          F claims no work that did not happen (I ran the tests, all tests pass,
          has been tested, verified to work, production-ready): nothing runs
          while an answer is written
  secrets --file F [--file …]
          F holds no credentials: known key formats (AWS, GitHub, OpenAI,
          Anthropic, OpenRouter, Hugging Face, Google, Stripe, Slack, JWT,
          private key blocks) or random-looking values assigned to a
          password/secret/token/key; the report shows only their first characters
  refusal --file F [--source SRC …]
          F does the task: no deflections (As an AI…, I'm sorry, but I can't…,
          I'm unable to…, I don't have access to…) unless a SRC says it
  date    --file F [--source SRC …] [--expect YYYY-MM-DD [--pattern P] [--all]]
          [--order mdy|dmy]   every date in F is real (no February 30) and its
          weekday right; with --expect, F's date (the one in P's group 1, or the
          only one) is that day; 10/08/2026 is read only with --order
  regex   --file F --match FILE --no-match FILE [--engine rust|python|javascript]
          [--mode search|full]   the pattern in F (one line) matches every --match
          line and no --no-match line (blank lines skipped), in that engine
          (default rust); full = the whole line, search (default) = anywhere in it
";

pub(crate) use crate::config::{bin_name, NODE_BIN_ENV, PYTHON_BIN_ENV};

/// The artifact failed: one line per problem.
pub(crate) struct Fail(pub Vec<String>);

/// The outcome of one primitive: `Ok(summary)` passes, `Err(Bad)` is a
/// misconfigured check (exit 2).
pub(crate) enum Outcome {
    Pass(String),
    Fail(Fail),
}

pub(crate) type Run = Result<Outcome, String>;

/// `--check …` from the command line; returns the exit code.
pub fn cli(args: &[String]) -> i32 {
    let Some((name, rest)) = args.split_first() else {
        eprint!("{USAGE}");
        return 2;
    };
    let run = match name.as_str() {
        "count" => count::run(rest),
        "numeric" => numeric::run(rest),
        "schema" => schema::run(rest),
        "csv" => csv::run(rest),
        "scope" => scope::run(rest),
        "differential" => differential::run(rest),
        "mutation" => mutation::run(rest),
        "resolve" => resolve::run(rest),
        "preserve" => preserve::run(rest),
        "bounds" => bounds::run(rest),
        "quote" => quote::run(rest),
        "overclaim" => overclaim::run(rest),
        "flattery" => flattery::run(rest),
        "placeholder" => placeholder::run(rest),
        "claims" => claims::run(rest),
        "secrets" => secrets::run(rest),
        "refusal" => refusal::run(rest),
        "date" => date::run(rest),
        "regex" => regex::run(rest),
        other => Err(format!("unknown check `{other}`\n\n{USAGE}")),
    };
    match run {
        Ok(Outcome::Pass(summary)) => {
            println!("{name}: ok — {summary}");
            0
        }
        Ok(Outcome::Fail(Fail(lines))) => {
            // The first problem on the first line: that line becomes the
            // verdict's reason in the logs.
            let shown = lines.len().min(MAX_REPORTED);
            eprintln!("{name}: FAILED — {}", lines.first().map_or("", String::as_str));
            for l in lines.iter().take(shown).skip(1) {
                eprintln!("  {l}");
            }
            if lines.len() > shown {
                eprintln!("  … and {} more", lines.len() - shown);
            }
            1
        }
        Err(why) => {
            eprintln!("{name}: the check is misconfigured: {why}");
            2
        }
    }
}

/// `--key value` options and positional arguments, parsed once.
pub(crate) struct Args {
    pairs: Vec<(String, Option<String>)>,
    pub positional: Vec<String>,
}

impl Args {
    /// `flags` take no value; every other `--name` takes the next argument.
    pub fn parse(args: &[String], flags: &[&str]) -> Result<Self, String> {
        let mut pairs = Vec::new();
        let mut positional = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if let Some(name) = a.strip_prefix("--") {
                if flags.contains(&name) {
                    pairs.push((name.to_string(), None));
                } else {
                    let v = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
                    pairs.push((name.to_string(), Some(v.clone())));
                }
            } else {
                positional.push(a.clone());
            }
        }
        Ok(Self { pairs, positional })
    }

    /// Refuse options the check doesn't know, so a typo can't silently
    /// weaken a gate.
    pub fn only(&self, known: &[&str]) -> Result<(), String> {
        match self.pairs.iter().find(|(k, _)| !known.contains(&k.as_str())) {
            Some((k, _)) => Err(format!("unknown option --{k}")),
            None => Ok(()),
        }
    }

    pub fn flag(&self, name: &str) -> bool {
        self.pairs.iter().any(|(k, _)| k == name)
    }

    /// The single value of an option given at most once.
    pub fn one(&self, name: &str) -> Result<Option<&str>, String> {
        match self.all(name).as_slice() {
            [] => Ok(None),
            [v] => Ok(Some(v)),
            _ => Err(format!("--{name} given more than once")),
        }
    }

    pub fn need(&self, name: &str) -> Result<&str, String> {
        self.one(name)?.ok_or_else(|| format!("--{name} is required"))
    }

    pub fn all(&self, name: &str) -> Vec<&str> {
        self.pairs.iter().filter(|(k, _)| k == name).filter_map(|(_, v)| v.as_deref()).collect()
    }

    pub fn number<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>, String> {
        self.one(name)?
            .map(|v| v.trim().parse::<T>().map_err(|_| format!("--{name} {v:?} is not a valid number")))
            .transpose()
    }
}

/// Read an artifact file: missing or unreadable is the artifact's failure,
/// not the check's.
pub(crate) fn read_artifact(path: &str) -> Result<String, Fail> {
    std::fs::read_to_string(Path::new(path)).map_err(|e| Fail(vec![format!("{path}: can't be read ({e})")]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn args_parse_and_refuse_unknown_or_repeated() {
        let a = Args::parse(&s(&["--pattern", "x", "--fixed", "f1", "f2"]), &["fixed"]).expect("parse");
        assert_eq!(a.need("pattern"), Ok("x"));
        assert!(a.flag("fixed"));
        assert_eq!(a.positional, s(&["f1", "f2"]));
        assert!(a.only(&["pattern", "fixed"]).is_ok());
        assert_eq!(a.only(&["pattern"]), Err("unknown option --fixed".into()));
        assert!(Args::parse(&s(&["--pattern"]), &[]).is_err(), "a missing value is refused");
        let twice = Args::parse(&s(&["--eq", "1", "--eq", "2"]), &[]).expect("parse");
        assert!(twice.one("eq").is_err());
        let bad = Args::parse(&s(&["--eq", "one"]), &[]).expect("parse");
        assert!(bad.number::<u64>("eq").is_err());
    }

    #[test]
    fn exit_codes() {
        assert_eq!(cli(&[]), 2);
        assert_eq!(cli(&s(&["nope"])), 2);
        assert_eq!(cli(&s(&["count", "--pattern", "x"])), 2, "no files and no bound");
    }
}
