//! Small helpers: names, times and the output folder.

use crate::config::{OUT_DIR_ENV, RECORDS_DIR_ENV};
use crate::error::{Error, Result};
use std::path::PathBuf;

/// A short folder name from a label: the first five words, lowercase,
/// letters, digits and hyphens only, at most 40 characters.
pub fn slugify(prompt: &str) -> String {
    let lowered = prompt.to_lowercase();
    let words: Vec<&str> = lowered
        .split_whitespace()
        .take(5)
        .collect();
    let mut slug: String = words.join("-");
    // Strip anything that isn't a lowercase letter, digit, or hyphen.
    slug = slug
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    // Collapse runs of hyphens.
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug = slug.trim_matches('-').to_string();
    // Truncate to 40 chars on a word boundary.
    if slug.len() > 40 {
        slug = slug[..40].trim_end_matches('-').to_string();
    }
    // 3-char hash suffix for collision resistance.
    let hash = hash_str(prompt);
    let suffix = format!("{:03x}", hash % 0x1000);
    format!("{slug}-{suffix}")
}

/// FNV-1a, for short stable suffixes.
fn hash_str(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

/// Now, as `YYYY-MM-DDTHH:MM:SSZ` (UTC), with no date library.
pub fn chrono_now() -> String {
    // Format: YYYY-MM-DDTHH:MM:SSZ. Uses SystemTime + a tiny civil-date
    // conversion so we don't need a date library in the binary.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = epoch_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Convert unix seconds to (year, month, day, hour, minute, second) UTC.
/// Howard Hinnant's days-from-civil algorithm — public domain.
fn epoch_to_ymdhms(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y_final = if m <= 2 { y + 1 } else { y };
    let h = sod / 3600;
    let mi = (sod % 3600) / 60;
    let s = sod % 60;
    (y_final, m, d, h, mi, s)
}

/// Where validated artifacts are kept: `GATESTRAINER_OUT`, else the
/// temp dir's `gatestrainer/output`. Created if missing.
pub fn output_dir() -> Result<PathBuf> {
    let dir = std::env::var_os(OUT_DIR_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gatestrainer").join("output"));
    std::fs::create_dir_all(&dir).map_err(|e| Error::SandboxError(format!("mkdir {}: {e}", dir.display())))?;
    Ok(dir)
}

/// Where records that outlive a run are kept (the plant record):
/// `GATESTRAINER_RECORDS`, else the temp dir's `gatestrainer`.
pub fn records_root() -> PathBuf {
    std::env::var_os(RECORDS_DIR_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gatestrainer"))
}
