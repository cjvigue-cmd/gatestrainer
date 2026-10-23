//! `schema`: a JSON file fits a JSON Schema — a deliberate subset
//! (draft 2020-12 meanings). Anything outside the subset (`$ref`, `oneOf`,
//! `format`, tuple `items` …) is refused up front, so a gate never passes
//! because a keyword was silently skipped.
//!
//! The validator began as the local model's draft under 13 hidden tests;
//! it didn't compile, and was finished here.

use super::{read_artifact, Args, Fail, Outcome, Run};
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::HashMap;

const ANNOTATIONS: [&str; 7] = ["$schema", "$id", "title", "description", "default", "examples", "$comment"];
const TYPES: [&str; 7] = ["null", "boolean", "object", "array", "number", "integer", "string"];

/// The schema checked once; its patterns compiled once.
struct Checked {
    patterns: HashMap<String, Regex>,
}

/// Validate `doc` against `schema`. `Err` = the schema is unsupported or
/// malformed; `Ok(errors)` = `"<json pointer>: <problem>"` per error.
pub(crate) fn validate(schema: &Value, doc: &Value) -> Result<Vec<String>, String> {
    let mut checked = Checked { patterns: HashMap::new() };
    check_schema(schema, "#", &mut checked)?;
    let mut errors = Vec::new();
    walk(schema, doc, "", &checked, &mut errors);
    Ok(errors)
}

fn check_schema(schema: &Value, at: &str, c: &mut Checked) -> Result<(), String> {
    let map = match schema {
        Value::Bool(_) => return Ok(()),
        Value::Object(m) => m,
        _ => return Err(format!("{at}: a schema is an object or a boolean")),
    };
    for (k, v) in map {
        let here = format!("{at}/{k}");
        let bad = |what: &str| Err(format!("{here}: {what}"));
        match k.as_str() {
            k if ANNOTATIONS.contains(&k) => {}
            "type" => {
                let names: Vec<&Value> = match v {
                    Value::Array(a) => a.iter().collect(),
                    one => vec![one],
                };
                for n in names {
                    if !n.as_str().is_some_and(|s| TYPES.contains(&s)) {
                        return bad(&format!("{n} is not a type ({})", TYPES.join(", ")));
                    }
                }
            }
            "properties" => match v {
                Value::Object(props) => {
                    for (name, sub) in props {
                        check_schema(sub, &format!("{here}/{name}"), c)?;
                    }
                }
                _ => return bad("must be an object of schemas"),
            },
            "required" => {
                if !v.as_array().is_some_and(|a| a.iter().all(Value::is_string)) {
                    return bad("must be an array of property names");
                }
            }
            "additionalProperties" | "items" => {
                if !(v.is_object() || v.is_boolean()) {
                    return bad("must be one schema (the tuple form isn't supported)");
                }
                check_schema(v, &here, c)?;
            }
            "enum" => {
                if !v.is_array() {
                    return bad("must be an array");
                }
            }
            "const" => {}
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => {
                if !v.is_number() {
                    return bad("must be a number");
                }
            }
            "minLength" | "maxLength" | "minItems" | "maxItems" => {
                if !v.is_u64() {
                    return bad("must be a non-negative integer");
                }
            }
            "uniqueItems" => {
                if !v.is_boolean() {
                    return bad("must be true or false");
                }
            }
            "pattern" => {
                let Some(p) = v.as_str() else { return bad("must be a string") };
                let re = Regex::new(&ecma_classes(p)).map_err(|e| format!("{here}: {e}"))?;
                c.patterns.insert(p.to_string(), re);
            }
            other => return Err(format!("{at}: unsupported keyword `{other}` (supported: type, properties, required, additionalProperties, items, enum, const, minimum, maximum, exclusiveMinimum, exclusiveMaximum, minLength, maxLength, minItems, maxItems, uniqueItems, pattern)")),
        }
    }
    Ok(())
}

/// JSON Pointer segment: `~` → `~0`, `/` → `~1`.
fn seg(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// Two JSON numbers in order, exactly: whole numbers as i128 (IDs past 2^53
/// that an f64 can't tell apart stay apart), a whole number against a float
/// by the float's integer part and then its fraction, two floats as f64.
/// A number beyond the 64-bit range is parsed as the nearest f64 (serde_json),
/// so it is compared as that.
fn order(x: &serde_json::Number, y: &serde_json::Number) -> Option<std::cmp::Ordering> {
    let whole = |n: &serde_json::Number| n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from));
    match (whole(x), whole(y)) {
        (Some(p), Some(q)) => Some(p.cmp(&q)),
        (Some(p), None) => int_vs_float(p, y.as_f64()?),
        (None, Some(q)) => int_vs_float(q, x.as_f64()?).map(std::cmp::Ordering::reverse),
        (None, None) => x.as_f64()?.partial_cmp(&y.as_f64()?),
    }
}

/// `p` against `f`, exactly (an i128 cast of a float past 2^127 would saturate).
fn int_vs_float(p: i128, f: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    const EDGE: f64 = 170_141_183_460_469_231_731_687_303_715_884_105_728.0; // 2^127
    if f.is_nan() {
        return None;
    }
    if f >= EDGE {
        return Some(Less);
    }
    if f < -EDGE {
        return Some(Greater);
    }
    Some(match p.cmp(&(f.trunc() as i128)) {
        Equal if f.fract() > 0.0 => Less,
        Equal if f.fract() < 0.0 => Greater,
        o => o,
    })
}

/// JSON equality with numbers compared by value (1 == 1.0).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => order(x, y) == Some(std::cmp::Ordering::Equal),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

fn type_ok(name: &str, doc: &Value) -> bool {
    match name {
        "null" => doc.is_null(),
        "boolean" => doc.is_boolean(),
        "object" => doc.is_object(),
        "array" => doc.is_array(),
        "number" => doc.is_number(),
        "integer" => doc.is_i64() || doc.is_u64() || doc.as_f64().is_some_and(|f| f.fract() == 0.0),
        "string" => doc.is_string(),
        _ => false,
    }
}

fn walk(schema: &Value, doc: &Value, ptr: &str, c: &Checked, errors: &mut Vec<String>) {
    let here = if ptr.is_empty() { "/" } else { ptr };
    let map = match schema {
        Value::Bool(true) => return,
        Value::Bool(false) => return errors.push(format!("{here}: no value is allowed here")),
        Value::Object(m) => m,
        _ => return,
    };
    let mut err = |msg: String| errors.push(format!("{here}: {msg}"));
    if let Some(t) = map.get("type") {
        let names: Vec<&str> = match t {
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            one => one.as_str().into_iter().collect(),
        };
        if !names.iter().any(|n| type_ok(n, doc)) {
            err(format!("expected {}, got {doc}", names.join(" or ")));
        }
    }
    if let Some(Value::Array(options)) = map.get("enum") {
        if !options.iter().any(|o| same(o, doc)) {
            err(format!("{doc} is not one of {}", Value::Array(options.clone())));
        }
    }
    if let Some(k) = map.get("const") {
        if !same(k, doc) {
            err(format!("{doc} is not {k}"));
        }
    }
    let len = |k: &str| map.get(k).and_then(Value::as_u64).map(|n| n as usize);
    if let Value::Number(v) = doc {
        use std::cmp::Ordering::{Equal, Greater, Less};
        let against = |k: &str| match map.get(k) {
            Some(Value::Number(m)) => order(v, m),
            _ => None,
        };
        if against("minimum") == Some(Less) {
            err(format!("{v} is below the minimum {}", map["minimum"]));
        }
        if against("maximum") == Some(Greater) {
            err(format!("{v} is above the maximum {}", map["maximum"]));
        }
        if matches!(against("exclusiveMinimum"), Some(Less | Equal)) {
            err(format!("{v} must be above {}", map["exclusiveMinimum"]));
        }
        if matches!(against("exclusiveMaximum"), Some(Greater | Equal)) {
            err(format!("{v} must be below {}", map["exclusiveMaximum"]));
        }
    }
    if let Some(s) = doc.as_str() {
        let n = s.chars().count();
        if len("minLength").is_some_and(|m| n < m) {
            err(format!("{n} characters; at least {} required", map["minLength"]));
        }
        if len("maxLength").is_some_and(|m| n > m) {
            err(format!("{n} characters; at most {} allowed", map["maxLength"]));
        }
        if let Some(p) = map.get("pattern").and_then(Value::as_str) {
            match c.patterns.get(p) {
                Some(re) if re.is_match(s) => {}
                Some(_) => err(format!("{s:?} doesn't match {p:?}")),
                None => err(format!("pattern {p:?} wasn't compiled")),
            }
        }
    }
    if let Some(items) = doc.as_array() {
        if len("minItems").is_some_and(|m| items.len() < m) {
            err(format!("{} items; at least {} required", items.len(), map["minItems"]));
        }
        if len("maxItems").is_some_and(|m| items.len() > m) {
            err(format!("{} items; at most {} allowed", items.len(), map["maxItems"]));
        }
        if map.get("uniqueItems") == Some(&Value::Bool(true)) {
            let dup = (0..items.len()).find_map(|i| (i + 1..items.len()).find(|&j| same(&items[i], &items[j])).map(|j| (i, j)));
            if let Some((i, j)) = dup {
                err(format!("items {i} and {j} are equal"));
            }
        }
        if let Some(sub) = map.get("items") {
            for (i, v) in items.iter().enumerate() {
                walk(sub, v, &format!("{ptr}/{i}"), c, errors);
            }
        }
    }
    if let Some(obj) = doc.as_object() {
        object(map, obj, ptr, here, c, errors);
    }
}

fn object(map: &Map<String, Value>, obj: &Map<String, Value>, ptr: &str, here: &str, c: &Checked, errors: &mut Vec<String>) {
    let props = map.get("properties").and_then(Value::as_object);
    for name in map.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        if !obj.contains_key(name) {
            errors.push(format!("{here}: required property {name:?} is missing"));
        }
    }
    for (k, v) in obj {
        let at = format!("{ptr}/{}", seg(k));
        match (props.and_then(|p| p.get(k)), map.get("additionalProperties")) {
            (Some(sub), _) => walk(sub, v, &at, c, errors),
            (None, Some(Value::Bool(false))) => errors.push(format!("{at}: property {k:?} is not allowed")),
            (None, Some(sub)) => walk(sub, v, &at, c, errors),
            (None, None) => {}
        }
    }
}

/// JSON Schema patterns are ECMA-262 regular expressions, where `\d` and
/// `\w` are ASCII only; the regex crate's are Unicode (`\d` matches "١٢٣").
/// Written as the ASCII classes they mean, inside a class too (`[\d.]` →
/// `[[0-9].]`, a nested class the regex crate reads).
fn ecma_classes(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('d') => out.push_str("[0-9]"),
            Some('D') => out.push_str("[^0-9]"),
            Some('w') => out.push_str("[0-9A-Za-z_]"),
            Some('W') => out.push_str("[^0-9A-Za-z_]"),
            Some(n) => {
                out.push('\\');
                out.push(n);
            }
            None => out.push('\\'),
        }
    }
    out
}

pub(crate) fn run(args: &[String]) -> Run {
    let a = Args::parse(args, &[])?;
    a.only(&["schema"])?;
    let schema_path = a.need("schema")?;
    let [file] = a.positional.as_slice() else {
        return Err("give exactly one JSON file".into());
    };
    let schema_text = std::fs::read_to_string(schema_path).map_err(|e| format!("--schema {schema_path}: {e}"))?;
    let schema: Value = serde_json::from_str(&schema_text).map_err(|e| format!("--schema {schema_path}: not JSON: {e}"))?;
    let text = match read_artifact(file) {
        Ok(t) => t,
        Err(f) => return Ok(Outcome::Fail(f)),
    };
    let doc: Value = match serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text)) {
        Ok(d) => d,
        Err(e) => return Ok(Outcome::Fail(Fail(vec![format!("{file}: not valid JSON: {e}")]))),
    };
    let errors = validate(&schema, &doc).map_err(|e| format!("--schema {schema_path}: {e}"))?;
    if errors.is_empty() {
        Ok(Outcome::Pass(format!("{file} fits {schema_path}")))
    } else {
        Ok(Outcome::Fail(Fail(errors)))
    }
}

#[cfg(test)]
mod tests {
    //! The hidden tests the local model's draft was gated on.
    use super::*;
    use serde_json::json;

    fn errs(s: Value, d: Value) -> Vec<String> {
        validate(&s, &d).expect("schema should be accepted")
    }
    fn has(errors: &[String], prefix: &str) -> bool {
        errors.iter().any(|e| e.starts_with(prefix))
    }

    #[test]
    fn boolean_schemas() {
        assert!(errs(json!(true), json!({"a": 1})).is_empty());
        assert_eq!(errs(json!(false), json!(1)).len(), 1);
        assert!(errs(json!({}), json!([1, "x"])).is_empty());
    }
    #[test]
    fn types() {
        assert!(errs(json!({"type": "string"}), json!("x")).is_empty());
        assert_eq!(errs(json!({"type": "string"}), json!(3)).len(), 1);
        assert!(errs(json!({"type": "integer"}), json!(3)).is_empty());
        assert!(errs(json!({"type": "integer"}), json!(3.0)).is_empty(), "3.0 is an integer");
        assert_eq!(errs(json!({"type": "integer"}), json!(3.5)).len(), 1);
        assert!(errs(json!({"type": "number"}), json!(3)).is_empty());
        assert!(errs(json!({"type": ["null", "boolean"]}), json!(null)).is_empty());
        assert!(errs(json!({"type": ["null", "boolean"]}), json!(false)).is_empty());
        assert_eq!(errs(json!({"type": ["null", "boolean"]}), json!("no")).len(), 1);
        assert!(errs(json!({"type": "object"}), json!({})).is_empty());
        assert_eq!(errs(json!({"type": "array"}), json!({})).len(), 1);
    }
    #[test]
    fn root_pointer_is_slash() {
        let e = errs(json!({"type": "string"}), json!(1));
        assert!(e[0].starts_with("/:"), "{e:?}");
    }
    #[test]
    fn object_keywords_and_pointers() {
        let s = json!({
            "type": "object",
            "required": ["id", "tags"],
            "properties": {
                "id": {"type": "integer", "minimum": 1},
                "tags": {"type": "array", "items": {"type": "string", "minLength": 2}, "maxItems": 3}
            },
            "additionalProperties": false
        });
        assert!(errs(s.clone(), json!({"id": 5, "tags": ["ab", "cd"]})).is_empty());
        let e = errs(s.clone(), json!({"id": 0, "tags": ["ab", "c"], "extra": 1}));
        assert_eq!(e.len(), 3, "{e:?}");
        assert!(has(&e, "/id:") && has(&e, "/tags/1:") && has(&e, "/extra:"), "{e:?}");
        let e = errs(s.clone(), json!({"id": 2}));
        assert_eq!(e.len(), 1, "{e:?}");
        assert!(e[0].contains("tags"), "{e:?}");
        let e = errs(s, json!({"id": 2, "tags": ["aa", "bb", "cc", "dd"]}));
        assert!(has(&e, "/tags:"), "{e:?}");
    }
    #[test]
    fn additional_properties_schema() {
        let s = json!({"properties": {"a": {}}, "additionalProperties": {"type": "number"}});
        assert!(errs(s.clone(), json!({"a": "x", "b": 2})).is_empty());
        assert!(has(&errs(s, json!({"a": "x", "b": "y"})), "/b:"));
    }
    #[test]
    fn pointer_escapes() {
        let s = json!({"properties": {"a/b": {"type": "string"}, "c~d": {"type": "string"}}});
        let e = errs(s, json!({"a/b": 1, "c~d": 1}));
        assert!(has(&e, "/a~1b:") && has(&e, "/c~0d:"), "{e:?}");
    }
    #[test]
    fn numbers() {
        let s = json!({"minimum": 1, "maximum": 10, "exclusiveMinimum": 0, "exclusiveMaximum": 10});
        assert!(errs(s.clone(), json!(5)).is_empty());
        assert!(!errs(s.clone(), json!(10)).is_empty());
        assert!(!errs(s, json!(0.5)).is_empty());
        assert!(errs(json!({"minimum": 1}), json!("not a number")).is_empty());
        // Past 2^53 an f64 can't tell 2^53 from 2^53 + 1; whole numbers compare exactly.
        let big = 9_007_199_254_740_993_u64;
        assert!(!errs(json!({"maximum": big - 1}), json!(big)).is_empty(), "over the maximum by one");
        assert!(!errs(json!({"exclusiveMaximum": big}), json!(big)).is_empty());
        assert!(errs(json!({"minimum": big}), json!(big)).is_empty());
        assert!(!errs(json!({"const": big - 1}), json!(big)).is_empty(), "const tells them apart");
        assert!(errs(json!({"enum": [1.0]}), json!(1)).is_empty(), "1 == 1.0 still");
        assert!(!errs(json!({"minimum": 0}), json!(-1)).is_empty());
        assert!(errs(json!({"maximum": u64::MAX}), json!(-5)).is_empty(), "i64 against u64");
        // A whole number against a float bound, and the reverse (the reviewer's cases).
        let two53 = 9_007_199_254_740_992_u64;
        assert!(!errs(json!({"maximum": two53 as f64}), json!(two53 + 1)).is_empty());
        assert!(!errs(json!({"const": two53 as f64}), json!(two53 + 1)).is_empty());
        assert!(errs(json!({"exclusiveMaximum": two53 + 1}), json!(two53 as f64)).is_empty(), "below the bound");
        assert!(!errs(json!({"maximum": 2.5}), json!(3)).is_empty() && errs(json!({"maximum": 2.5}), json!(2)).is_empty());
        assert!(!errs(json!({"minimum": -2.5}), json!(-3)).is_empty() && errs(json!({"minimum": -2.5}), json!(-2)).is_empty());
        assert!(!errs(json!({"maximum": u64::MAX}), json!(1e20)).is_empty(), "a float past u64");
        assert!(errs(json!({"maximum": 1e300}), json!(u64::MAX)).is_empty() && !errs(json!({"minimum": 1e300}), json!(u64::MAX)).is_empty());
    }
    #[test]
    fn strings() {
        let s = json!({"minLength": 2, "maxLength": 3, "pattern": "^[a-z]+$"});
        assert!(errs(s.clone(), json!("abc")).is_empty());
        assert!(errs(json!({"minLength": 2, "maxLength": 3}), json!("日本")).is_empty(), "characters, not bytes");
        assert_eq!(errs(s.clone(), json!("abcd")).len(), 1);
        assert_eq!(errs(s, json!("ABC")).len(), 1);
        assert!(errs(json!({"pattern": "b"}), json!("abc")).is_empty(), "a search, not anchored");
        assert_eq!(errs(json!({"pattern": "^\\d+$"}), json!("\u{0661}\u{0662}\u{0663}")).len(), 1, "\\d is ASCII digits, as in ECMA-262");
        assert!(errs(json!({"pattern": "^[\\d.]+$"}), json!("1.25")).is_empty() && errs(json!({"pattern": "^\\w+\\\\d$"}), json!("a_1\\d")).is_empty(), "inside a class, and an escaped backslash");
    }
    #[test]
    fn arrays() {
        let s = json!({"minItems": 1, "uniqueItems": true, "items": {"type": "integer"}});
        assert!(errs(s.clone(), json!([1, 2])).is_empty());
        assert!(!errs(s.clone(), json!([])).is_empty());
        assert_eq!(errs(s.clone(), json!([1, 1, 1])).len(), 1, "one error, not one per pair");
        assert!(!errs(s.clone(), json!([1, 1.0])).is_empty(), "1 and 1.0 are equal");
        assert!(has(&errs(s, json!([1, "x", 3])), "/1:"));
    }
    #[test]
    fn enum_and_const() {
        assert!(errs(json!({"enum": ["a", 1, null]}), json!(null)).is_empty());
        assert!(errs(json!({"enum": [1]}), json!(1.0)).is_empty());
        assert_eq!(errs(json!({"enum": ["a"]}), json!("b")).len(), 1);
        assert!(errs(json!({"const": {"x": [1, 2]}}), json!({"x": [1, 2]})).is_empty());
        assert_eq!(errs(json!({"const": {"x": [1, 2]}}), json!({"x": [2, 1]})).len(), 1);
        assert!(!same(&json!(u64::MAX), &json!(-1)), "large unsigned isn't confused with negatives");
    }
    #[test]
    fn annotations_are_ignored() {
        let s = json!({"$schema": "x", "$id": "y", "title": "t", "description": "d", "default": 1, "examples": [1], "$comment": "c", "type": "integer"});
        assert!(errs(s, json!(1)).is_empty());
    }
    #[test]
    fn unsupported_keywords_are_refused_anywhere() {
        assert!(validate(&json!({"$ref": "#/defs/a"}), &json!(1)).is_err_and(|e| e.contains("$ref")));
        assert!(validate(&json!({"properties": {"x": {"oneOf": []}}}), &json!({})).is_err_and(|e| e.contains("oneOf")));
        assert!(validate(&json!({"items": {"format": "email"}}), &json!([])).is_err_and(|e| e.contains("format")));
    }
    #[test]
    fn malformed_schemas_are_refused() {
        assert!(validate(&json!({"type": "str"}), &json!("x")).is_err());
        assert!(validate(&json!({"required": "id"}), &json!({})).is_err());
        assert!(validate(&json!({"minLength": -1}), &json!("")).is_err());
        assert!(validate(&json!({"pattern": "("}), &json!("")).is_err());
        assert!(validate(&json!({"items": [{}]}), &json!([])).is_err());
        assert!(validate(&json!(3), &json!(1)).is_err());
    }
}
