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

//! One error shape for every entry point (SPEC §memory.mcp error table): the CLI's
//! `--json`, the API and MCP all speak the same snake_case codes, so a client learns
//! one vocabulary. Extra fields (`current`, `similar`, `possibleMatch`, `rule`,
//! `projectId`) ride beside the code so a caller can act without parsing prose.

use serde_json::{Map, Value};

/// The closed set of business error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    InvalidCwd,
    ProjectAmbiguous,
    RootBound,
    NotFound,
    RevisionConflict,
    SimilarExists,
    Invalid,
    SensitiveContent,
    StoreBusy,
    StoreError,
    UnsupportedSchema,
    /// The panel's git previews: git missing, slow, failing, or no checkout.
    GitUnavailable,
    /// A domain path the project already has (SPEC §memory.domains).
    DomainExists,
    /// A memory question found no agent to answer it, or its agent failed (SPEC
    /// §memory.rag).
    AgentUnavailable,
}

impl Code {
    pub fn as_str(&self) -> &'static str {
        match self {
            Code::InvalidCwd => "memory_invalid_cwd",
            Code::ProjectAmbiguous => "memory_project_ambiguous",
            Code::RootBound => "memory_root_bound",
            Code::NotFound => "memory_not_found",
            Code::RevisionConflict => "memory_revision_conflict",
            Code::SimilarExists => "memory_similar_exists",
            Code::Invalid => "memory_invalid",
            Code::SensitiveContent => "memory_sensitive_content",
            Code::StoreBusy => "memory_store_busy",
            Code::StoreError => "memory_store_error",
            Code::UnsupportedSchema => "memory_unsupported_schema",
            Code::GitUnavailable => "memory_git_unavailable",
            Code::DomainExists => "memory_domain_exists",
            Code::AgentUnavailable => "memory_agent_unavailable",
        }
    }

    /// The HTTP status the API answers with.
    pub fn http_status(&self) -> u16 {
        match self {
            Code::InvalidCwd | Code::Invalid | Code::SensitiveContent => 400,
            Code::NotFound => 404,
            Code::ProjectAmbiguous
            | Code::RootBound
            | Code::RevisionConflict
            | Code::SimilarExists
            | Code::DomainExists => 409,
            Code::StoreBusy | Code::GitUnavailable | Code::AgentUnavailable => 503,
            Code::StoreError | Code::UnsupportedSchema => 500,
        }
    }

    /// Conflict-like codes: the CLI exits 3 on these (re-read and retry).
    pub fn is_conflict(&self) -> bool {
        matches!(
            self,
            Code::RevisionConflict | Code::SimilarExists | Code::ProjectAmbiguous | Code::RootBound | Code::DomainExists
        )
    }
}

/// One business error: a code, a human message, and typed extras.
#[derive(Debug, Clone, PartialEq)]
pub struct MemError {
    pub code: Code,
    pub message: String,
    pub extra: Map<String, Value>,
}

pub type MemResult<T> = Result<T, MemError>;

impl MemError {
    pub fn new(code: Code, message: impl Into<String>) -> MemError {
        MemError { code, message: message.into(), extra: Map::new() }
    }

    /// A field failed validation; the message names it.
    pub fn invalid(message: impl Into<String>) -> MemError {
        MemError::new(Code::Invalid, message)
    }

    pub fn not_found(message: impl Into<String>) -> MemError {
        MemError::new(Code::NotFound, message)
    }

    pub fn invalid_cwd(message: impl Into<String>) -> MemError {
        MemError::new(Code::InvalidCwd, message)
    }

    pub fn store(message: impl Into<String>) -> MemError {
        MemError::new(Code::StoreError, message)
    }

    /// Attach one extra field.
    pub fn with(mut self, key: &str, value: Value) -> MemError {
        self.extra.insert(key.to_string(), value);
        self
    }

    /// `{code, message, ...extra}` - the in-band MCP error text and the CLI's JSON.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("code".into(), Value::String(self.code.as_str().into()));
        out.insert("message".into(), Value::String(self.message.clone()));
        for (key, value) in &self.extra {
            out.insert(key.clone(), value.clone());
        }
        Value::Object(out)
    }
}

impl std::fmt::Display for MemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for MemError {}

impl From<sqlx::Error> for MemError {
    fn from(err: sqlx::Error) -> MemError {
        if is_busy(&err) {
            return MemError::new(
                Code::StoreBusy,
                "the memory store is busy (another process holds the write lock); retry",
            );
        }
        MemError::store(err.to_string())
    }
}

/// SQLITE_BUSY / SQLITE_LOCKED (any extended form), or the pool's own timeout.
fn is_busy(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::PoolTimedOut => true,
        sqlx::Error::Database(db) => {
            let primary = db.code().and_then(|c| c.parse::<i64>().ok()).map(|c| c & 0xff);
            matches!(primary, Some(5) | Some(6)) || db.message().contains("database is locked")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_map_to_statuses_and_exit_classes() {
        assert_eq!(Code::NotFound.http_status(), 404);
        assert_eq!(Code::StoreBusy.http_status(), 503);
        assert!(Code::RootBound.is_conflict());
        assert!(!Code::Invalid.is_conflict());
        let err = MemError::new(Code::RevisionConflict, "stale").with("current", Value::from(4));
        let json = err.to_json();
        assert_eq!(json["code"], "memory_revision_conflict");
        assert_eq!(json["current"], 4);
    }
}
