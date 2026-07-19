//! RustJSON — a lenient, human-authorable JSON superset designed for this
//! ecosystem (concept: user; grammar design and this
//! implementation: Claude, 2026-07-14).
//!
//! **Value model**: RustJSON does not introduce a new data model — the parsed
//! result is a plain [`serde_json::Value`], the same type every other
//! JSON-consuming piece of this codebase already uses. RustJSON's entire
//! contribution is a *more lenient input grammar*: it accepts several
//! common human-authoring conveniences that strict JSON (RFC 8259)
//! rejects, then normalizes down to the exact same value tree strict JSON
//! would produce. This keeps RustJSON strictly additive — anything that reads
//! `serde_json::Value` (this entire workspace) can consume RustJSON-derived
//! data with zero changes, and every valid strict-JSON document is also a
//! valid RustJSON document (RustJSON's grammar is a superset).
//!
//! ## Grammar extensions over strict JSON
//!
//! 1. **Trailing commas** — `[1, 2, 3,]` and `{"a": 1,}` are accepted.
//! 2. **Comments** — `//` line comments and `/* ... */` block comments,
//!    anywhere whitespace is allowed.
//! 3. **Unquoted object keys** — `{name: "sword"}` is accepted wherever
//!    the key is a valid identifier (`[A-Za-z_][A-Za-z0-9_]*`); keys that
//!    aren't valid identifiers (spaces, leading digits, etc.) still
//!    require quotes.
//! 4. **Single-quoted strings** — `'hello'` is accepted anywhere a
//!    double-quoted string is, with the same escape sequences.
//!
//! Deliberately *not* included (kept for a possible future revision, not
//! because they're hard, but to keep this first version's semantics
//! unambiguous and its parser small): hex/octal number literals,
//! multi-line strings, `NaN`/`Infinity` numeric literals, YAML-style
//! anchors/references. Every one of the four extensions above normalizes
//! to *exactly* what strict JSON would represent — there is no new
//! semantic value RustJSON can express that JSON cannot; the only thing that
//! changes is how forgiving the parser is about the source text.
//!
//! ## Design lineage
//!
//! The extension set (trailing commas, comments, unquoted keys) mirrors
//! [JSON5](https://json5.org/) and [JSONC](https://code.visualstudio.com/docs/languages/json#_json-with-comments),
//! two established, widely-implemented conventions — RustJSON does not
//! invent new *kinds* of leniency, it combines an established, minimal
//! subset of them into a single hand-rolled Rust parser with no external
//! parsing-crate dependency (`serde_json` is used only for the output
//! *value model*, not for parsing RustJSON's own lenient grammar — matching
//! this codebase's established "hand-roll the protocol/data-shape layer"
//! precedent already applied to WebSocket framing, multipart parsing, and
//! the gRPC Protocol Buffers codec elsewhere in this workspace).

use serde_json::{Map, Number, Value};

/// A parse error, with the byte offset it occurred at (for caller-side
/// error reporting -- e.g. "line N, column M" -- without this crate
/// needing to track line/column itself).
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RustJsonError {
    #[error("unexpected end of input at byte {0}")]
    UnexpectedEof(usize),
    #[error("unexpected character '{1}' at byte {0}")]
    UnexpectedChar(usize, char),
    #[error("invalid number literal at byte {0}")]
    InvalidNumber(usize),
    #[error("invalid escape sequence at byte {0}")]
    InvalidEscape(usize),
    #[error("unterminated string starting at byte {0}")]
    UnterminatedString(usize),
    #[error("unterminated comment starting at byte {0}")]
    UnterminatedComment(usize),
    #[error("trailing data after the top-level value, starting at byte {0}")]
    TrailingData(usize),
}

/// Parse an RustJSON document into a [`serde_json::Value`]. Accepts the
/// grammar extensions documented in this crate's module doc; anything
/// that's valid strict JSON is also accepted (RustJSON is a superset).
pub fn parse(input: &str) -> Result<Value, RustJsonError> {
    let bytes = input.as_bytes();
    let mut pos = 0;
    skip_whitespace_and_comments(bytes, &mut pos)?;
    let value = parse_value(bytes, &mut pos)?;
    skip_whitespace_and_comments(bytes, &mut pos)?;
    if pos != bytes.len() {
        return Err(RustJsonError::TrailingData(pos));
    }
    Ok(value)
}

/// Serialize `value` as canonical, strict-JSON output (compact form).
/// RustJSON's leniency is an *input*-side convenience only — the canonical
/// stored/transmitted form this crate produces is always plain strict
/// JSON, so every downstream consumer (including systems with no RustJSON
/// awareness at all) can read it back unambiguously.
pub fn to_string(value: &Value) -> String {
    // serde_json's own compact serializer already produces strict JSON;
    // reusing it here (rather than hand-rolling a serializer) is the
    // correct side of this crate's "hand-roll parsing, not the value
    // model" boundary -- there is no RustJSON-specific *output* grammar to
    // hand-roll, since the whole point is that the output is exactly
    // standard JSON.
    serde_json::to_string(value).expect("serde_json::Value serialization is infallible")
}

/// Server-side partial extraction (Phase 2, 2026-07-14) — the network-
/// bandwidth-savings benefit from the original RustJSON proposal: pull just
/// the field(s) a caller actually needs out of a stored value, instead of
/// transmitting the whole document and making the client discard the
/// rest.
///
/// `path` is a small dot/bracket path language:
/// - `.` separates object keys: `stats.damage`
/// - `[N]` indexes into an array: `bonuses[0]`
/// - the two compose: `items[2].name`
/// - an empty path (`""`) returns the whole value unchanged.
///
/// Returns `None` if any segment of the path doesn't exist (missing key,
/// out-of-bounds index, or indexing into a non-object/non-array) — a
/// missing field is not an error, it's simply absent, matching how
/// `serde_json::Value::get` already behaves for a single segment.
pub fn extract_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(value);
    }
    let mut current = value;
    for segment in parse_path_segments(path) {
        current = match segment {
            PathSegment::Key(key) => current.as_object()?.get(key)?,
            PathSegment::Index(idx) => current.as_array()?.get(idx)?,
        };
    }
    Some(current)
}

enum PathSegment<'a> {
    Key(&'a str),
    Index(usize),
}

/// Splits `"items[2].name"` into `[Key("items"), Index(2), Key("name")]`.
/// Hand-rolled (no regex dependency) to match this crate's existing
/// "no external parsing crate" boundary.
fn parse_path_segments(path: &str) -> Vec<PathSegment<'_>> {
    let mut segments = Vec::new();
    for dot_part in path.split('.') {
        let mut rest = dot_part;
        // A dot-separated part may itself carry one or more `[N]` index
        // suffixes -- split those off first, left to right.
        loop {
            if let Some(bracket_start) = rest.find('[') {
                let (key_part, bracket_and_after) = rest.split_at(bracket_start);
                if !key_part.is_empty() {
                    segments.push(PathSegment::Key(key_part));
                }
                let Some(bracket_end) = bracket_and_after.find(']') else { break };
                let index_str = &bracket_and_after[1..bracket_end];
                if let Ok(idx) = index_str.parse::<usize>() {
                    segments.push(PathSegment::Index(idx));
                }
                rest = &bracket_and_after[bracket_end + 1..];
                if rest.is_empty() {
                    break;
                }
            } else {
                if !rest.is_empty() {
                    segments.push(PathSegment::Key(rest));
                }
                break;
            }
        }
    }
    segments
}

fn skip_whitespace_and_comments(bytes: &[u8], pos: &mut usize) -> Result<(), RustJsonError> {
    loop {
        while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
        if *pos + 1 < bytes.len() && bytes[*pos] == b'/' && bytes[*pos + 1] == b'/' {
            *pos += 2;
            while *pos < bytes.len() && bytes[*pos] != b'\n' {
                *pos += 1;
            }
            continue;
        }
        if *pos + 1 < bytes.len() && bytes[*pos] == b'/' && bytes[*pos + 1] == b'*' {
            let start = *pos;
            *pos += 2;
            loop {
                if *pos + 1 >= bytes.len() {
                    if bytes.get(*pos..*pos + 2) == Some(b"*/") {
                        *pos += 2;
                        break;
                    }
                    return Err(RustJsonError::UnterminatedComment(start));
                }
                if bytes[*pos] == b'*' && bytes[*pos + 1] == b'/' {
                    *pos += 2;
                    break;
                }
                *pos += 1;
            }
            continue;
        }
        break;
    }
    Ok(())
}

fn peek(bytes: &[u8], pos: usize) -> Result<u8, RustJsonError> {
    bytes.get(pos).copied().ok_or(RustJsonError::UnexpectedEof(pos))
}

fn parse_value(bytes: &[u8], pos: &mut usize) -> Result<Value, RustJsonError> {
    skip_whitespace_and_comments(bytes, pos)?;
    match peek(bytes, *pos)? {
        b'{' => parse_object(bytes, pos),
        b'[' => parse_array(bytes, pos),
        b'"' => Ok(Value::String(parse_quoted_string(bytes, pos, b'"')?)),
        b'\'' => Ok(Value::String(parse_quoted_string(bytes, pos, b'\'')?)),
        b't' => parse_literal(bytes, pos, "true", Value::Bool(true)),
        b'f' => parse_literal(bytes, pos, "false", Value::Bool(false)),
        b'n' => parse_literal(bytes, pos, "null", Value::Null),
        b'-' | b'0'..=b'9' => parse_number(bytes, pos),
        c => Err(RustJsonError::UnexpectedChar(*pos, c as char)),
    }
}

fn parse_literal(bytes: &[u8], pos: &mut usize, literal: &str, value: Value) -> Result<Value, RustJsonError> {
    let end = *pos + literal.len();
    if bytes.get(*pos..end) == Some(literal.as_bytes()) {
        *pos = end;
        Ok(value)
    } else {
        Err(RustJsonError::UnexpectedChar(*pos, peek(bytes, *pos)? as char))
    }
}

fn parse_object(bytes: &[u8], pos: &mut usize) -> Result<Value, RustJsonError> {
    debug_assert_eq!(bytes[*pos], b'{');
    *pos += 1;
    let mut map = Map::new();
    loop {
        skip_whitespace_and_comments(bytes, pos)?;
        if peek(bytes, *pos)? == b'}' {
            *pos += 1;
            break;
        }
        let key = parse_key(bytes, pos)?;
        skip_whitespace_and_comments(bytes, pos)?;
        if peek(bytes, *pos)? != b':' {
            return Err(RustJsonError::UnexpectedChar(*pos, peek(bytes, *pos)? as char));
        }
        *pos += 1;
        let value = parse_value(bytes, pos)?;
        map.insert(key, value);
        skip_whitespace_and_comments(bytes, pos)?;
        match peek(bytes, *pos)? {
            b',' => {
                *pos += 1;
                skip_whitespace_and_comments(bytes, pos)?;
                // Trailing comma: a `}` right after the comma closes the
                // object instead of requiring another key/value pair.
                if peek(bytes, *pos)? == b'}' {
                    *pos += 1;
                    break;
                }
            }
            b'}' => {
                *pos += 1;
                break;
            }
            c => return Err(RustJsonError::UnexpectedChar(*pos, c as char)),
        }
    }
    Ok(Value::Object(map))
}

/// An object key: either a quoted string (double or single) or, as
/// RustJSON's unquoted-key extension, a bare identifier
/// (`[A-Za-z_][A-Za-z0-9_]*`).
fn parse_key(bytes: &[u8], pos: &mut usize) -> Result<String, RustJsonError> {
    match peek(bytes, *pos)? {
        b'"' => parse_quoted_string(bytes, pos, b'"'),
        b'\'' => parse_quoted_string(bytes, pos, b'\''),
        c if c == b'_' || c.is_ascii_alphabetic() => {
            let start = *pos;
            *pos += 1;
            while *pos < bytes.len() && (bytes[*pos] == b'_' || bytes[*pos].is_ascii_alphanumeric()) {
                *pos += 1;
            }
            Ok(String::from_utf8_lossy(&bytes[start..*pos]).into_owned())
        }
        c => Err(RustJsonError::UnexpectedChar(*pos, c as char)),
    }
}

fn parse_array(bytes: &[u8], pos: &mut usize) -> Result<Value, RustJsonError> {
    debug_assert_eq!(bytes[*pos], b'[');
    *pos += 1;
    let mut items = Vec::new();
    loop {
        skip_whitespace_and_comments(bytes, pos)?;
        if peek(bytes, *pos)? == b']' {
            *pos += 1;
            break;
        }
        items.push(parse_value(bytes, pos)?);
        skip_whitespace_and_comments(bytes, pos)?;
        match peek(bytes, *pos)? {
            b',' => {
                *pos += 1;
                skip_whitespace_and_comments(bytes, pos)?;
                // Trailing comma: a `]` right after the comma closes the
                // array instead of requiring another element.
                if peek(bytes, *pos)? == b']' {
                    *pos += 1;
                    break;
                }
            }
            b']' => {
                *pos += 1;
                break;
            }
            c => return Err(RustJsonError::UnexpectedChar(*pos, c as char)),
        }
    }
    Ok(Value::Array(items))
}

fn parse_quoted_string(bytes: &[u8], pos: &mut usize, quote: u8) -> Result<String, RustJsonError> {
    let start = *pos;
    debug_assert_eq!(bytes[*pos], quote);
    *pos += 1;
    let mut out = String::new();
    loop {
        let c = *bytes.get(*pos).ok_or(RustJsonError::UnterminatedString(start))?;
        if c == quote {
            *pos += 1;
            return Ok(out);
        }
        if c == b'\\' {
            *pos += 1;
            let escaped = *bytes.get(*pos).ok_or(RustJsonError::UnterminatedString(start))?;
            match escaped {
                b'"' => out.push('"'),
                b'\'' => out.push('\''),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'n' => out.push('\n'),
                b't' => out.push('\t'),
                b'r' => out.push('\r'),
                b'b' => out.push('\u{8}'),
                b'f' => out.push('\u{c}'),
                b'u' => {
                    let hex_start = *pos + 1;
                    let hex = bytes
                        .get(hex_start..hex_start + 4)
                        .and_then(|h| std::str::from_utf8(h).ok())
                        .ok_or(RustJsonError::InvalidEscape(*pos))?;
                    let code = u32::from_str_radix(hex, 16).map_err(|_| RustJsonError::InvalidEscape(*pos))?;
                    let ch = char::from_u32(code).ok_or(RustJsonError::InvalidEscape(*pos))?;
                    out.push(ch);
                    *pos += 4;
                }
                _ => return Err(RustJsonError::InvalidEscape(*pos)),
            }
            *pos += 1;
            continue;
        }
        // Decode one UTF-8 scalar starting at `c` rather than assuming
        // ASCII -- object/array/string content may contain any Unicode
        // text.
        let ch_len = utf8_char_len(c);
        let ch_bytes = bytes
            .get(*pos..*pos + ch_len)
            .ok_or(RustJsonError::UnterminatedString(start))?;
        let ch_str = std::str::from_utf8(ch_bytes).map_err(|_| RustJsonError::UnterminatedString(start))?;
        out.push_str(ch_str);
        *pos += ch_len;
    }
}

fn utf8_char_len(first_byte: u8) -> usize {
    if first_byte & 0x80 == 0 {
        1
    } else if first_byte & 0xE0 == 0xC0 {
        2
    } else if first_byte & 0xF0 == 0xE0 {
        3
    } else {
        4
    }
}

fn parse_number(bytes: &[u8], pos: &mut usize) -> Result<Value, RustJsonError> {
    let start = *pos;
    if peek(bytes, *pos)? == b'-' {
        *pos += 1;
    }
    let digits_start = *pos;
    while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if *pos == digits_start {
        return Err(RustJsonError::InvalidNumber(start));
    }
    // Track whether this literal has a fractional/exponent part -- an
    // integer literal like `3` must round-trip as an integer `Number`
    // (matching what `serde_json`'s own parser produces for the same
    // input), not silently widen to a float representation the way a
    // naive "always parse as f64" implementation would (`3` becoming
    // `3.0`, which compares unequal to strict-JSON-parsed `3`).
    let mut is_integer = true;
    if *pos < bytes.len() && bytes[*pos] == b'.' {
        is_integer = false;
        *pos += 1;
        let frac_start = *pos;
        while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
            *pos += 1;
        }
        if *pos == frac_start {
            return Err(RustJsonError::InvalidNumber(start));
        }
    }
    if *pos < bytes.len() && (bytes[*pos] == b'e' || bytes[*pos] == b'E') {
        is_integer = false;
        *pos += 1;
        if *pos < bytes.len() && (bytes[*pos] == b'+' || bytes[*pos] == b'-') {
            *pos += 1;
        }
        let exp_start = *pos;
        while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
            *pos += 1;
        }
        if *pos == exp_start {
            return Err(RustJsonError::InvalidNumber(start));
        }
    }
    let text = std::str::from_utf8(&bytes[start..*pos]).map_err(|_| RustJsonError::InvalidNumber(start))?;
    let number: Number = if is_integer {
        if let Ok(i) = text.parse::<i64>() {
            Number::from(i)
        } else if let Ok(u) = text.parse::<u64>() {
            Number::from(u)
        } else {
            text.parse::<f64>().ok().and_then(Number::from_f64).ok_or(RustJsonError::InvalidNumber(start))?
        }
    } else {
        text.parse::<f64>().ok().and_then(Number::from_f64).ok_or(RustJsonError::InvalidNumber(start))?
    };
    Ok(Value::Number(number))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_strict_json_unchanged() {
        let input = r#"{"a": 1, "b": [true, false, null], "c": "hello"}"#;
        assert_eq!(parse(input).unwrap(), json!({"a": 1, "b": [true, false, null], "c": "hello"}));
    }

    #[test]
    fn trailing_comma_in_object_is_accepted() {
        assert_eq!(parse(r#"{"a": 1,}"#).unwrap(), json!({"a": 1}));
    }

    #[test]
    fn trailing_comma_in_array_is_accepted() {
        assert_eq!(parse("[1, 2, 3,]").unwrap(), json!([1, 2, 3]));
    }

    #[test]
    fn line_comments_are_skipped() {
        let input = "{\n  // this is the quantity\n  \"qty\": 5\n}";
        assert_eq!(parse(input).unwrap(), json!({"qty": 5}));
    }

    #[test]
    fn block_comments_are_skipped() {
        let input = r#"{ "a": /* inline note */ 1 }"#;
        assert_eq!(parse(input).unwrap(), json!({"a": 1}));
    }

    #[test]
    fn unquoted_identifier_keys_are_accepted() {
        assert_eq!(parse("{name: 'sword', qty: 3}").unwrap(), json!({"name": "sword", "qty": 3}));
    }

    #[test]
    fn single_quoted_strings_are_accepted() {
        assert_eq!(parse("['a', 'b']").unwrap(), json!(["a", "b"]));
    }

    #[test]
    fn nested_structures_with_all_extensions_combined() {
        let input = r#"
        {
            // top-level comment
            name: 'longsword',
            stats: {
                damage: 12,
                bonuses: [1, 2, 3,], // trailing comma
            },
        }
        "#;
        assert_eq!(
            parse(input).unwrap(),
            json!({"name": "longsword", "stats": {"damage": 12, "bonuses": [1, 2, 3]}})
        );
    }

    #[test]
    fn escape_sequences_in_strings_round_trip() {
        assert_eq!(parse(r#""line1\nline2\ttab""#).unwrap(), json!("line1\nline2\ttab"));
    }

    #[test]
    fn unicode_escape_is_decoded() {
        assert_eq!(parse(r#""é""#).unwrap(), json!("\u{e9}"));
    }

    #[test]
    fn non_ascii_content_is_preserved() {
        assert_eq!(parse(r#""こんにちは""#).unwrap(), json!("こんにちは"));
    }

    #[test]
    fn unterminated_string_is_an_error() {
        assert!(matches!(parse(r#""unterminated"#), Err(RustJsonError::UnterminatedString(_))));
    }

    #[test]
    fn unterminated_block_comment_is_an_error() {
        assert!(matches!(parse("{ /* never closed"), Err(RustJsonError::UnterminatedComment(_))));
    }

    #[test]
    fn invalid_key_without_quotes_or_identifier_shape_is_an_error() {
        assert!(parse("{123abc: 1}").is_err());
    }

    #[test]
    fn trailing_data_after_top_level_value_is_an_error() {
        assert!(matches!(parse("{} garbage"), Err(RustJsonError::TrailingData(_))));
    }

    #[test]
    fn to_string_produces_strict_json_regardless_of_how_lenient_the_source_was() {
        let value = parse("{name: 'sword', qty: 3,}").unwrap();
        let canonical = to_string(&value);
        // Round-trips through strict serde_json parsing (no RustJSON leniency
        // needed to read it back) -- proving the *output* is always plain
        // strict JSON.
        let reparsed: Value = serde_json::from_str(&canonical).unwrap();
        assert_eq!(reparsed, value);
        assert!(!canonical.contains("//"), "canonical output must not carry comments through");
    }

    #[test]
    fn numbers_including_negative_fraction_and_exponent() {
        assert_eq!(parse("-3.5e2").unwrap(), json!(-350.0));
    }

    #[test]
    fn extract_path_empty_returns_whole_value() {
        let value = json!({"a": 1});
        assert_eq!(extract_path(&value, ""), Some(&value));
    }

    #[test]
    fn extract_path_single_key() {
        let value = json!({"name": "sword", "qty": 3});
        assert_eq!(extract_path(&value, "name"), Some(&json!("sword")));
    }

    #[test]
    fn extract_path_nested_keys() {
        let value = json!({"stats": {"damage": 12, "weight": 4}});
        assert_eq!(extract_path(&value, "stats.damage"), Some(&json!(12)));
    }

    #[test]
    fn extract_path_array_index() {
        let value = json!({"bonuses": [10, 20, 30]});
        assert_eq!(extract_path(&value, "bonuses[1]"), Some(&json!(20)));
    }

    #[test]
    fn extract_path_combined_key_and_index() {
        let value = json!({"items": [{"name": "sword"}, {"name": "shield"}]});
        assert_eq!(extract_path(&value, "items[1].name"), Some(&json!("shield")));
    }

    #[test]
    fn extract_path_missing_key_returns_none() {
        let value = json!({"a": 1});
        assert_eq!(extract_path(&value, "b"), None);
    }

    #[test]
    fn extract_path_out_of_bounds_index_returns_none() {
        let value = json!({"bonuses": [1, 2]});
        assert_eq!(extract_path(&value, "bonuses[5]"), None);
    }

    #[test]
    fn extract_path_indexing_into_non_array_returns_none() {
        let value = json!({"name": "sword"});
        assert_eq!(extract_path(&value, "name[0]"), None);
    }
}
