// Copyright 2026 young1lin
// SPDX-License-Identifier: Apache-2.0

//! Vault references in an interactive agent's shell command (SPEC §llm.agents Test).
//! A `${secret://name}` reads as a shell variable. Where the shell expands a variable
//! (unquoted, inside double quotes, in an expanding here-document) it becomes one, whose value
//! the child gets in its environment: never on its command line, never through its parser,
//! never in what the model or the run record sees. Where the shell does not (single quotes, a
//! quoted here-document, an escaped `$`, a comment), it stays the reference itself, so a file
//! the command writes keeps `${secret://name}` for whatever resolves it at its own use.

use swiss_core::secure::refs::secret_refs_aside;
use swiss_core::secure::secretstore::grant_covers;

/// Which parser reads the command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dialect {
    Bash,
    PowerShell,
}

/// The variables the values travel in: `SWISS_SECRET_0`, `SWISS_SECRET_1`, ...
const VAR_PREFIX: &str = "SWISS_SECRET_";

/// What one command's references came to: the text the shell runs, the environment its child
/// gets, and the stored values its output masks.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub command: String,
    pub env: Vec<(String, String)>,
    pub masked: Vec<String>,
}

/// Turn the expanded references of `command` into variables, refusing a name `grant` does not
/// cover (whether the vault holds it or not) and a granted one the vault lacks.
pub(crate) fn prepare(command: &str, dialect: Dialect, grant: &[String]) -> Result<Prepared, String> {
    let literal = literal_chars(command, dialect);
    let gate = |name: &str| {
        if grant_covers(grant, name) {
            Ok(())
        } else {
            Err(format!("secret://{name} is not granted to this agent: use only the secrets this conversation named, or ask the user to grant it in the agent's settings"))
        }
    };
    let stand_in = |n: usize| match dialect {
        Dialect::PowerShell => format!("${{env:{VAR_PREFIX}{n}}}"),
        Dialect::Bash => format!("${{{VAR_PREFIX}{n}}}"),
    };
    let expands = |at: usize| !literal.get(at).copied().unwrap_or(false);
    let (command, aside) = secret_refs_aside(command, &expands, &gate, &stand_in)?;
    let env = aside.iter().enumerate().map(|(n, one)| (format!("{VAR_PREFIX}{n}"), one.value.clone())).collect();
    let masked = aside.into_iter().filter(|one| one.stored).map(|one| one.value).collect();
    Ok(Prepared { command, env, masked })
}

/// For each character of `command`, whether the shell reads it as literal text, where a `$`
/// starts no expansion.
fn literal_chars(command: &str, dialect: Dialect) -> Vec<bool> {
    let chars: Vec<char> = command.chars().collect();
    match dialect {
        Dialect::Bash => bash_literal(&chars),
        Dialect::PowerShell => powershell_literal(&chars),
    }
}

fn line_start(c: &[char], i: usize) -> bool {
    i == 0 || c[i - 1] == '\n'
}

fn bash_literal(c: &[char]) -> Vec<bool> {
    #[derive(PartialEq)]
    enum S { Normal, Single, Ansi, Double }
    let mut lit = vec![false; c.len()];
    let mut s = S::Normal;
    // Here-documents opened on the current line: delimiter, quoted (a literal body), `<<-`.
    let mut pending: Vec<(String, bool, bool)> = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match s {
            S::Single => {
                lit[i] = true;
                if ch == '\'' { s = S::Normal; }
                i += 1;
            }
            S::Ansi => {
                lit[i] = true;
                if ch == '\\' {
                    if i + 1 < c.len() { lit[i + 1] = true; }
                    i += 2;
                    continue;
                }
                if ch == '\'' { s = S::Normal; }
                i += 1;
            }
            S::Double => {
                if ch == '\\' && i + 1 < c.len() {
                    lit[i + 1] = c[i + 1] == '$';
                    i += 2;
                    continue;
                }
                if ch == '"' { s = S::Normal; }
                i += 1;
            }
            S::Normal => {
                if ch == '\\' {
                    if i + 1 < c.len() { lit[i + 1] = true; }
                    i += 2;
                } else if ch == '\'' {
                    lit[i] = true;
                    s = S::Single;
                    i += 1;
                } else if ch == '$' && c.get(i + 1) == Some(&'\'') {
                    lit[i] = true;
                    lit[i + 1] = true;
                    s = S::Ansi;
                    i += 2;
                } else if ch == '"' {
                    s = S::Double;
                    i += 1;
                } else if ch == '#' && (i == 0 || c[i - 1].is_whitespace() || ";&|()".contains(c[i - 1])) {
                    while i < c.len() && c[i] != '\n' {
                        lit[i] = true;
                        i += 1;
                    }
                } else if ch == '<' && c.get(i + 1) == Some(&'<') && c.get(i + 2) != Some(&'<') {
                    let mut j = i + 2;
                    let strip = c.get(j) == Some(&'-');
                    if strip { j += 1; }
                    while j < c.len() && (c[j] == ' ' || c[j] == '\t') { j += 1; }
                    let (mut word, mut quoted) = (String::new(), false);
                    while j < c.len() && !c[j].is_whitespace() && !";&|<>()".contains(c[j]) {
                        if matches!(c[j], '\'' | '"' | '\\') { quoted = true; } else { word.push(c[j]); }
                        j += 1;
                    }
                    if !word.is_empty() { pending.push((word, quoted, strip)); }
                    i = j;
                } else if ch == '\n' && !pending.is_empty() {
                    i += 1;
                    for (word, quoted, strip) in std::mem::take(&mut pending) {
                        while i < c.len() {
                            let start = i;
                            let end = (start..c.len()).find(|&k| c[k] == '\n').unwrap_or(c.len());
                            let line: String = c[start..end].iter().collect();
                            i = (end + 1).min(c.len());
                            if (if strip { line.trim_start_matches('\t') } else { line.as_str() }) == word { break; }
                            for k in start..end {
                                if quoted {
                                    lit[k] = true;
                                } else if c[k] == '\\' && k + 1 < end && c[k + 1] == '$' {
                                    lit[k + 1] = true;
                                }
                            }
                        }
                    }
                } else {
                    i += 1;
                }
            }
        }
    }
    lit
}

fn powershell_literal(c: &[char]) -> Vec<bool> {
    #[derive(PartialEq)]
    enum S { Normal, Single, Double, HereSingle, HereDouble, Block }
    let mut lit = vec![false; c.len()];
    let mut s = S::Normal;
    // `@'` or `@"` opens a here-string only when nothing but blanks follow it on its line.
    let opens_here = |i: usize| (i + 2..c.len()).find(|&k| c[k] != ' ' && c[k] != '\t').is_none_or(|k| c[k] == '\n' || c[k] == '\r');
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match s {
            S::Single => {
                lit[i] = true;
                if ch == '\'' {
                    if c.get(i + 1) == Some(&'\'') {
                        lit[i + 1] = true;
                        i += 2;
                        continue;
                    }
                    s = S::Normal;
                }
                i += 1;
            }
            S::Double => {
                if ch == '`' && i + 1 < c.len() {
                    lit[i + 1] = c[i + 1] == '$';
                    i += 2;
                    continue;
                }
                if ch == '"' {
                    if c.get(i + 1) == Some(&'"') {
                        i += 2;
                        continue;
                    }
                    s = S::Normal;
                }
                i += 1;
            }
            S::HereSingle => {
                lit[i] = true;
                if line_start(c, i) && ch == '\'' && c.get(i + 1) == Some(&'@') {
                    lit[i + 1] = true;
                    s = S::Normal;
                    i += 2;
                    continue;
                }
                i += 1;
            }
            S::HereDouble => {
                if line_start(c, i) && ch == '"' && c.get(i + 1) == Some(&'@') {
                    s = S::Normal;
                    i += 2;
                    continue;
                }
                if ch == '`' && i + 1 < c.len() {
                    lit[i + 1] = c[i + 1] == '$';
                    i += 2;
                    continue;
                }
                i += 1;
            }
            S::Block => {
                lit[i] = true;
                if ch == '#' && c.get(i + 1) == Some(&'>') {
                    lit[i + 1] = true;
                    s = S::Normal;
                    i += 2;
                    continue;
                }
                i += 1;
            }
            S::Normal => {
                if ch == '`' {
                    if i + 1 < c.len() { lit[i + 1] = true; }
                    i += 2;
                } else if ch == '@' && c.get(i + 1) == Some(&'\'') && opens_here(i) {
                    lit[i] = true;
                    s = S::HereSingle;
                    i += 2;
                } else if ch == '@' && c.get(i + 1) == Some(&'"') && opens_here(i) {
                    s = S::HereDouble;
                    i += 2;
                } else if ch == '\'' {
                    lit[i] = true;
                    s = S::Single;
                    i += 1;
                } else if ch == '"' {
                    s = S::Double;
                    i += 1;
                } else if ch == '<' && c.get(i + 1) == Some(&'#') {
                    lit[i] = true;
                    s = S::Block;
                    i += 1;
                } else if ch == '#' && (i == 0 || c[i - 1].is_whitespace() || ";|({".contains(c[i - 1])) {
                    while i < c.len() && c[i] != '\n' {
                        lit[i] = true;
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
        }
    }
    lit
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plant a vault value under a test-unique name; the vault is process-wide.
    fn plant(name: &str, value: &str) {
        let _guard = swiss_core::paths::DATA_DIR_LOCK.blocking_lock();
        let path = swiss_core::paths::test_home().join("secrets.json");
        swiss_core::secure::secretstore::inject_vault(&path);
        let rev = swiss_core::secure::secretstore::vault_rev();
        swiss_core::secure::secretstore::put_secret(&path, name, value, rev).expect("plant");
    }

    fn grant(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn bash_expands_a_reference_where_it_expands_a_variable() {
        plant("prep-bash-key", "prep-bash-value-1");
        let command = "curl -H \"Authorization: Bearer ${secret://prep-bash-key}\" x; echo ${secret://prep-bash-key}; \
                       printf '%s' '${secret://prep-bash-key}' > ref; echo \\${secret://prep-bash-key} # ${secret://prep-bash-key}\n\
                       cat <<'EOF' > a\n${secret://prep-bash-key}\nEOF\ncat <<EOF > b\nv=${secret://prep-bash-key} don't\nEOF\necho \"${secret://prep-bash-key}\"";
        let got = prepare(command, Dialect::Bash, &grant(&["prep-bash-key"])).expect("prepared");
        assert_eq!(
            got.command,
            "curl -H \"Authorization: Bearer ${SWISS_SECRET_0}\" x; echo ${SWISS_SECRET_0}; \
             printf '%s' '${secret://prep-bash-key}' > ref; echo \\${secret://prep-bash-key} # ${secret://prep-bash-key}\n\
             cat <<'EOF' > a\n${secret://prep-bash-key}\nEOF\ncat <<EOF > b\nv=${SWISS_SECRET_0} don't\nEOF\necho \"${SWISS_SECRET_0}\""
        );
        assert_eq!(got.env, [("SWISS_SECRET_0".to_string(), "prep-bash-value-1".to_string())]);
        assert_eq!(got.masked, ["prep-bash-value-1"]);
    }

    #[test]
    fn powershell_expands_a_reference_where_it_expands_a_variable() {
        plant("prep-ps-key", "prep-ps-value-1");
        let command = "Invoke-RestMethod -Headers @{ Authorization = \"Bearer ${secret://prep-ps-key}\" } x; \
                       Set-Content f '${secret://prep-ps-key}'; 'it''s ${secret://prep-ps-key}'; \"a `${secret://prep-ps-key}\" # ${secret://prep-ps-key}\n\
                       <# ${secret://prep-ps-key} #> $x = @'\n${secret://prep-ps-key} don't\n'@\n$y = @\"\nv=${secret://prep-ps-key}\n\"@\n\
                       curl.exe -u ${secret://prep-ps-key:anon} \"${secret://prep-ps-key}\"";
        let got = prepare(command, Dialect::PowerShell, &grant(&["*"])).expect("prepared");
        assert_eq!(
            got.command,
            "Invoke-RestMethod -Headers @{ Authorization = \"Bearer ${env:SWISS_SECRET_0}\" } x; \
             Set-Content f '${secret://prep-ps-key}'; 'it''s ${secret://prep-ps-key}'; \"a `${secret://prep-ps-key}\" # ${secret://prep-ps-key}\n\
             <# ${secret://prep-ps-key} #> $x = @'\n${secret://prep-ps-key} don't\n'@\n$y = @\"\nv=${env:SWISS_SECRET_0}\n\"@\n\
             curl.exe -u ${env:SWISS_SECRET_1} \"${env:SWISS_SECRET_0}\""
        );
        // The defaulted reference is its own variable; a stored value stands behind it too.
        assert_eq!(got.env.len(), 2);
        assert_eq!(got.env[0], ("SWISS_SECRET_0".to_string(), "prep-ps-value-1".to_string()));
        assert_eq!(got.masked, ["prep-ps-value-1", "prep-ps-value-1"]);
    }

    #[test]
    fn a_reference_the_grant_does_not_cover_is_refused_stored_or_not() {
        plant("prep-held", "prep-held-value");
        for (command, granted) in [
            ("echo \"${secret://prep-held}\"", grant(&[])),
            ("echo \"${secret://prep-held}\"", grant(&["other"])),
            ("echo \"${secret://prep-never}\"", grant(&[])),
        ] {
            let err = prepare(command, Dialect::Bash, &granted).unwrap_err();
            assert!(err.contains("is not granted to this agent"), "{err}");
        }
        let err = prepare("echo \"${secret://prep-never}\"", Dialect::Bash, &grant(&["prep-never"])).unwrap_err();
        assert!(err.contains("secret://prep-never") && err.contains("not in the vault"), "{err}");
        // A command without a reference needs no grant, and text in single quotes is text.
        let plain = prepare("echo '${secret://prep-never}' $HOME", Dialect::Bash, &grant(&[])).expect("text");
        assert_eq!((plain.command.as_str(), plain.env.len()), ("echo '${secret://prep-never}' $HOME", 0));
    }
}
