/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The secrets check (SPEC §memory.capture): a hand-written scan for credential
//! shapes in text about to be stored. A hit refuses the write and names the rule,
//! never the match. Defence in depth, not a guarantee - a note store that persists
//! across sessions must not quietly become a credential store.
//!
//! [redact] is the same rules turned into replacements, plus rules too loose to
//! refuse a write on (a long mixed-case token, a short password, a URL's password):
//! sync's conversation digest
//! (SPEC §memory.sync) passes through it before any of it leaves the process, and a
//! false positive there costs one replaced word, never a refused note.

/// One credential-shaped span of a text: its byte range and the rule that matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hit {
    start: usize,
    end: usize,
    rule: &'static str,
}

/// The rules [scan] applies, in the order it names them.
const SCAN_RULES: [&str; 12] = [
    "pem_private_key",
    "aws_access_key",
    "github_token",
    "slack_token",
    "jwt",
    "api_key",
    "dotted_api_key",
    "bearer_token",
    "assignment",
    // Redaction only (see [redact]); listed so a rule name is never unknown.
    "mixed_token",
    "password",
    "url_password",
];

/// The rule a text trips, or None.
pub fn scan(text: &str) -> Option<&'static str> {
    let hits = hits(text, false);
    SCAN_RULES.iter().copied().find(|rule| hits.iter().any(|hit| hit.rule == *rule))
}

/// The text with every credential-shaped span replaced by `[redacted:<rule>]`: the
/// [scan] rules, long mixed-case tokens, short passwords and a URL's password. Text
/// around a hit is kept byte for byte.
pub fn redact(text: &str) -> String {
    redacted(text, false)
}

/// [redact], each replaced span's line breaks kept after its marker: a line number read
/// from the result is the text's - what a definition or a section is found at.
pub fn redact_lines(text: &str) -> String {
    redacted(text, true)
}

fn redacted(text: &str, lines: bool) -> String {
    let mut hits = hits(text, true);
    if hits.is_empty() {
        return text.to_string();
    }
    hits.sort_by_key(|hit| (hit.start, std::cmp::Reverse(hit.end)));
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for hit in hits {
        if hit.start < at {
            // Inside a span already replaced (a JWT's segment, an assignment's value).
            continue;
        }
        out.push_str(&text[at..hit.start]);
        out.push_str("[redacted:");
        out.push_str(hit.rule);
        out.push(']');
        if lines {
            out.extend(text[hit.start..hit.end].matches('\n'));
        }
        at = hit.end;
    }
    out.push_str(&text[at..]);
    out
}

/// Every hit of every rule; `loose` adds the redaction-only rules.
fn hits(text: &str, loose: bool) -> Vec<Hit> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    pem_private_key(text, &mut out);
    prefixed(bytes, b"AKIA", 16, 16, |b| b.is_ascii_uppercase() || b.is_ascii_digit(), "aws_access_key", &mut out);
    for prefix in [&b"ghp_"[..], b"gho_", b"ghu_", b"ghs_", b"github_pat_"] {
        prefixed(bytes, prefix, 20, usize::MAX, token_byte, "github_token", &mut out);
    }
    for prefix in [&b"xoxa-"[..], b"xoxb-", b"xoxp-", b"xoxr-"] {
        prefixed(bytes, prefix, 10, usize::MAX, |b| b.is_ascii_alphanumeric() || b == b'-', "slack_token", &mut out);
    }
    jwt(bytes, &mut out);
    prefixed(bytes, b"sk-", 20, usize::MAX, |b| token_byte(b) || b == b'-', "api_key", &mut out);
    dotted_api_key(bytes, &mut out);
    bearer(bytes, &mut out);
    assignment(text, &mut out);
    if loose {
        mixed_token(bytes, &mut out);
        password(text, &mut out);
        url_password(bytes, &mut out);
    }
    out
}

fn token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn base64url_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// `prefix` at a word start, followed by `min..=max` bytes of `class` (a run
/// longer than `max` is not this shape).
fn prefixed(
    bytes: &[u8],
    prefix: &[u8],
    min: usize,
    max: usize,
    class: impl Fn(u8) -> bool,
    rule: &'static str,
    out: &mut Vec<Hit>,
) {
    let mut from = 0;
    while let Some(at) = find(&bytes[from..], prefix).map(|i| i + from) {
        let boundary = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        let run = bytes[at + prefix.len()..].iter().take_while(|b| class(**b)).count();
        if boundary && run >= min && run <= max {
            out.push(Hit { start: at, end: at + prefix.len() + run, rule });
        }
        from = at + 1;
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A `-----BEGIN … PRIVATE KEY-----` line through its END line (or the end of the text).
fn pem_private_key(text: &str, out: &mut Vec<Hit>) {
    let mut from = 0;
    while let Some(at) = text[from..].find("-----BEGIN ").map(|i| i + from) {
        let line_end = text[at..].find('\n').map(|i| i + at).unwrap_or(text.len());
        if !text[at..line_end].contains("PRIVATE KEY-----") {
            from = at + 1;
            continue;
        }
        let end = match text[line_end..].find("-----END ") {
            Some(i) => {
                let close = line_end + i;
                text[close + 9..].find("-----").map(|j| close + 9 + j + 5).unwrap_or(text.len())
            }
            None => text.len(),
        };
        out.push(Hit { start: at, end, rule: "pem_private_key" });
        from = end.max(at + 1);
    }
}

/// `eyJ…` header, then two more base64url segments, each at least 10 long.
fn jwt(bytes: &[u8], out: &mut Vec<Hit>) {
    let mut from = 0;
    while let Some(at) = find(&bytes[from..], b"eyJ").map(|i| i + from) {
        let mut pos = at;
        let mut ok = true;
        for segment in 0..3 {
            let run = bytes[pos..].iter().take_while(|b| base64url_byte(**b)).count();
            if run < 10 {
                ok = false;
                break;
            }
            pos += run;
            if segment < 2 {
                if bytes.get(pos) != Some(&b'.') {
                    ok = false;
                    break;
                }
                pos += 1;
            }
        }
        if ok {
            out.push(Hit { start: at, end: pos, rule: "jwt" });
            from = pos;
        } else {
            from = at + 1;
        }
    }
}

/// 32 lowercase hex digits, a dot, 16 letters or digits, standing alone: the shape of
/// a Zhipu (GLM) API key, which no other rule here catches.
fn dotted_api_key(bytes: &[u8], out: &mut Vec<Hit>) {
    let alone = |at: Option<&u8>| at.is_none_or(|b| !b.is_ascii_alphanumeric());
    let mut at = 0;
    while at + 49 <= bytes.len() {
        let hex = bytes[at..at + 32].iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b));
        if hex
            && bytes[at + 32] == b'.'
            && bytes[at + 33..at + 49].iter().all(u8::is_ascii_alphanumeric)
            && (at == 0 || alone(bytes.get(at - 1)))
            && alone(bytes.get(at + 49))
        {
            out.push(Hit { start: at, end: at + 49, rule: "dotted_api_key" });
            at += 49;
        } else {
            at += 1;
        }
    }
}

/// `Bearer ` (any case) then at least 16 bytes of a token: the token is the hit.
fn bearer(bytes: &[u8], out: &mut Vec<Hit>) {
    let lower = bytes.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = find(&lower[from..], b"bearer ").map(|i| i + from) {
        from = at + 1;
        if at > 0 && lower[at - 1].is_ascii_alphanumeric() {
            continue;
        }
        let start = at + 7;
        let mut run = bytes[start..].iter().take_while(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(b)).count();
        // A sentence's closing period is not part of the token.
        while run > 0 && bytes[start + run - 1] == b'.' {
            run -= 1;
        }
        if run >= 16 {
            out.push(Hit { start, end: start + run, rule: "bearer_token" });
        }
    }
}

/// `password|passwd|secret|token|api_key|apikey|api-key` then `:` or `=` then a value
/// of at least 12 token characters holding both a letter and a digit: the value is
/// the hit.
fn assignment(text: &str, out: &mut Vec<Hit>) {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for key in ["password", "passwd", "secret", "token", "api_key", "apikey", "api-key"] {
        let mut from = 0;
        while let Some(at) = find(&bytes[from..], key.as_bytes()).map(|i| i + from) {
            from = at + 1;
            let mut pos = at + key.len();
            pos += skip(&bytes[pos..], |b| b == b' ' || b == b'"' || b == b'\'' || b == b'\t');
            if !matches!(bytes.get(pos), Some(b':') | Some(b'=')) {
                continue;
            }
            pos += 1;
            pos += skip(&bytes[pos..], |b| b == b' ' || b == b'"' || b == b'\'' || b == b'\t' || b == b'=');
            let original = &text.as_bytes()[pos.min(text.len())..];
            let value: Vec<u8> = original.iter().copied().take_while(|b| value_byte(*b)).collect();
            if value.len() >= 12
                && value.iter().any(|b| b.is_ascii_alphabetic())
                && value.iter().any(|b| b.is_ascii_digit())
            {
                out.push(Hit { start: pos, end: pos + value.len(), rule: "assignment" });
            }
        }
    }
}

/// `password|passwd|pwd|密码`, then `:`, `:=`, `=` or `：`, then a value [password_shaped]:
/// the value is the hit. A quoted value runs to its closing quote; a bare one stops at
/// space, punctuation that ends a query, a list or a template, or non-ASCII. In `${NAME:default}` only
/// the default is a value. Redaction only: a value this short is too often a sample or a
/// rule to refuse a note on, and [assignment] already refuses the long mixed ones.
fn password(text: &str, out: &mut Vec<Hit>) {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for key in ["password", "passwd", "pwd", "密码"] {
        let mut from = 0;
        while let Some(at) = find(&bytes[from..], key.as_bytes()).map(|i| i + from) {
            from = at + 1;
            let mut pos = at + key.len();
            pos += skip(&bytes[pos..], |b| b == b' ' || b == b'"' || b == b'\'' || b == b'\t');
            let rest = &bytes[pos..];
            pos += if rest.starts_with(b":=") {
                2
            } else if rest.starts_with(b"==") {
                // A comparison, never a value.
                continue;
            } else if rest.starts_with(b":") || rest.starts_with(b"=") {
                1
            } else if rest.starts_with("：".as_bytes()) {
                "：".len()
            } else {
                continue;
            };
            pos += skip(&bytes[pos..], |b| b == b' ' || b == b'\t');
            let (start, end) = password_value(bytes, pos);
            let (start, end) = match bytes[start..end].strip_prefix(b"${") {
                Some(inner) => match inner.iter().position(|b| *b == b':') {
                    Some(colon) => (start + 2 + colon + 1, end - usize::from(bytes[end - 1] == b'}')),
                    None => continue,
                },
                None => (start, end),
            };
            if password_shaped(&bytes[start..end]) {
                out.push(Hit { start, end, rule: "password" });
            }
        }
    }
}

/// The value at `pos`: inside its quotes when it opens with one that closes within 64
/// bytes on the line, else the bare run.
fn password_value(bytes: &[u8], pos: usize) -> (usize, usize) {
    if let Some(&quote) = bytes.get(pos).filter(|b| **b == b'"' || **b == b'\'') {
        let body = &bytes[pos + 1..];
        let close = body.iter().take(65).take_while(|b| **b != b'\n').position(|b| *b == quote);
        if let Some(close) = close {
            return (pos + 1, pos + 1 + close);
        }
        return bare_value(bytes, pos + 1);
    }
    bare_value(bytes, pos)
}

fn bare_value(bytes: &[u8], pos: usize) -> (usize, usize) {
    let run = skip(&bytes[pos..], |b| b.is_ascii_graphic() && !b"&,;\"'`)<>|}".contains(&b));
    // A sentence's full stop or colon is not part of the value.
    let kept = bytes[pos..pos + run].iter().rev().skip_while(|b| matches!(**b, b'.' | b':')).count();
    (pos, pos + kept)
}

/// A password rather than a word, a name, code, a path or a placeholder: 6 to 64 printable
/// ASCII bytes, a digit or a symbol beyond `_-.`, no call or template (`(`, `<`, `{`), no
/// variable, annotation, list or path opening it (`$`, `#`, `@`, `[`, `/`, `~`, `.`), and
/// not one byte repeated (`******`, `xxxxxx`). So `getPassword()`, `raw_password`,
/// `config.password`, `read-only` and a quoted sentence stay.
fn password_shaped(value: &[u8]) -> bool {
    if !(6..=64).contains(&value.len()) || value.iter().all(|b| *b == value[0]) || !value.iter().all(u8::is_ascii_graphic) {
        return false;
    }
    if value.iter().any(|b| b"(<{".contains(b)) || b"$#@[/~.".contains(&value[0]) {
        return false;
    }
    value.iter().any(|b| b.is_ascii_digit() || !(b.is_ascii_alphabetic() || b"_-.".contains(b)))
}

/// A URL's password: `://`, a user of letters, digits and `._-%` (empty for `redis://:pw@`),
/// `:`, then the password up to `@` - the hit - unless it is a placeholder (`${...}`,
/// `<...>`, one byte repeated). A host's port has no `@` after it and stays.
fn url_password(bytes: &[u8], out: &mut Vec<Hit>) {
    let mut from = 0;
    while let Some(at) = find(&bytes[from..], b"://").map(|i| i + from) {
        from = at + 3;
        let user = skip(&bytes[from..], |b| b.is_ascii_alphanumeric() || b"._-%".contains(&b));
        let colon = from + user;
        if bytes.get(colon) != Some(&b':') {
            continue;
        }
        let start = colon + 1;
        let run = skip(&bytes[start..], |b| b.is_ascii_graphic() && !b"@/?#\"'`<>".contains(&b));
        let end = start + run;
        if bytes.get(end) != Some(&b'@') || run == 0 || run > 64 {
            continue;
        }
        let value = &bytes[start..end];
        let placeholder = value.iter().any(|b| b"${<".contains(b)) || value.iter().all(|b| *b == value[0]);
        if !placeholder {
            out.push(Hit { start, end, rule: "url_password" });
        }
    }
}

fn value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_./+=~!@#$%^&*".contains(&b)
}

fn skip(bytes: &[u8], class: impl Fn(u8) -> bool) -> usize {
    bytes.iter().take_while(|b| class(**b)).count()
}

/// Whether a repo-relative path names something that holds credentials or git's own
/// state: the panel's previews refuse it and the code index never reads it.
pub fn sensitive_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();
    if parts.iter().any(|p| matches!(*p, ".git" | ".ssh" | ".gnupg" | ".aws")) {
        return true;
    }
    let name = parts.last().copied().unwrap_or_default();
    if name.starts_with(".env") {
        return !matches!(name, ".env.example" | ".env.sample" | ".env.template");
    }
    matches!(
        name,
        "master.key"
            | "session.json"
            | "secrets.json"
            | "gateway.config.json"
            | "managed.json"
            | "tunnels.json"
            | ".npmrc"
            | ".netrc"
            | "_netrc"
            | ".pypirc"
            | ".git-credentials"
            | ".htpasswd"
            | "credentials"
            | "credentials.json"
    ) || ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"].iter().any(|key| name.starts_with(key))
        || [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".kdbx"].iter().any(|ext| name.ends_with(ext))
}

/// A run of letters and digits that reads as random rather than as a word: 24 or more
/// with upper case, lower case and a digit, or 32 or more with a digit and a letter
/// beyond hex. Hex hashes, UUIDs and identifiers ([identifier_shaped]) stay.
fn mixed_token(bytes: &[u8], out: &mut Vec<Hit>) {
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_alphanumeric() {
            at += 1;
            continue;
        }
        let run = bytes[at..].iter().take_while(|b| b.is_ascii_alphanumeric()).count();
        let word = &bytes[at..at + run];
        let upper = word.iter().any(u8::is_ascii_uppercase);
        let lower = word.iter().any(u8::is_ascii_lowercase);
        let digits = word.iter().filter(|b| b.is_ascii_digit()).count();
        let beyond_hex = word.iter().any(|b| b.is_ascii_alphabetic() && !b.is_ascii_hexdigit());
        let random = (run >= 24 && upper && lower && digits > 0) || (run >= 32 && digits > 0 && beyond_hex);
        if random && !identifier_shaped(word) {
            out.push(Hit { start: at, end: at + run, rule: "mixed_token" });
        }
        at += run;
    }
}

/// A camelCase or PascalCase name: an optional lower-case head, then words that each open
/// with one capital, digit runs of at most three between them, and three or more words of
/// three or more letters (`handleOrderNoticeV1`). Random keys repeat capitals, run
/// digits long or break words in two, so they almost never read so.
fn identifier_shaped(word: &[u8]) -> bool {
    let (mut at, mut words) = (0, 0);
    while at < word.len() {
        let digits = word[at..].iter().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0 {
            if digits > 3 {
                return false;
            }
            at += digits;
            continue;
        }
        let capital = word[at].is_ascii_uppercase();
        if !capital && at > 0 {
            return false;
        }
        at += usize::from(capital);
        let lower = word[at..].iter().take_while(|b| b.is_ascii_lowercase()).count();
        at += lower;
        // A lone capital closes the name or stands before a digit (`V1`), never before a capital.
        if lower == 0 && word.get(at).is_some_and(|b| !b.is_ascii_digit()) {
            return false;
        }
        if usize::from(capital) + lower >= 3 {
            words += 1;
        }
    }
    words >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic key of the GLM shape - 32 hex, a dot, 16 alphanumerics - that is no
    /// real key.
    const DOTTED: &str = "0123456789abcdef0123456789abcdef.AbCdEfGh12345678";

    #[test]
    fn each_rule_fires_on_its_shape() {
        assert_eq!(scan("-----BEGIN RSA PRIVATE KEY-----\nMIIE"), Some("pem_private_key"));
        assert_eq!(scan("key AKIAABCDEFGHIJKLMNOP here"), Some("aws_access_key"));
        assert_eq!(scan("ghp_abcdefghijklmnopqrstuvwxyz0123"), Some("github_token"));
        assert_eq!(scan("xoxb-1234567890-abc"), Some("slack_token"));
        assert_eq!(scan("eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2QT4f"), Some("jwt"));
        assert_eq!(scan("use sk-proj0123456789abcdefghij"), Some("api_key"));
        assert_eq!(scan(&format!("the key is {DOTTED}")), Some("dotted_api_key"));
        assert_eq!(scan("Authorization: Bearer abcdefghij0123456789"), Some("bearer_token"));
        assert_eq!(scan("DB_PASSWORD=hunter2hunter2x9"), Some("assignment"));
        assert_eq!(scan("access_token: \"a1b2c3d4e5f6g7\""), Some("assignment"));
        assert_eq!(scan("\"apiKey\": \"a1b2c3d4e5f6g7\""), Some("assignment"));
    }

    #[test]
    fn ordinary_engineering_text_passes() {
        assert_eq!(scan("Use BEGIN IMMEDIATE for every mutating transaction."), None);
        assert_eq!(scan("the task-runner keeps max_tokens: 4096 per call"), None);
        assert_eq!(scan("password: read it from the vault, never inline"), None);
        assert_eq!(scan("api_key = os.environ['API_KEY']"), None);
        assert_eq!(scan("token=short1"), None);
        assert_eq!(scan("AKIA is the AWS key prefix"), None);
        assert_eq!(scan("send Authorization: Bearer <token>"), None);
        // The loose rule redacts but never refuses a note.
        assert_eq!(scan("AbCdEfGhIjKlMnOpQrStUv1234"), None);
    }

    #[test]
    fn redact_replaces_each_hit_and_keeps_the_rest() {
        let text = format!(
            "export ANTHROPIC_AUTH_TOKEN={DOTTED}\nthen \"ANTHROPIC_AUTH_TOKEN\": \"{DOTTED}\" and Bearer abcdefghij0123456789xyz."
        );
        let out = redact(&text);
        assert!(!out.contains(DOTTED) && !out.contains("abcdefghij0123456789xyz"), "{out}");
        assert!(out.starts_with("export ANTHROPIC_AUTH_TOKEN=[redacted:"), "{out}");
        assert!(out.contains("\"ANTHROPIC_AUTH_TOKEN\": \"[redacted:"), "{out}");
        assert!(out.ends_with("Bearer [redacted:bearer_token]."), "{out}");
        let pem = "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIEabc\n-----END RSA PRIVATE KEY-----\nb";
        assert_eq!(redact(pem), "a\n[redacted:pem_private_key]\nb");
        assert_eq!(redact_lines(pem), "a\n[redacted:pem_private_key]\n\n\nb", "its lines kept");
        assert_eq!(redact_lines(&text), out, "a span within a line");
        assert_eq!(
            redact("jwt eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2QT4f end"),
            "jwt [redacted:jwt] end"
        );
        assert_eq!(redact("random AbCdEfGhIjKlMnOpQrStUv1234 word"), "random [redacted:mixed_token] word");
    }

    #[test]
    fn redact_leaves_hashes_ids_and_identifiers() {
        let text = "commit 4f9c2e1d0b8a7c6e5f4d3c2b1a0f9e8d7c6b5a49 fixed knowledge_capture_update_is_compare_and_swap \
                    in HttpEmbedderConfig for 2c1f5a7e-1b2c-4d3e-9f8a-0b1c2d3e4f5a; 缓存设计 stays; SCHEMA_V4 too";
        assert_eq!(redact(text), text);
        // A long camelCase method with a version digit is a name, not a key.
        let call = "okMaxService.handleOrderNoticeV1(payload) and WithdrawOrderCallbackHandler2Impl";
        assert_eq!(redact(call), call);
    }

    #[test]
    fn redact_takes_a_short_password_and_a_urls_password() {
        // Synthetic samples of the shapes a design doc leaks: a JSON field, a deeplink's
        // query, a YAML default, a Chinese label and a JDBC URL.
        let text = "{\"Password\": \"Q*w@7e:[\"} and open?login=1&password=Ab%2Ac9x1 \
                    with ${DB_PASSWORD:root1234} then 密码：Zx9!kq77。and jdbc:mysql://root:s3cr3t@db:3306/app";
        let out = redact(text);
        for secret in ["Q*w@7e:[", "Ab%2Ac9x1", "root1234", "Zx9!kq77", "s3cr3t"] {
            assert!(!out.contains(secret), "{secret} left in {out}");
        }
        assert!(out.contains("{\"Password\": \"[redacted:password]\"}"), "{out}");
        assert!(out.contains("&password=[redacted:password] with"), "{out}");
        assert!(out.contains("${DB_PASSWORD:[redacted:password]}"), "{out}");
        assert!(out.contains("密码：[redacted:password]。"), "{out}");
        assert!(out.contains("mysql://root:[redacted:url_password]@db:3306"), "{out}");
        // Redaction only: a note naming a sample password is not refused for it.
        assert_eq!(scan("the sample password: Abc12345"), None);
    }

    #[test]
    fn redact_leaves_code_and_placeholders_beside_a_password() {
        for text in [
            "String password = passwordEncoder.encode(rawPassword);",
            "if (password == null || password.length() < 8) { return; }",
            "user.setPassword(request.getPassword()); pwd: Path, pwd: str",
            "password: ${DB_PASSWORD} and password=#{password} and password: <your password>",
            "\"password\": \"******\", \"pwd\": \"xxxxxxxx\", password: string(6-20), 密码：6-20位",
            "password: required, password=null, password_hash = bcrypt(raw), newPassword: rawPassword",
            "http://localhost:8080/api and redis://:${REDIS_PASSWORD}@cache:6379 and https://<user>:<pass>@host",
        ] {
            assert_eq!(redact(text), text);
        }
    }

    #[test]
    fn a_key_never_passes_for_an_identifier() {
        for key in ["LTAI5tAbCdEfGhIjKlMnOpQr", "EAAe2zgf1VYIBOwmuYjdaorEaAik", "AbcDefGhiJklMno12345PqrStu","abcDefGhiJkl9mnoPqrStuVwx"] {
            assert_eq!(redact(key), "[redacted:mixed_token]", "{key}");
        }
    }
}
