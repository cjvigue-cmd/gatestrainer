//! `secrets`: an answer carries no credentials — no API key in a known
//! format, no private key block, no token, no long random-looking string
//! assigned to a password/secret/token/key. A model may paste one it saw in
//! its context, or invent a real-looking one that a reader then trusts.
//! The report never repeats what it found: only its first characters.

use super::{read_artifact, Args, Fail, Outcome, Run};
use regex::Regex;

/// Known credential formats: (pattern, what it is).
const FORMATS: &[(&str, &str)] = &[
    (r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY(?: BLOCK)?-----", "a private key"),
    (r"\bAKIA[0-9A-Z]{16}\b", "an AWS access key"),
    (r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36}\b|\bgithub_pat_[A-Za-z0-9_]{50,}", "a GitHub token"),
    (r"\bsk-ant-[A-Za-z0-9_\-]{20,}", "an Anthropic key"),
    (r"\bsk-or-(?:v1-)?[A-Za-z0-9]{20,}", "an OpenRouter key"),
    (r"\bsk-(?:proj-)?[A-Za-z0-9_\-]{32,}", "an OpenAI-style key"),
    (r"\bhf_[A-Za-z0-9]{30,}\b", "a Hugging Face token"),
    (r"\bAIza[0-9A-Za-z_\-]{35}\b", "a Google API key"),
    (r"\b(?:sk|rk)_live_[0-9A-Za-z]{20,}\b", "a Stripe live key"),
    (r"\bxox[abprs]-[0-9A-Za-z\-]{10,}", "a Slack token"),
    (r"\beyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}", "a JSON web token"),
];

/// A credential-looking name assigned a quoted literal (the value is checked
/// for randomness separately): `api_key = "…"`, `"password": "…"`, and
/// with a prefix — `DB_PASSWORD`, `aws_secret_access_key`, `MY_API_KEY`.
const ASSIGNED: &str = r#"(?i)(?:^|[^A-Za-z0-9])(?:[A-Za-z0-9]+[_-])*(?:api[_-]?key|secret(?:[_-]?access)?(?:[_-]?key)?|access[_-]?key|access[_-]?token|auth[_-]?token|token|password|passwd|pwd|private[_-]?key|client[_-]?secret)["']?\s*[:=]\s*["']([^"'\s]{16,})["']"#;

/// The same in a `.env` file or a shell export: the value unquoted, to the
/// end of the line (`API_KEY=Jq8v…`).
/// Names in capitals, as `.env` files write them; a value with no brackets
/// or dots (`token = tokenizer.encode(text)` is code, not a key).
const ENV_ASSIGNED: &str = r#"^\s*(?:export\s+)?(?:[A-Z0-9]+_)*(?:API_?KEY|SECRET(?:_ACCESS)?(?:_KEY)?|ACCESS_KEY|ACCESS_TOKEN|AUTH_TOKEN|TOKEN|PASSWORD|PASSWD|PWD|PRIVATE_KEY|CLIENT_SECRET)\s*=\s*([^\s"'#()\[\].]{16,})\s*$"#;

/// Shannon entropy per character: random keys are high, words are low.
fn entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let n = s.chars().count() as f64;
    counts.values().map(|&k| { let p = k as f64 / n; -p * p.log2() }).sum()
}

/// A value that's plainly a stand-in, not a credential.
fn placeholder(v: &str) -> bool {
    let l = v.to_lowercase();
    ["your", "example", "xxxx", "<", "${", "{{", "changeme", "replace", "dummy", "test", "fake", "redacted", "env", "os.environ", "process.env"].iter().any(|p| l.contains(p))
}

/// The first characters of a secret, never the rest.
fn masked(s: &str) -> String {
    format!("{}…", s.chars().take(6).collect::<String>())
}

/// Every credential in `text`: (line, what it is, masked).
pub(crate) fn secrets(text: &str) -> Vec<(usize, &'static str, String)> {
    let formats: Vec<(Regex, &str)> = FORMATS.iter().filter_map(|(p, w)| Regex::new(p).ok().map(|r| (r, *w))).collect();
    let assigned = [Regex::new(ASSIGNED).ok(), Regex::new(ENV_ASSIGNED).ok()];
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if let Some((m, what)) = formats.iter().find_map(|(r, w)| r.find(line).map(|m| (m, *w))) {
            if !placeholder(m.as_str()) {
                out.push((i + 1, what, masked(m.as_str())));
                continue;
            }
        }
        if let Some(c) = assigned.iter().flatten().find_map(|r| r.captures(line)) {
            let v = c.get(1).map_or("", |m| m.as_str());
            if !placeholder(v) && entropy(v) >= 3.5 {
                out.push((i + 1, "a credential assigned in the code", masked(v)));
            }
        }
    }
    out
}

/// The report a gate gives, or None: what `prechecks: ["secrets"]` runs.
pub(crate) fn report(text: &str, file: &str) -> Option<String> {
    let found = secrets(text);
    let (l, w, m) = found.first()?;
    let more = match found.len() - 1 {
        0 => String::new(),
        n => format!(" (and {n} more)"),
    };
    let mut out = vec![format!("secrets: FAILED — {file} line {l} holds {w} ({m}){more}: read credentials from the environment or a settings file, never write them into the answer")];
    out.extend(found.iter().skip(1).take(10).map(|(l, w, m)| format!("  {file} line {l}: {w} ({m})")));
    Some(out.join("\n"))
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["file"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let files = a.all("file");
    if files.is_empty() {
        return Err("give the answer to check: --file F (repeatable)".into());
    }
    let mut lines = Vec::new();
    for file in files {
        let text = match read_artifact(file) {
            Ok(t) => t,
            Err(f) => return Ok(Outcome::Fail(f)),
        };
        lines.extend(secrets(&text).into_iter().map(|(l, w, m)| format!("{file} line {l}: {w} ({m})")));
    }
    if lines.is_empty() {
        return Ok(Outcome::Pass("no credentials".into()));
    }
    let mut out = vec![format!("{} — read credentials from the environment, never write them into the answer", lines[0])];
    out.extend(lines.into_iter().skip(1).take(10));
    Ok(Outcome::Fail(Fail(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<&'static str> {
        secrets(text).into_iter().map(|(_, w, _)| w).collect()
    }

    #[test]
    fn credentials_are_caught_and_never_repeated() {
        // Assembled at run time so this file holds no key-shaped literal.
        let aws = format!("{}{}", "AKIA", "Q3EGRZ7XMPLK4TBN");
        assert_eq!(kinds(&format!("key = '{aws}'")), ["an AWS access key"]);
        let gh = format!("ghp_{}", "aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3z5A");
        assert_eq!(kinds(&format!("TOKEN={gh}")), ["a GitHub token"]);
        assert_eq!(kinds(&format!("-----BEGIN OPENSSH {}-----", "PRIVATE KEY")), ["a private key"]);
        let ant = format!("sk-ant-{}", "api03-Zx9Qw8Ev7Rt6Yu5Io4Pa3Sd2Fg1Hj0Kl");
        let found = secrets(&format!("client = Anthropic(api_key=\"{ant}\")"));
        assert_eq!(found[0].1, "an Anthropic key");
        assert_eq!(found[0].2, "sk-ant…", "only the first characters are shown");
        assert!(!report(&format!("x = '{ant}'"), "a.py").unwrap().contains(&ant[8..]), "the report doesn't repeat it");
        assert_eq!(kinds("password = \"Zq8#rT2!vB9@kL4$mN7p\""), ["a credential assigned in the code"]);
        for line in ["DB_PASSWORD = \"h7GkQ2pZ9xLmW4vT8bNc\"", "aws_secret_access_key = \"wJalrXUtnFEMIK7MDENGbPxRfiCYzZ9q\"", "API_KEY=Jq8vR3mT6yW1zX9kL2pQ5sD7", "export GITHUB_TOKEN=Zq7vT2mR8yW4xK9pL3nQ6sD1"] {
            assert_eq!(secrets(line).len(), 1, "{line}");
        }
        for code in ["token = tokenizer.encode(text)", "password = os.environ[\"DB_PASSWORD\"]", "API_KEY = settings.API_KEY_VALUE", "TOKEN=${GITHUB_TOKEN_FROM_CI}"] {
            assert!(secrets(code).is_empty(), "{code}");
        }
    }

    #[test]
    fn placeholders_and_environment_reads_pass() {
        assert!(kinds("api_key = os.environ[\"OPENAI_API_KEY\"]").is_empty());
        assert!(kinds("API_KEY = \"your-api-key-here-xxxx\"").is_empty());
        assert!(kinds("password = \"aaaaaaaaaaaaaaaaaaaa\"").is_empty(), "not random-looking");
        assert!(kinds("token = process.env.GITHUB_TOKEN").is_empty());
        assert!(kinds("def check_password(pw):\n    return len(pw) >= 12\n").is_empty());
    }
}
