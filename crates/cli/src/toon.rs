//! TOON, the compact output form the axi tools print for agents
//! (`protocol.md` section 6.1). The same serde value the `--json` contract
//! prints is encoded here, so the two forms cannot drift.
//!
//! Objects are `key: value` lines indented two spaces per level. An array of
//! scalars is one line, `key[3]: a,b,c`. An array of objects that all have the
//! same scalar fields is a table, `key[2]{id,name}:` with one row per object.
//! Anything else is a list of `- ` items.

use serde_json::{Map, Value};

/// A response: `ok` first and `data` before the bookkeeping, then the rest.
pub fn encode_response(value: &Value) -> String {
    const ORDER: [&str; 8] = [
        "ok", "error", "data", "events", "replayed", "key", "warning", "help",
    ];
    let Value::Object(map) = value else {
        return encode(value);
    };
    let mut out = String::new();
    for name in ORDER {
        if let Some(item) = map.get(name) {
            field(&mut out, 0, name, item);
        }
    }
    for (name, item) in map {
        if !ORDER.contains(&name.as_str()) {
            field(&mut out, 0, name, item);
        }
    }
    out
}

pub fn encode(value: &Value) -> String {
    let mut out = String::new();
    match value {
        Value::Object(map) => object(&mut out, map, 0),
        Value::Array(items) => array(&mut out, 0, "", items),
        scalar_value => {
            out.push_str(&scalar(scalar_value));
            out.push('\n');
        }
    }
    out
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn object(out: &mut String, map: &Map<String, Value>, depth: usize) {
    for (key, value) in map {
        field(out, depth, key, value);
    }
}

fn field(out: &mut String, depth: usize, key: &str, value: &Value) {
    match value {
        Value::Object(map) if map.is_empty() => {
            indent(out, depth);
            out.push_str(&format!("{}: {{}}\n", key_text(key)));
        }
        Value::Object(map) => {
            indent(out, depth);
            out.push_str(&format!("{}:\n", key_text(key)));
            object(out, map, depth + 1);
        }
        Value::Array(items) => array(out, depth, &key_text(key), items),
        other => {
            indent(out, depth);
            out.push_str(&format!("{}: {}\n", key_text(key), scalar(other)));
        }
    }
}

fn array(out: &mut String, depth: usize, key: &str, items: &[Value]) {
    indent(out, depth);
    let n = items.len();
    if items.iter().all(is_scalar) {
        let joined: Vec<String> = items.iter().map(scalar).collect();
        if n == 0 {
            out.push_str(&format!("{key}[0]:\n"));
        } else {
            out.push_str(&format!("{key}[{n}]: {}\n", joined.join(",")));
        }
        return;
    }
    if let Some(columns) = table_columns(items) {
        let header: Vec<String> = columns.iter().map(|c| key_text(c)).collect();
        out.push_str(&format!("{key}[{n}]{{{}}}:\n", header.join(",")));
        for item in items {
            indent(out, depth + 1);
            let cells: Vec<String> = columns
                .iter()
                .map(|column| scalar(item.get(column).unwrap_or(&Value::Null)))
                .collect();
            out.push_str(&cells.join(","));
            out.push('\n');
        }
        return;
    }
    out.push_str(&format!("{key}[{n}]:\n"));
    for item in items {
        list_item(out, depth + 1, item);
    }
}

/// One `- ` item. An object's first field shares the hyphen's line.
fn list_item(out: &mut String, depth: usize, item: &Value) {
    match item {
        Value::Object(map) if !map.is_empty() => {
            let mut inner = String::new();
            object(&mut inner, map, depth + 1);
            let lead = "  ".repeat(depth + 1);
            let rest = inner.strip_prefix(&lead).unwrap_or(&inner);
            indent(out, depth);
            out.push_str("- ");
            out.push_str(rest);
        }
        Value::Array(items) => {
            let mut inner = String::new();
            array(&mut inner, 0, "", items);
            indent(out, depth);
            out.push_str("- ");
            out.push_str(&inner);
        }
        other => {
            indent(out, depth);
            out.push_str(&format!("- {}\n", scalar(other)));
        }
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

/// The shared column names when every item is an object of scalars with the
/// same keys in the same order.
fn table_columns(items: &[Value]) -> Option<Vec<String>> {
    let first = items.first()?.as_object()?;
    if first.is_empty() {
        return None;
    }
    let columns: Vec<String> = first.keys().cloned().collect();
    for item in items {
        let map = item.as_object()?;
        if map.len() != columns.len() || !map.keys().eq(columns.iter()) {
            return None;
        }
        if !map.values().all(is_scalar) {
            return None;
        }
    }
    Some(columns)
}

fn key_text(key: &str) -> String {
    let plain = !key.is_empty()
        && key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if plain { key.to_owned() } else { quote(key) }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => string(s),
        // Callers only pass scalars; nested values are encoded as JSON text.
        other => quote(&other.to_string()),
    }
}

fn string(text: &str) -> String {
    if needs_quotes(text) {
        quote(text)
    } else {
        text.to_owned()
    }
}

fn needs_quotes(text: &str) -> bool {
    if text.is_empty() || text != text.trim() {
        return true;
    }
    if matches!(text, "true" | "false" | "null") || looks_numeric(text) {
        return true;
    }
    if text.starts_with("- ") || text == "-" {
        return true;
    }
    text.chars()
        .any(|c| matches!(c, ':' | '"' | '\\' | '[' | ']' | '{' | '}' | ',') || c.is_control())
}

fn looks_numeric(text: &str) -> bool {
    let body = text.strip_prefix('-').unwrap_or(text);
    let mut chars = body.chars().peekable();
    if !chars.peek().is_some_and(char::is_ascii_digit) {
        return false;
    }
    // Leading zeros ("007") are quoted too, so they survive a round trip.
    body.chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn objects_scalars_and_nesting() {
        let text = encode(&json!({
            "ok": true,
            "data": {"id": "alpha-1", "rev": 3, "note": null, "empty": {}},
        }));
        assert_eq!(
            text,
            "data:\n  empty: {}\n  id: alpha-1\n  note: null\n  rev: 3\nok: true\n"
        );
    }

    #[test]
    fn scalar_arrays_are_one_line_and_uniform_objects_a_table() {
        let text = encode(&json!({
            "events": [4, 5],
            "none": [],
            "rows": [{"id": "a", "n": 1}, {"id": "b", "n": 2}],
        }));
        assert_eq!(
            text,
            "events[2]: 4,5\nnone[0]:\nrows[2]{id,n}:\n  a,1\n  b,2\n"
        );
    }

    #[test]
    fn mixed_arrays_are_hyphen_lists() {
        let text = encode(&json!({"items": [{"a": 1, "b": {"c": 2}}, "x"]}));
        assert_eq!(text, "items[2]:\n  - a: 1\n    b:\n      c: 2\n  - x\n");
    }

    #[test]
    fn strings_that_could_be_read_as_something_else_are_quoted() {
        let text = encode(&json!({
            "a": "true", "b": "12", "c": "", "d": "x: y", "e": " pad", "f": "line\nbreak",
            "g": "plain text", "h": "007", "i": "a,b",
        }));
        assert_eq!(
            text,
            "a: \"true\"\nb: \"12\"\nc: \"\"\nd: \"x: y\"\ne: \" pad\"\nf: \"line\\nbreak\"\ng: plain text\nh: \"007\"\ni: \"a,b\"\n"
        );
    }
}
