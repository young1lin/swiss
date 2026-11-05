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

//! Credential literals on a command line (SPEC §remote.security): a password, token or key
//! typed into a command as text instead of referenced from the vault. A remote exec refuses
//! one in its argv or cwd - an argv word shows in the far side's `ps` for the command's whole
//! life and lands in every run record - and the records mask whatever reached them anyway.
//!
//! Hand-rolled, no regex crate (ADR-007). A hit is a VALUE the scanner can bound, in one of six
//! slots:
//!
//! - an assignment `NAME=value` whose name ends in a credential word (`REDISCLI_AUTH=`,
//!   `PGPASSWORD=`, `MYSQL_PWD=`, `spring.redis.password=`, `?token=`), at a word's start or
//!   after a space or shell punctuation;
//! - a long option: `--password=value`, `--password value`, `--api-key value`;
//! - a header: `Authorization: Bearer value`, `X-Api-Key: value`;
//! - a JSON member: `"password": "value"`;
//! - a URL's password: `redis://:value@host`;
//! - a client's password flag: `redis-cli -a value`, `sshpass -p value`, `mysql -pvalue`.
//!
//! A value that is a reference (`${secret://name}`, `$VAR`, `$(cmd)`), a placeholder
//! (`<password>`, `{{token}}`), a path, the next flag, a mask (`••••••••`, `***`) or a flag word
//! (`true`, `none`) is not a literal - so a masked text scans clean and [mask] is idempotent.
//! Defence in depth, not a guarantee: a password under an innocent name passes.

/// What a masked value reads as - the glyphs the output mask and the panel print.
pub const MASK: &str = "••••••••";

/// One credential literal: where its value sits and what holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Literal {
    /// What holds the value, never the value: `REDISCLI_AUTH=`, `--password`, `redis-cli -a`,
    /// `Authorization:`, `"password":`, `a URL's password`.
    pub slot: String,
    /// The value's byte range in the scanned text.
    pub start: usize,
    pub end: usize,
}

/// The first credential literal in one text, or None.
pub fn find(text: &str) -> Option<Literal> {
    scan(text).into_iter().next()
}

/// The text with every credential literal's value replaced by [MASK]; the rest byte for byte.
pub fn mask(text: &str) -> String {
    let hits = scan(text);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for hit in hits {
        out.push_str(&text[at..hit.start]);
        out.push_str(MASK);
        at = hit.end;
    }
    out.push_str(&text[at..]);
    out
}

/// [find] over an argv: the words are scanned as the one command line they make (joined by
/// single spaces), so an option and its value in two words (`--password`, `x`) are read
/// together. Returns the index of the word holding the value and the hit, its range within
/// that word. A value never holds whitespace, so it never spans two words.
pub fn find_argv(argv: &[String]) -> Option<(usize, Literal)> {
    let (line, starts) = joined(argv);
    scan(&line).into_iter().next().map(|hit| local(&starts, hit))
}

/// [mask] over an argv, word by word, with the words read as one command line ([find_argv]).
pub fn mask_argv(argv: &[String]) -> Vec<String> {
    let (line, starts) = joined(argv);
    let mut words: Vec<String> = argv.to_vec();
    // Right to left, so an earlier range in the same word is still where it was.
    for (i, hit) in scan(&line).into_iter().map(|hit| local(&starts, hit)).rev() {
        words[i].replace_range(hit.start..hit.end, MASK);
    }
    words
}

fn joined(argv: &[String]) -> (String, Vec<usize>) {
    let mut line = String::new();
    let mut starts = Vec::with_capacity(argv.len());
    for (i, word) in argv.iter().enumerate() {
        if i > 0 {
            line.push(' ');
        }
        starts.push(line.len());
        line.push_str(word);
    }
    (line, starts)
}

fn local(starts: &[usize], hit: Literal) -> (usize, Literal) {
    let i = starts.iter().rposition(|s| *s <= hit.start).unwrap_or(0);
    let base = starts[i];
    (i, Literal { start: hit.start - base, end: hit.end - base, ..hit })
}

/// Every hit, in text order, none overlapping another.
fn scan(text: &str) -> Vec<Literal> {
    let mut hits = Vec::new();
    assignments(text, &mut hits);
    spaced_options(text, &mut hits);
    headers(text, &mut hits);
    json_members(text, &mut hits);
    url_passwords(text, &mut hits);
    client_flags(text, &mut hits);
    hits.sort_by_key(|h| (h.start, std::cmp::Reverse(h.end)));
    let mut kept: Vec<Literal> = Vec::with_capacity(hits.len());
    for hit in hits {
        if kept.last().is_none_or(|last| hit.start >= last.end) {
            kept.push(hit);
        }
    }
    kept
}

/// Where a name may start: the text's start, whitespace, or shell or query punctuation.
fn boundary(bytes: &[u8], at: usize) -> bool {
    at == 0 || b" \t\r\n;&|({\"'`?,=".contains(&bytes[at - 1])
}

fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"_.-".contains(&b)
}

/// The name's words, upper-cased: split at `_`, `.` and `-` and where a lower-case letter or
/// a digit meets a capital (`apiKey`, `accessToken`).
fn words(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if matches!(c, '_' | '.' | '-') {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        cur.push(c.to_ascii_uppercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Whether a name holds a credential: its LAST word is one (`DB_PASSWORD` yes,
/// `DB_PASSWORD_FILE` no - that holds a path; `--no-auth-warning` no). `PWD`, the working
/// directory, is not.
pub fn credential_name(name: &str) -> bool {
    if name.eq_ignore_ascii_case("PWD") || name.eq_ignore_ascii_case("OLDPWD") {
        return false;
    }
    let words = words(name);
    let Some(last) = words.last() else {
        return false;
    };
    if last.contains("PASSWORD") || last.contains("PASSWD") {
        return true;
    }
    if matches!(
        last.as_str(),
        "PASS"
            | "PWD"
            | "PASSPHRASE"
            | "SECRET"
            | "TOKEN"
            | "AUTH"
            | "APIKEY"
            | "CREDENTIAL"
            | "CREDENTIALS"
            // redis-server's own: `--requirepass`, `--masterauth`.
            | "REQUIREPASS"
            | "MASTERAUTH"
    ) {
        return true;
    }
    last == "KEY"
        && words.len() >= 2
        && matches!(
            words[words.len() - 2].as_str(),
            "API" | "ACCESS" | "SECRET" | "PRIVATE" | "AUTH" | "CLIENT" | "MASTER" | "SIGNING"
        )
}

/// Words a flag takes that are no password.
const FLAG_WORDS: [&str; 13] = [
    "true", "false", "yes", "no", "on", "off", "none", "null", "nil", "empty", "unset", "required", "prompt",
];

/// Whether a bounded value is typed-in text rather than a reference, placeholder, path, flag,
/// mask or flag word ([module docs](self)). `min` is the shortest value counted.
pub fn typed_in(value: &str, min: usize) -> bool {
    let Some(first) = value.chars().next() else {
        return false;
    };
    if value.chars().count() < min || value.chars().all(|c| c == first) {
        return false;
    }
    if first == '$' || value.contains("${") || value.contains("$(") {
        return false;
    }
    if (first == '<' && value.ends_with('>')) || value.starts_with("{{") || value.starts_with('-') {
        return false;
    }
    if ["/", "~/", "./", "../"].iter().any(|p| value.starts_with(p)) {
        return false;
    }
    !FLAG_WORDS.iter().any(|w| value.eq_ignore_ascii_case(w))
}

/// The value at `at`: inside quotes when it opens with one that closes before any whitespace,
/// else the bare run up to whitespace or shell punctuation. None when there is no literal.
fn value_at(text: &str, at: usize, min: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let (start, end) = match *bytes.get(at)? {
        q @ (b'"' | b'\'') => {
            let body = &bytes[at + 1..];
            let close = body.iter().position(|b| *b == q || b.is_ascii_whitespace())?;
            if body[close] != q {
                return None;
            }
            (at + 1, at + 1 + close)
        }
        _ => {
            let run = bytes[at..]
                .iter()
                .take_while(|b| !b.is_ascii_whitespace() && !b";&|()<>\"'`".contains(b))
                .count();
            (at, at + run)
        }
    };
    typed_in(&text[start..end], min).then_some((start, end))
}

fn hit(slot: String, (start, end): (usize, usize)) -> Literal {
    Literal { slot, start, end }
}

/// `NAME=value` and `--name=value`: each `=` with a credential name standing before it.
fn assignments(text: &str, out: &mut Vec<Literal>) {
    let bytes = text.as_bytes();
    for eq in (0..bytes.len()).filter(|i| bytes[*i] == b'=') {
        if bytes.get(eq + 1) == Some(&b'=') {
            continue;
        }
        let from = (0..eq).rev().take_while(|i| name_byte(bytes[*i])).last().unwrap_or(eq);
        if from == eq || !boundary(bytes, from) {
            continue;
        }
        let raw = &text[from..eq];
        // An option's name drops its dashes: `--password`, Go's `-password`, Java's
        // `-Dspring.redis.password` (only the last word counts, so the D never matters).
        let option = raw.strip_prefix("--").or_else(|| raw.strip_prefix('-'));
        let (name, slot) = match option {
            Some(name) if name.starts_with(|c: char| c.is_ascii_alphanumeric()) => (name, raw.to_string()),
            Some(_) => continue,
            None if raw.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') => (raw, format!("{raw}=")),
            None => continue,
        };
        if credential_name(name) {
            if let Some(range) = value_at(text, eq + 1, 4) {
                out.push(hit(slot, range));
            }
        }
    }
}

/// `--name value`: a credential long option whose value is the next word.
fn spaced_options(text: &str, out: &mut Vec<Literal>) {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(at) = text[from..].find("--").map(|i| i + from) {
        from = at + 2;
        if !boundary(bytes, at) {
            continue;
        }
        let name_end = at + 2 + bytes[at + 2..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b == b'-').count();
        let name = &text[at + 2..name_end];
        let gap = bytes[name_end..].iter().take_while(|b| **b == b' ' || **b == b'\t').count();
        if gap == 0 || !name.starts_with(|c: char| c.is_ascii_alphanumeric()) || !credential_name(name) {
            continue;
        }
        if let Some(range) = value_at(text, name_end + gap, 4) {
            out.push(hit(format!("--{name}"), range));
        }
    }
}

/// `Authorization: <scheme> value` and a hyphenated credential header, `X-Api-Key: value`.
/// A bare `token: ...` is prose as often as not (a commit message) and is left alone.
fn headers(text: &str, out: &mut Vec<Literal>) {
    let bytes = text.as_bytes();
    for colon in (0..bytes.len()).filter(|i| bytes[*i] == b':') {
        let from = (0..colon)
            .rev()
            .take_while(|i| bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'-')
            .last()
            .unwrap_or(colon);
        if from == colon || !boundary(bytes, from) {
            continue;
        }
        let name = &text[from..colon];
        let authorization = name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("proxy-authorization");
        if !authorization && !(name.contains('-') && credential_name(name)) {
            continue;
        }
        let mut at = colon + 1;
        at += bytes[at..].iter().take_while(|b| **b == b' ' || **b == b'\t').count();
        if authorization {
            // The scheme word (Bearer, Basic, Token) and its space come before the credential.
            let word = bytes[at..].iter().take_while(|b| b.is_ascii_alphabetic()).count();
            if word > 0 && bytes.get(at + word) == Some(&b' ') {
                at += word + 1;
            }
        }
        if let Some(range) = value_at(text, at, 4) {
            out.push(hit(format!("{name}:"), range));
        }
    }
}

/// `"name": "value"` with a credential name.
fn json_members(text: &str, out: &mut Vec<Literal>) {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(open) = text[from..].find('"').map(|i| i + from) {
        from = open + 1;
        let name_end = open + 1 + bytes[open + 1..].iter().take_while(|b| name_byte(**b)).count();
        if bytes.get(name_end) != Some(&b'"') || name_end == open + 1 {
            continue;
        }
        let name = &text[open + 1..name_end];
        let mut at = name_end + 1;
        at += bytes[at..].iter().take_while(|b| b.is_ascii_whitespace()).count();
        if bytes.get(at) != Some(&b':') {
            continue;
        }
        at += 1;
        at += bytes[at..].iter().take_while(|b| b.is_ascii_whitespace()).count();
        if bytes.get(at) == Some(&b'"') && credential_name(name) {
            if let Some(range) = value_at(text, at, 4) {
                out.push(hit(format!("\"{name}\":"), range));
            }
        }
        from = name_end + 1;
    }
}

/// `scheme://user:password@`: the password, however short.
fn url_passwords(text: &str, out: &mut Vec<Literal>) {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(at) = text[from..].find("://").map(|i| i + from) {
        from = at + 3;
        let scheme = (0..at)
            .rev()
            .take_while(|i| bytes[*i].is_ascii_alphanumeric() || b"+.-".contains(&bytes[*i]))
            .last();
        if scheme.is_none_or(|s| !bytes[s].is_ascii_alphabetic()) {
            continue;
        }
        let stop = |b: &u8| b.is_ascii_whitespace() || b"\"'`".contains(b);
        let user = bytes[from..].iter().take_while(|b| !stop(b) && !b":@/".contains(b)).count();
        let colon = from + user;
        if bytes.get(colon) != Some(&b':') {
            continue;
        }
        let run = bytes[colon + 1..].iter().take_while(|b| !stop(b) && !b"@/".contains(b)).count();
        let end = colon + 1 + run;
        if bytes.get(end) != Some(&b'@') {
            continue;
        }
        if typed_in(&text[colon + 1..end], 1) {
            out.push(hit("a URL's password".into(), (colon + 1, end)));
        }
    }
}

/// How a client takes its password on the command line.
enum Takes {
    /// The flag, then the password as the next word (`redis-cli -a pw`).
    Next(&'static str),
    /// The password glued to the flag (`mysql -ppw`).
    Glued(&'static str),
}

const CLIENTS: [(&str, Takes); 6] = [
    ("redis-cli", Takes::Next("-a")),
    ("sshpass", Takes::Next("-p")),
    ("mysql", Takes::Glued("-p")),
    ("mysqldump", Takes::Glued("-p")),
    ("mysqladmin", Takes::Glued("-p")),
    ("mariadb", Takes::Glued("-p")),
];

/// A client's own password flag, read among that client's words up to the end of its command
/// (`;`, `|`, `&`, `)` or a line break) - so `ls -a` is never `redis-cli -a`.
fn client_flags(text: &str, out: &mut Vec<Literal>) {
    let bytes = text.as_bytes();
    let token_end = |at: usize| at + bytes[at..].iter().take_while(|b| !b.is_ascii_whitespace() && !b";&|)".contains(b)).count();
    for (client, takes) in &CLIENTS {
        let mut from = 0;
        while let Some(at) = text[from..].find(client).map(|i| i + from) {
            from = at + client.len();
            let ends = bytes.get(from).is_none_or(|b| *b == b' ' || *b == b'\t');
            // A path before the name (`/usr/bin/redis-cli`) still names the client.
            if !ends || !(boundary(bytes, at) || bytes[at - 1] == b'/') {
                continue;
            }
            let mut at = from;
            loop {
                at += bytes[at..].iter().take_while(|b| **b == b' ' || **b == b'\t').count();
                if at >= bytes.len() || b";&|)\r\n".contains(&bytes[at]) {
                    break;
                }
                let end = token_end(at);
                let token = &text[at..end];
                match takes {
                    Takes::Next(flag) if token == *flag => {
                        let next = end + bytes[end..].iter().take_while(|b| **b == b' ' || **b == b'\t').count();
                        if let Some(range) = value_at(text, next, 4) {
                            out.push(hit(format!("{client} {flag}"), range));
                        }
                    }
                    Takes::Glued(flag) if token.len() > flag.len() && token.starts_with(flag) => {
                        if let Some(range) = value_at(text, at + flag.len(), 4) {
                            out.push(hit(format!("{client} {flag}"), range));
                        }
                    }
                    _ => {}
                }
                at = end.max(at + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(text: &str) -> Option<String> {
        find(text).map(|hit| hit.slot)
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn each_slot_finds_its_literal() {
        // Synthetic values throughout; none is a real credential.
        let cases = [
            ("sudo -n docker exec -e REDISCLI_AUTH=Xq7pLm2Rv9 redis redis-cli ping", "REDISCLI_AUTH="),
            ("PGPASSWORD=hunter2x psql -h db", "PGPASSWORD="),
            ("MYSQL_PWD=s3cr3t-pw mysql", "MYSQL_PWD="),
            ("java -Dspring.redis.password=abcd1234 -jar app.jar", "-Dspring.redis.password"),
            ("./server -db-password=abcd1234", "-db-password"),
            ("curl 'https://api.local/v1?token=tk_0123456789'", "token="),
            ("helm upgrade app . --set auth.password=Zz99yy88", "auth.password="),
            ("app --password=pa55word", "--password"),
            ("app --api-key k-0123456789", "--api-key"),
            ("curl -H 'Authorization: Bearer abcdef0123456789' https://x", "Authorization:"),
            ("curl -H 'X-Api-Key: 0123abcd4567' https://x", "X-Api-Key:"),
            ("curl -d '{\"user\":\"a\",\"password\":\"pa55word\"}' https://x", "\"password\":"),
            ("curl -d '{\"apiKey\": \"k0123456789\"}' https://x", "\"apiKey\":"),
            ("redis-cli -u redis://:p4ssw0rd@cache:6379 ping", "a URL's password"),
            ("psql postgres://app:pw@db/app", "a URL's password"),
            ("redis-cli -h cache -a p4ssw0rd ping", "redis-cli -a"),
            ("/usr/bin/redis-cli -a p4ssw0rd ping", "redis-cli -a"),
            ("sshpass -p p4ssw0rd ssh host", "sshpass -p"),
            ("mysqldump -uroot -pp4ssw0rd app", "mysqldump -p"),
            ("export GITHUB_TOKEN=ghp_notreal123 && gh pr list", "GITHUB_TOKEN="),
            ("AWS_SECRET_ACCESS_KEY=abcd/efgh+1234 aws s3 ls", "AWS_SECRET_ACCESS_KEY="),
            ("docker run -d redis redis-server --requirepass p4ssw0rd", "--requirepass"),
        ];
        for (text, want) in cases {
            assert_eq!(slot(text).as_deref(), Some(want), "{text}");
        }
    }

    #[test]
    fn ordinary_command_lines_pass() {
        for text in [
            "redis-cli --no-auth-warning INFO keyspace",
            "sudo -n docker exec -e REDISCLI_AUTH redis redis-cli ping",
            "sudo -n --preserve-env=REDISCLI_AUTH docker exec -e REDISCLI_AUTH r redis-cli ping",
            "REDISCLI_AUTH=${secret://redis-password} redis-cli ping",
            "PGPASSWORD=\"$PW\" psql",
            "app --password \"$(cat /run/secrets/pw)\"",
            "DB_PASSWORD_FILE=/run/secrets/db app",
            "app --password-file /etc/app/pw",
            "app --private-key ~/.ssh/id_ed25519",
            "grep -n \"password=\" app.properties",
            "grep -rn 'token:' conf/",
            "sed -i 's/password=old/password=new/' app.conf",
            "git commit -m \"fix token: handle expiry\"",
            "ls -a /srv && mysql -p",
            "cd $PWD && PWD=/srv/app make",
            "NO_AUTH=true TOKENIZERS_PARALLELISM=false python3 train.py",
            "GIT_AUTHOR_NAME=bot SSH_AUTH_SOCK=/tmp/agent.sock git push",
            "curl https://host:8443/health http://localhost:8080/x",
            "curl -H 'Authorization: Bearer ${secret://api-token}' https://x",
            "echo REDISCLI_AUTH=•••••••• && app --token=••••••••",
            "app --password=<your-password> --token={{token}} --secret=***",
            "if [ \"$A\" == \"$B\" ]; then echo same; fi",
            "systemctl status mm-proxy; journalctl -u mm-proxy --since today",
        ] {
            assert_eq!(find(text), None, "{text}");
        }
    }

    #[test]
    fn mask_replaces_values_only_and_is_idempotent() {
        let text = "R(){ sudo -n docker exec -e REDISCLI_AUTH=Xq7pLm2Rv9 r redis-cli \"$@\"; }; redis-cli -u redis://:p4ssw0rd@c -a p4ssw0rd INFO";
        let once = mask(text);
        assert_eq!(
            once,
            "R(){ sudo -n docker exec -e REDISCLI_AUTH=•••••••• r redis-cli \"$@\"; }; redis-cli -u redis://:••••••••@c -a •••••••• INFO"
        );
        assert_eq!(mask(&once), once, "a masked text scans clean");
        assert_eq!(mask("make -j8"), "make -j8");
        // A quoted value keeps its quotes.
        assert_eq!(mask("curl -d '{\"password\":\"pa55word\"}'"), "curl -d '{\"password\":\"••••••••\"}'");
    }

    #[test]
    fn an_argv_is_read_as_one_command_line() {
        let words = argv(&["app", "--password", "pa55word", "--verbose"]);
        let (i, hit) = find_argv(&words).expect("the value in the next word");
        assert_eq!((i, hit.slot.as_str(), &words[i][hit.start..hit.end]), (2, "--password", "pa55word"));
        assert_eq!(mask_argv(&words), argv(&["app", "--password", "••••••••", "--verbose"]));
        let words = argv(&["redis-cli", "-a", "p4ssw0rd", "ping"]);
        assert_eq!(find_argv(&words).map(|(i, _)| i), Some(2));
        // A script word: the hit's range is within that word.
        let words = argv(&["bash", "-c", "A=1; X_AUTH=Xq7pLm2Rv9 run; Y_TOKEN=tk012345 run"]);
        let (i, hit) = find_argv(&words).expect("inside the script");
        assert_eq!((i, &words[i][hit.start..hit.end]), (2, "Xq7pLm2Rv9"));
        assert_eq!(mask_argv(&words), argv(&["bash", "-c", "A=1; X_AUTH=•••••••• run; Y_TOKEN=•••••••• run"]));
        assert_eq!(find_argv(&argv(&["ls", "-a"])), None);
        assert_eq!(mask_argv(&argv(&["ls", "-la"])), argv(&["ls", "-la"]));
    }

    #[test]
    fn a_name_is_a_credential_by_its_last_word() {
        for name in ["REDISCLI_AUTH", "PGPASSWORD", "db.password", "apiKey", "clientSecret", "x-auth-token", "MYSQL_PWD", "AWS_SECRET_ACCESS_KEY"] {
            assert!(credential_name(name), "{name}");
        }
        for name in ["PWD", "DB_PASSWORD_FILE", "no-auth-warning", "AWS_ACCESS_KEY_ID", "TOKENIZERS_PARALLELISM", "GIT_AUTHOR_NAME", "sort-key", "askpass"] {
            assert!(!credential_name(name), "{name}");
        }
    }
}
