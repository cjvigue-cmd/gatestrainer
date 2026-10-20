//! `mesh`: an STL file (ASCII or binary) is a closed solid of the right
//! size — its bounding box within `--tol` millimetres of `--size X,Y,Z`
//! (in that axis order, or in any order with `--orientation any`) and,
//! with `--volume`, its enclosed volume within `--vol-tol` (relative) of
//! the caller's value. Closed means every edge is shared by exactly two
//! triangles: a mesh with a gap or a dangling face isn't a solid, so its
//! volume means nothing and a slicer can't print it. The bounding box
//! and the volume are measured, not read from the model's source, so they
//! don't depend on how the task was understood.

use super::{Args, Fail, Outcome, Run};
use std::collections::HashMap;

type Tri = [[f64; 3]; 3];

/// A vertex snapped to a grid, and an edge as its two ends (lower first).
type Snapped = (i64, i64, i64);

/// The triangles of an STL: binary when the size matches the count in
/// its header, ASCII otherwise.
fn triangles(bytes: &[u8]) -> Result<Vec<Tri>, String> {
    if bytes.len() >= 84 {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        if n.checked_mul(50).and_then(|b| b.checked_add(84)) == Some(bytes.len()) {
            let f = |at: usize| f64::from(f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]));
            return Ok((0..n)
                .map(|i| {
                    let base = 84 + i * 50 + 12; // skip the normal
                    let v = |k: usize| [f(base + k * 12), f(base + k * 12 + 4), f(base + k * 12 + 8)];
                    [v(0), v(1), v(2)]
                })
                .collect());
        }
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "neither a binary STL (its size doesn't match its triangle count) nor ASCII STL".to_string())?;
    if !text.trim_start().starts_with("solid") {
        return Err("not an STL: no `solid` line and not binary".into());
    }
    let mut out = Vec::new();
    let mut tri: Vec<[f64; 3]> = Vec::with_capacity(3);
    for (i, line) in text.lines().enumerate() {
        let mut w = line.split_whitespace();
        match w.next() {
            Some("vertex") => {
                let xyz: Vec<f64> = w.map(str::parse).collect::<Result<_, _>>().map_err(|_| format!("line {}: a vertex that isn't three numbers", i + 1))?;
                let [x, y, z] = xyz[..] else { return Err(format!("line {}: a vertex that isn't three numbers", i + 1)) };
                tri.push([x, y, z]);
            }
            Some("endloop") => {
                let [a, b, c] = tri[..] else { return Err(format!("line {}: a facet with {} vertices", i + 1, tri.len())) };
                out.push([a, b, c]);
                tri.clear();
            }
            _ => {}
        }
    }
    Ok(out)
}

/// (min, max) per axis.
fn bounds(tris: &[Tri]) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for v in tris.iter().flatten() {
        for k in 0..3 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    (lo, hi)
}

/// Enclosed volume (divergence theorem); meaningful only for a closed mesh.
fn volume(tris: &[Tri]) -> f64 {
    let six: f64 = tris
        .iter()
        .map(|[a, b, c]| a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0]))
        .sum();
    (six / 6.0).abs()
}

/// Edges not shared by exactly two triangles (vertices matched to 1e-5 of
/// the part's size, since STL stores 32-bit floats).
fn open_edges(tris: &[Tri], scale: f64) -> usize {
    let q = |v: &[f64; 3]| {
        let step = (scale * 1e-5).max(1e-9);
        // Rounded to a grid: the casts can't overflow for any real part.
        #[allow(clippy::cast_possible_truncation)]
        let k = |x: f64| (x / step).round() as i64;
        (k(v[0]), k(v[1]), k(v[2]))
    };
    let mut edges: HashMap<(Snapped, Snapped), u32> = HashMap::new();
    for t in tris {
        let p = [q(&t[0]), q(&t[1]), q(&t[2])];
        if p[0] == p[1] || p[1] == p[2] || p[0] == p[2] {
            continue; // a degenerate sliver adds no edge
        }
        for (a, b) in [(p[0], p[1]), (p[1], p[2]), (p[2], p[0])] {
            *edges.entry(if a < b { (a, b) } else { (b, a) }).or_default() += 1;
        }
    }
    edges.values().filter(|&&n| n != 2).count()
}

fn mm(x: f64) -> String {
    let s = format!("{x:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["file", "size", "tol", "orientation", "volume", "vol-tol"])?;
    if let Some(p) = a.positional.first() {
        return Err(format!("unexpected argument {p:?}"));
    }
    let want: Vec<f64> = a.need("size")?.split(',').map(|s| s.trim().parse::<f64>()).collect::<Result<_, _>>().map_err(|_| "--size: three numbers, X,Y,Z in mm".to_string())?;
    let [wx, wy, wz] = want[..] else { return Err("--size: three numbers, X,Y,Z in mm".into()) };
    if [wx, wy, wz].iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err("--size: every side must be a positive number".into());
    }
    let tol: f64 = a.number("tol")?.unwrap_or(0.05);
    let any_order = match a.one("orientation")?.unwrap_or("fixed") {
        "fixed" => false,
        "any" => true,
        other => return Err(format!("--orientation {other:?}: fixed or any")),
    };
    // An empty --volume (a blueprint's default) means: not checked.
    let want_volume = match a.one("volume")?.map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(s.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0).ok_or("--volume: a positive number of mm³")?),
    };
    let vol_tol: f64 = a.number("vol-tol")?.unwrap_or(0.01);
    let file = a.need("file")?;
    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) => return Ok(Outcome::Fail(Fail(vec![format!("{file}: can't be read ({e})")]))),
    };
    let tris = match triangles(&bytes) {
        Ok(t) if t.is_empty() => return Ok(Outcome::Fail(Fail(vec![format!("{file}: no triangles — the model is empty")]))),
        Ok(t) => t,
        Err(why) => return Ok(Outcome::Fail(Fail(vec![format!("{file}: {why}")]))),
    };
    let (lo, hi) = bounds(&tris);
    let mut got = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let mut want = [wx, wy, wz];
    if any_order {
        got.sort_by(f64::total_cmp);
        want.sort_by(f64::total_cmp);
    }
    let mut problems = Vec::new();
    let axes = if any_order { ["smallest", "middle", "largest"] } else { ["X", "Y", "Z"] };
    for k in 0..3 {
        if (got[k] - want[k]).abs() > tol {
            problems.push(format!("{} size {} mm, want {} ± {} mm", axes[k], mm(got[k]), mm(want[k]), mm(tol)));
        }
    }
    let shown = |g: [f64; 3]| format!("{} × {} × {} mm", mm(g[0]), mm(g[1]), mm(g[2]));
    let scale = got.iter().copied().fold(0.0, f64::max);
    let open = open_edges(&tris, scale);
    if open > 0 {
        problems.push(format!("not a closed solid: {open} edge(s) aren't shared by exactly two faces (a gap, a dangling face, or two bodies touching along an edge)"));
    }
    let v = volume(&tris);
    if let Some(wv) = want_volume {
        if open == 0 && ((v - wv) / wv).abs() > vol_tol {
            problems.push(format!("volume {} mm³, want {} mm³ ± {}% ({}% off): a feature missing or extra, or a hole not through", mm(v), mm(wv), mm(vol_tol * 100.0), mm((v - wv) / wv * 100.0)));
        }
    }
    let size = shown([hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]);
    if problems.is_empty() {
        return Ok(Outcome::Pass(format!("{size}, closed, {} mm³, {} triangles", mm(v), tris.len())));
    }
    let n = problems.len();
    problems[0] = format!("{} (the part measures {size})", problems[0]);
    if n > 1 {
        problems[0] = format!("{} — {n} problems", problems[0]);
    }
    Ok(Outcome::Fail(Fail(problems)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box from (0,0,0) to (x,y,z) as 12 triangles, outward-facing.
    fn cuboid(x: f64, y: f64, z: f64) -> Vec<Tri> {
        let p = |i: u8| [if i & 1 == 1 { x } else { 0.0 }, if i & 2 == 2 { y } else { 0.0 }, if i & 4 == 4 { z } else { 0.0 }];
        let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        quads.iter().flat_map(|q| [[p(q[0]), p(q[1]), p(q[2])], [p(q[0]), p(q[2]), p(q[3])]]).collect()
    }

    fn ascii(tris: &[Tri]) -> String {
        let mut s = String::from("solid t\n");
        for t in tris {
            s.push_str("  facet normal 0 0 0\n    outer loop\n");
            for v in t {
                s.push_str(&format!("      vertex {} {} {}\n", v[0], v[1], v[2]));
            }
            s.push_str("    endloop\n  endfacet\n");
        }
        s + "endsolid t\n"
    }

    fn binary(tris: &[Tri]) -> Vec<u8> {
        let mut b = vec![0u8; 80];
        b.extend(u32::try_from(tris.len()).expect("small").to_le_bytes());
        for t in tris {
            b.extend([0u8; 12]);
            for v in t {
                for c in v {
                    #[allow(clippy::cast_possible_truncation)]
                    b.extend((*c as f32).to_le_bytes());
                }
            }
            b.extend([0u8; 2]);
        }
        b
    }

    #[test]
    fn both_encodings_measure_the_same() {
        let c = cuboid(60.0, 40.0, 4.0);
        for t in [triangles(ascii(&c).as_bytes()).expect("ascii"), triangles(&binary(&c)).expect("binary")] {
            assert_eq!(t.len(), 12);
            let (lo, hi) = bounds(&t);
            assert_eq!((lo, hi), ([0.0; 3], [60.0, 40.0, 4.0]));
            assert!((volume(&t) - 9600.0).abs() < 1e-6);
            assert_eq!(open_edges(&t, 60.0), 0);
        }
    }

    #[test]
    fn a_gap_or_a_bad_file_is_named() {
        let mut c = cuboid(10.0, 10.0, 10.0);
        c.pop();
        assert_eq!(open_edges(&c, 10.0), 3, "a missing face leaves its three edges single");
        assert!(triangles(b"hello").unwrap_err().contains("not an STL"));
        assert!(triangles(b"solid x\nfacet\nouter loop\nvertex 1 2\nendloop\n").unwrap_err().contains("three numbers"));
    }

    #[test]
    fn cli_sizes_orientation_volume() {
        let d = tempfile::tempdir().expect("tmp");
        let f = d.path().join("m.stl");
        std::fs::write(&f, binary(&cuboid(60.0, 40.0, 4.0))).expect("write");
        let file = f.to_string_lossy().into_owned();
        let run_ = |extra: &[&str]| {
            let mut v = vec!["--file".to_string(), file.clone()];
            v.extend(extra.iter().map(|s| s.to_string()));
            run(&v)
        };
        let fails = |r: Run| match r {
            Ok(Outcome::Fail(Fail(l))) => l,
            Ok(Outcome::Pass(p)) => panic!("passed: {p}"),
            Err(e) => panic!("misconfigured: {e}"),
        };
        assert!(matches!(run_(&["--size", "60,40,4"]), Ok(Outcome::Pass(_))));
        assert!(matches!(run_(&["--size", "60,40,4", "--volume", "", "--vol-tol", "0.01"]), Ok(Outcome::Pass(_))), "an empty --volume isn't checked");
        assert!(fails(run_(&["--size", "40,60,4"]))[0].starts_with("X size 60 mm, want 40 ± 0.05 mm (the part measures 60 × 40 × 4 mm) — 2 problems"));
        assert!(matches!(run_(&["--size", "4,60,40", "--orientation", "any"]), Ok(Outcome::Pass(_))));
        assert!(fails(run_(&["--size", "60,40,4", "--volume", "8214.7"]))[0].contains("16.86"));
        assert!(matches!(run_(&["--size", "60,40,4", "--volume", "9650", "--vol-tol", "0.01"]), Ok(Outcome::Pass(_))));
        assert!(run_(&["--size", "60,40"]).is_err());
        assert!(run_(&["--size", "60,40,4", "--orientation", "up"]).is_err());
        std::fs::write(&f, "solid empty\nendsolid empty\n").expect("write");
        assert!(fails(run_(&["--size", "1,1,1"]))[0].contains("no triangles"));
    }
}
