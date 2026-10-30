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

//! Identifier checks and per-dialect quoting: the only way a name reaches SQL text.

use super::DbDialect;

/// Identifiers reach the catalog as bound parameters, but SHOW CREATE TABLE cannot bind one —
/// `[A-Za-z0-9_$]{1,64}` vetted by hand. Returns the Node build's message as a plain String;
/// the adapters wrap it into their own fault type (same text, one allocation).
pub fn assert_ident(name: &str, what: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$');
    if ok {
        Ok(())
    } else {
        Err(format!("not a valid {what}: {name}"))
    }
}

// --- identifier quoting --------------------------------------------------------------------------

/// Quote one identifier for the dialect, after assert_ident has vetted it.
///
/// The browser interpolates table/column names into SQL (LIMIT/OFFSET paging cannot bind them),
/// so every identifier passes the same SAFE_IDENT gate the resource URIs use — then it is quoted
/// anyway. Schema-qualified names are quoted per part by qualified().
pub fn quote_ident(dialect: DbDialect, name: &str) -> Result<String, String> {
    let what = if dialect == DbDialect::Mysql {
        "MySQL identifier"
    } else {
        "Postgres identifier"
    };
    assert_ident(name, what)?;
    Ok(if dialect == DbDialect::Mysql {
        format!("`{name}`")
    } else {
        format!("\"{name}\"")
    })
}

/// A quoted, schema-qualified table reference (backticks for MySQL, double quotes for PG).
/// An empty schema is dropped the way JS's truthiness check did (`ref.schema ? s.t : t`).
pub fn qualified(dialect: DbDialect, schema: Option<&str>, table: &str) -> Result<String, String> {
    match schema.filter(|s| !s.is_empty()) {
        Some(s) => Ok(format!(
            "{}.{}",
            quote_ident(dialect, s)?,
            quote_ident(dialect, table)?
        )),
        None => quote_ident(dialect, table),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_quoting() {
        assert_eq!(quote_ident(DbDialect::Mysql, "users").unwrap(), "`users`");
        assert_eq!(quote_ident(DbDialect::Pg, "users").unwrap(), "\"users\"");
        for bad in ["users; DROP TABLE x", "a`b", "c\"d", "with space", ""] {
            assert!(
                quote_ident(DbDialect::Mysql, bad).is_err(),
                "mysql accepted {bad:?}"
            );
            assert!(
                quote_ident(DbDialect::Pg, bad).is_err(),
                "pg accepted {bad:?}"
            );
        }
    }
}
