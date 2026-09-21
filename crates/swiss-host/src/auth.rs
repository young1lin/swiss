/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Credential helpers for the HTTP surface — port of `auth.ts`.
//!
//! The MCP endpoints (`POST /<name>`) stay bearer-token-gated: that token is what AI clients
//! authenticate with and is managed per client from the panel. The management API under /api has
//! no credential check — the loopback guard is its boundary.

/// Pull the `<token>` out of `Authorization: Bearer <token>`, or `""` when the header is absent.
///
/// The scheme is matched case-insensitively and across any run of spaces, because RFC 7235 says
/// the scheme token is case-insensitive and separated by one or more SP — a client sending
/// `bearer <token>` is conformant, and rejecting it looked from the outside like a wrong token.
/// Hand-rolled rather than a regex, per ADR-007; the Node build pins the same cases.
pub fn bearer_secret(header: Option<&str>) -> &str {
    let Some(header) = header else { return "" };
    let Some(rest) = strip_bearer_prefix(header) else {
        return "";
    };
    rest.trim()
}

/// Drop a leading case-insensitive `bearer` + one-or-more-spaces prefix. Everything after the
/// spaces is kept verbatim (the token itself may contain spaces; the caller trims).
fn strip_bearer_prefix(header: &str) -> Option<&str> {
    // `get(..6)` is None when the header is short or the cut lands mid-UTF-8-char — both mean
    // "not a bearer header", never a panic.
    if !header.get(..6)?.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let bytes = header.as_bytes();
    if bytes.get(6) != Some(&b' ') {
        return None;
    }
    let mut end = 6;
    while bytes.get(end) == Some(&b' ') {
        end += 1;
    }
    // `end` always sits just past an ASCII space, so it is a char boundary.
    header.get(end..)
}

#[cfg(test)]
mod tests {
    // Ported from test/auth.test.ts, assertion for assertion.
    use super::bearer_secret;

    #[test]
    fn pulls_the_token_out_of_a_bearer_header() {
        assert_eq!(bearer_secret(Some("Bearer s3cret-token")), "s3cret-token");
    }

    #[test]
    fn returns_empty_when_the_header_is_absent() {
        assert_eq!(bearer_secret(None), "");
    }

    #[test]
    fn returns_empty_for_a_non_bearer_scheme() {
        assert_eq!(bearer_secret(Some("Basic s3cret-token")), "");
    }

    #[test]
    fn accepts_a_lowercase_scheme_and_any_run_of_spaces() {
        assert_eq!(bearer_secret(Some("bearer s3cret-token")), "s3cret-token");
        assert_eq!(bearer_secret(Some("BEARER s3cret-token")), "s3cret-token");
        assert_eq!(bearer_secret(Some("Bearer   s3cret-token")), "s3cret-token");
        // Everything after the prefix is the token — a second word is kept, not dropped.
        assert_eq!(
            bearer_secret(Some("Bearer s3cret-token extra")),
            "s3cret-token extra"
        );
    }
}
