//! Minimal, hand-written GraphQL SDL parser sufficient to extract
//! `ServiceSchema` (type name -> field names) from a real subgraph's raw
//! SDL text, plus detection of whether that SDL follows Apollo Federation
//! v1 (bare `@key`/`@requires`/`@provides`/`@external` directives, no
//! `@link` spec import) or v2 (`@link(url: "https://specs.apollo.dev/
//! federation/v2...")`) conventions.
//!
//! This does not implement a general-purpose GraphQL parser (no
//! validation, no support for `input`/`enum`/`union`/`scalar` blocks since
//! those never contribute fields to a composed object/interface type) —
//! it is scoped exactly to what `compose()` needs: type/interface name ->
//! field name extraction, tolerant of directives, arguments, and
//! `implements` clauses in either federation dialect.

use crate::{FederationVersion, ServiceSchema};
use open_runo_core::{AppError, Result};
use std::collections::{BTreeMap, BTreeSet};

/// Parses a subgraph's raw SDL text into a [`ServiceSchema`] by extracting
/// every `type`/`interface`/`extend type` block's field names. Directives,
/// arguments, and `implements` clauses are skipped (composition in this
/// engine only tracks type/field presence, not directive semantics), which
/// is what makes this parser federation-v1/v2-agnostic: both dialects
/// place their directives in the same syntactic positions, so a parser
/// that simply skips `@directive(...)` tokens wherever they appear works
/// identically for either style.
pub fn parse_service_sdl(service_name: &str, sdl: &str) -> Result<ServiceSchema> {
    let mut types: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let n = sdl.len();
    let mut i = 0usize;

    while i < n {
        if let Some(kw_end) = match_type_keyword(sdl, i) {
            let mut j = skip_ws_and_comments(sdl, kw_end);
            let (name, name_end) = read_ident(sdl, j);
            if name.is_empty() {
                i = j.max(i + 1);
                continue;
            }
            j = skip_ws_and_comments(sdl, name_end);
            j = skip_implements_and_directives(sdl, j);
            j = skip_ws_and_comments(sdl, j);

            if sdl.as_bytes().get(j) == Some(&b'{') {
                if let Some((block, after)) = read_balanced_braces(sdl, j) {
                    let fields = parse_fields(block);
                    types.entry(name.to_string()).or_default().extend(fields);
                    i = after;
                    continue;
                }
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }

    if types.is_empty() {
        return Err(AppError::Validation(format!(
            "SDL for service '{service_name}' contains no type/interface \
             definitions with fields (parse produced an empty schema)"
        )));
    }

    Ok(ServiceSchema {
        service_name: service_name.to_string(),
        types,
    })
}

/// Detects which Apollo Federation dialect a subgraph's SDL follows.
///
/// - [`FederationVersion::V2`]: an `@link` directive importing
///   `https://specs.apollo.dev/federation/v2*` is present (the explicit,
///   required spec-import mechanism Federation 2 introduced).
/// - [`FederationVersion::V1`]: no such `@link` import, but at least one
///   of the classic federation directives (`@key`, `@requires`,
///   `@provides`, `@external`) appears directly on a type/field — the
///   pre-Federation-2 convention where these directives were implicitly
///   available without an import statement.
/// - [`FederationVersion::None`]: no federation directives detected at
///   all (a plain, non-federated GraphQL SDL).
pub fn detect_federation_version(sdl: &str) -> FederationVersion {
    if sdl.contains("specs.apollo.dev/federation/v2") {
        return FederationVersion::V2;
    }
    let v1_directives = ["@key", "@requires", "@provides", "@external"];
    if v1_directives.iter().any(|d| sdl.contains(d)) {
        return FederationVersion::V1;
    }
    FederationVersion::None
}

fn match_type_keyword(s: &str, pos: usize) -> Option<usize> {
    let rest = &s[pos..];
    // Optional leading "extend " before "type"/"interface".
    let rest = rest.strip_prefix("extend").map_or(rest, |r| {
        if r.starts_with(char::is_whitespace) {
            r
        } else {
            rest
        }
    });
    let offset = s[pos..].len() - rest.len();
    let rest = skip_leading_ws(rest);
    let ws_skip = (s[pos..].len() - rest.len()) - offset;

    for kw in ["type", "interface"] {
        if let Some(after_kw) = rest.strip_prefix(kw) {
            let boundary_ok = match after_kw.chars().next() {
                None => true,
                Some(c) => c.is_whitespace() || c == '{',
            };
            let before_ok = is_word_boundary_before(s, pos);
            if boundary_ok && before_ok {
                return Some(pos + offset + ws_skip + kw.len());
            }
        }
    }
    None
}

fn is_word_boundary_before(s: &str, pos: usize) -> bool {
    match s[..pos].chars().next_back() {
        None => true,
        Some(c) => !is_ident_char(c),
    }
}

fn skip_leading_ws(s: &str) -> &str {
    s.trim_start_matches(|c: char| c.is_whitespace())
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn skip_ws_and_comments(s: &str, mut pos: usize) -> usize {
    let bytes = s.as_bytes();
    loop {
        while pos < bytes.len() && (bytes[pos] as char).is_whitespace() {
            pos += 1;
        }
        if pos < bytes.len() && bytes[pos] == b'#' {
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }
        break;
    }
    pos
}

fn read_ident(s: &str, pos: usize) -> (&str, usize) {
    let bytes = s.as_bytes();
    let start = pos;
    let mut end = pos;
    while end < bytes.len() && is_ident_char(bytes[end] as char) {
        end += 1;
    }
    (&s[start..end], end)
}

/// Skips an `implements A & B & C` clause and any number of `@directive`
/// or `@directive(...)` tokens, in whatever order/mix they appear (both
/// federation dialects place them here identically).
fn skip_implements_and_directives(s: &str, mut pos: usize) -> usize {
    loop {
        pos = skip_ws_and_comments(s, pos);
        if s[pos..].starts_with("implements") {
            pos += "implements".len();
            loop {
                pos = skip_ws_and_comments(s, pos);
                if s.as_bytes().get(pos) == Some(&b'&') {
                    pos += 1;
                    continue;
                }
                let (ident, after) = read_ident(s, pos);
                if ident.is_empty() {
                    break;
                }
                pos = after;
            }
            continue;
        }
        if s.as_bytes().get(pos) == Some(&b'@') {
            pos += 1;
            let (_name, after) = read_ident(s, pos);
            pos = after;
            pos = skip_ws_and_comments(s, pos);
            if s.as_bytes().get(pos) == Some(&b'(') {
                if let Some(after_parens) = skip_balanced(s, pos, b'(', b')') {
                    pos = after_parens;
                } else {
                    break;
                }
            }
            continue;
        }
        break;
    }
    pos
}

fn skip_balanced(s: &str, pos: usize, open: u8, close: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.get(pos) != Some(&open) {
        return None;
    }
    let mut depth = 0i32;
    let mut i = pos;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
                in_string = false;
            }
        } else if c == b'"' {
            in_string = true;
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
}

fn read_balanced_braces(s: &str, pos: usize) -> Option<(&str, usize)> {
    let after = skip_balanced(s, pos, b'{', b'}')?;
    Some((&s[pos + 1..after - 1], after))
}

/// Extracts field names from a type/interface body. Each field is of the
/// form `name(args...): Type @directives` — arguments, the type, and
/// directives are all skipped; only the leading identifier that is
/// eventually followed by `:` (after an optional argument list) counts as
/// a field name.
fn parse_fields(block: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let n = block.len();
    let mut i = 0usize;

    while i < n {
        i = skip_ws_and_comments(block, i);
        if i >= n {
            break;
        }
        let (ident, after_ident) = read_ident(block, i);
        if ident.is_empty() {
            i += 1;
            continue;
        }
        let mut j = skip_ws_and_comments(block, after_ident);
        if block.as_bytes().get(j) == Some(&b'(') {
            match skip_balanced(block, j, b'(', b')') {
                Some(after) => j = after,
                None => {
                    i = after_ident;
                    continue;
                },
            }
            j = skip_ws_and_comments(block, j);
        }
        if block.as_bytes().get(j) == Some(&b':') {
            fields.push(ident.to_string());
            j += 1;
            j = skip_type_reference(block, j);
            j = skip_implements_and_directives(block, j);
            i = j;
        } else {
            i = after_ident;
        }
    }

    fields
}

/// Skips a GraphQL type reference: an identifier optionally wrapped in
/// `[...]` (list) and/or suffixed with `!` (non-null), e.g. `[User!]!`.
fn skip_type_reference(s: &str, mut pos: usize) -> usize {
    pos = skip_ws_and_comments(s, pos);
    if s.as_bytes().get(pos) == Some(&b'[') {
        if let Some(after) = skip_balanced(s, pos, b'[', b']') {
            pos = after;
        }
    } else {
        let (_ident, after) = read_ident(s, pos);
        pos = after;
    }
    while s.as_bytes().get(pos) == Some(&b'!') {
        pos += 1;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Authentic Federation v1-style subgraph: `@key`/`@external` used
    /// directly, no `@link` spec import (the convention used by
    /// pre-Federation-2 subgraph libraries).
    const V1_SUBGRAPH_SDL: &str = r#"
        type Query {
          me: User
        }

        type User @key(fields: "id") {
          id: ID!
          name: String
          reviews: [Review]
        }

        type Review @key(fields: "id") {
          id: ID!
          body: String
          author: User @external
        }
    "#;

    /// Authentic Federation v2-style subgraph: explicit `@link` import of
    /// the v2 spec, plus v2-only directives (`@shareable`).
    const V2_SUBGRAPH_SDL: &str = r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.3",
                import: ["@key", "@shareable"])

        type Query {
          billingHealth: String
        }

        type User @key(fields: "id") {
          id: ID!
          plan: String @shareable
          balanceCents: Int
        }
    "#;

    #[test]
    fn detects_v1_from_bare_directives_without_link() {
        assert_eq!(
            detect_federation_version(V1_SUBGRAPH_SDL),
            FederationVersion::V1
        );
    }

    #[test]
    fn detects_v2_from_link_header() {
        assert_eq!(
            detect_federation_version(V2_SUBGRAPH_SDL),
            FederationVersion::V2
        );
    }

    #[test]
    fn detects_none_for_plain_sdl() {
        let plain = "type Query { hello: String }";
        assert_eq!(detect_federation_version(plain), FederationVersion::None);
    }

    #[test]
    fn parses_v1_subgraph_into_service_schema() {
        let schema = parse_service_sdl("users-service", V1_SUBGRAPH_SDL).unwrap();
        assert_eq!(schema.service_name, "users-service");
        assert!(schema.types["Query"].contains("me"));
        assert!(schema.types["User"].contains("id"));
        assert!(schema.types["User"].contains("name"));
        assert!(schema.types["User"].contains("reviews"));
        assert!(schema.types["Review"].contains("body"));
        assert!(schema.types["Review"].contains("author"));
    }

    #[test]
    fn parses_v2_subgraph_into_service_schema() {
        let schema = parse_service_sdl("billing-service", V2_SUBGRAPH_SDL).unwrap();
        assert_eq!(schema.service_name, "billing-service");
        assert!(schema.types["Query"].contains("billingHealth"));
        assert!(schema.types["User"].contains("id"));
        assert!(schema.types["User"].contains("plan"));
        assert!(schema.types["User"].contains("balanceCents"));
    }

    /// The actual point of "Federation v1 compatibility": a v1-style
    /// subgraph and a v2-style subgraph, parsed independently through the
    /// *same* code path, compose into one correct supergraph together —
    /// proving v1 subgraphs aren't rejected or mishandled by a pipeline
    /// that also accepts v2 subgraphs.
    #[test]
    fn v1_and_v2_subgraphs_compose_together_into_one_supergraph() {
        let v1 = parse_service_sdl("users-service", V1_SUBGRAPH_SDL).unwrap();
        let v2 = parse_service_sdl("billing-service", V2_SUBGRAPH_SDL).unwrap();

        assert_eq!(
            detect_federation_version(V1_SUBGRAPH_SDL),
            FederationVersion::V1
        );
        assert_eq!(
            detect_federation_version(V2_SUBGRAPH_SDL),
            FederationVersion::V2
        );

        let composed = crate::compose(&[v1, v2]).unwrap();
        assert_eq!(composed.contributing_services.len(), 2);
        // User is declared by both services (users-service in v1 style,
        // billing-service in v2 style) — fields merge into one type.
        let user_fields = &composed.types["User"];
        assert!(user_fields.contains("name")); // from v1 subgraph
        assert!(user_fields.contains("plan")); // from v2 subgraph
        assert!(user_fields.contains("id")); // declared in both
    }

    #[test]
    fn empty_sdl_is_rejected_with_clear_error() {
        let err = parse_service_sdl("empty-service", "# just a comment\n").unwrap_err();
        assert!(err.to_string().contains("empty-service"));
    }

    #[test]
    fn handles_multiple_directives_and_no_directives_mixed() {
        let sdl = r#"
            type Product @key(fields: "sku") @tag(name: "internal") {
              sku: ID!
              price: Int
              description: String @deprecated(reason: "use longDescription")
            }
        "#;
        let schema = parse_service_sdl("catalog-service", sdl).unwrap();
        assert!(schema.types["Product"].contains("sku"));
        assert!(schema.types["Product"].contains("price"));
        assert!(schema.types["Product"].contains("description"));
    }
}
