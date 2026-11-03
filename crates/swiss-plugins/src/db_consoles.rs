//! Server-owned DB Agent consoles. No resident SQL cache: records are sealed on demand,
//! atomically replaced, and compared by revision before any browser or agent mutation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use axum::body::Bytes;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Extension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use swiss_core::secure::statefile::{read_secure_json, write_secure_json};
use swiss_host::reply::{admin_error, admin_json};
use swiss_host::services::RuntimeServices;

const TEXT_MAX: usize = 2_000_000;
const PATH_COMPONENT: usize = 200;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Console {
    pub id: String,
    pub tab: String,
    pub agent: bool,
    pub agent_tab: Option<String>,
    pub open: bool,
    pub sql: String,
    pub revision: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SaveMode {
    /// The agent's own path: a record it does not have yet is created, one it has is compared.
    #[default]
    Auto,
    /// Legacy browser drafts and a page's first save of a new console: never replaces a record.
    Create,
    /// A page's later save: a record removed elsewhere is refused rather than recreated.
    Update,
}

pub struct ConsoleStore { root: PathBuf, write: Mutex<()> }

// Encoding every byte keeps connection names and legacy IDs out of the path grammar.
fn encoded(text: &str) -> String { text.as_bytes().iter().map(|b| format!("{b:02x}")).collect() }

impl ConsoleStore {
    pub fn at(root: PathBuf) -> Arc<Self> { Arc::new(Self { root, write: Mutex::new(()) }) }
    fn legacy_dir(&self, conn: &str) -> PathBuf { self.root.join(encoded(conn)) }
    fn legacy_path(&self, conn: &str, id: &str) -> PathBuf { self.legacy_dir(conn).join(format!("{}.json", encoded(id))) }
    fn dir(&self, conn: &str) -> PathBuf {
        let name = encoded(conn);
        if name.len() <= PATH_COMPONENT { return self.root.join(name); }
        let mut dir = self.root.join("~");
        for part in name.as_bytes().chunks(PATH_COMPONENT) {
            dir.push(std::str::from_utf8(part).expect("hex is ASCII"));
        }
        dir.join("@")
    }
    fn path(&self, conn: &str, id: &str) -> PathBuf {
        let name = encoded(id);
        if name.len() <= PATH_COMPONENT { return self.dir(conn).join(format!("{name}.json")); }
        let mut parts = name.as_bytes().chunks(PATH_COMPONENT).peekable();
        let mut path = self.dir(conn).join("~");
        while let Some(part) = parts.next() {
            let part = std::str::from_utf8(part).expect("hex is ASCII");
            path.push(if parts.peek().is_some() { part.to_string() } else { format!("{part}.json") });
        }
        path
    }
    fn read(path: &Path) -> Result<Option<Console>, String> {
        read_secure_json(path)?.map(|v| serde_json::from_value(v).map_err(|e| e.to_string())).transpose()
    }
    pub fn list(&self, conn: &str) -> Result<Vec<Console>, String> {
        let _guard = self.write.lock().map_err(|_| "console store lock failed")?;
        self.list_unlocked(conn)
    }
    fn list_unlocked(&self, conn: &str) -> Result<Vec<Console>, String> {
        let mut out = Vec::new();
        fn collect(dir: &Path, out: &mut Vec<Console>) -> Result<(), String> {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e.to_string()),
            };
            for entry in entries {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                    collect(&path, out)?;
                } else if path.extension().is_some_and(|ext| ext == "json") {
                    if let Some(console) = ConsoleStore::read(&path)? { out.push(console); }
                }
            }
            Ok(())
        }
        let dir = self.dir(conn);
        collect(&dir, &mut out)?;
        let legacy = self.legacy_dir(conn);
        if legacy != dir && legacy.exists() { collect(&legacy, &mut out)?; }
        let mut seen = std::collections::HashSet::new();
        if out.iter().any(|console| !seen.insert(&console.id)) {
            return Err("duplicate console identity in storage".into());
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }
    /// Delete one record. A page that saw a revision must prove it: deleting a console another
    /// page has edited since would discard that edit, so the stale request is refused.
    pub fn remove(&self, conn: &str, id: &str, revision: Option<u64>) -> Result<bool, String> {
        let _guard = self.write.lock().map_err(|_| "console store lock failed")?;
        let legacy = self.legacy_path(conn, id);
        let path = if legacy.exists() { legacy } else { self.path(conn, id) };
        let Some(before) = Self::read(&path)? else { return Ok(false); };
        if revision.is_some_and(|revision| revision != before.revision) {
            return Err("conflict: console changed; reload before deleting".into());
        }
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        Ok(true)
    }
    /// Forget the connection's closed consoles (the menu's Clear). Open consoles are work in
    /// progress and stay; the count says how many were removed.
    pub fn clear_closed(&self, conn: &str) -> Result<usize, String> {
        let _guard = self.write.lock().map_err(|_| "console store lock failed")?;
        let closed: Vec<String> = self.list_unlocked(conn)?.into_iter().filter(|c| !c.open).map(|c| c.id).collect();
        for id in &closed {
            let legacy = self.legacy_path(conn, id);
            let path = if legacy.exists() { legacy } else { self.path(conn, id) };
            match std::fs::remove_file(&path) {
                Ok(()) => {},
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(closed.len())
    }
    /// Auto-mode save. The page names Create or Update; tests use this for the default path.
    #[cfg(test)]
    pub fn save(&self, conn: &str, console: Console) -> Result<Console, String> {
        self.put(conn, console, SaveMode::Auto)
    }

    pub fn put(&self, conn: &str, mut console: Console, mode: SaveMode) -> Result<Console, String> {
        if conn.is_empty() || conn.len() > 256 || console.id.is_empty() || console.id.len() > 160
            || console.id.chars().any(char::is_control) || console.tab.chars().count() > 500
            || console.agent_tab.as_ref().is_some_and(|name| name.chars().count() > 500)
            || console.sql.len() > TEXT_MAX {
            return Err("console identity, title or text exceeds its limit".into());
        }
        let _guard = self.write.lock().map_err(|_| "console store lock failed")?;
        let legacy = self.legacy_path(conn, &console.id);
        let path = if legacy.exists() { legacy } else { self.path(conn, &console.id) };
        let before = Self::read(&path)?;
        match (mode, before.is_some()) {
            (SaveMode::Create, true) => return Err("conflict: console already exists; kept the server record".into()),
            (SaveMode::Update, false) => return Err("conflict: console was removed; kept the local draft".into()),
            _ => {},
        }
        if before.as_ref().map_or(0, |v| v.revision) != console.revision {
            return Err("conflict: console changed; reload the server record before saving".into());
        }
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(before) = before {
            console.created_at = before.created_at;
            console.updated_at = if before.sql != console.sql || before.tab != console.tab { now } else { before.updated_at };
            console.closed_at = if console.open { None } else { before.closed_at.or(Some(now)) };
        } else {
            if console.created_at <= 0 { console.created_at = now; }
            if console.updated_at <= 0 { console.updated_at = now; }
            console.closed_at = (!console.open).then_some(now);
        }
        console.revision = console.revision.checked_add(1).ok_or("console revision exhausted")?;
        std::fs::create_dir_all(path.parent().ok_or("console path has no parent")?).map_err(|e| e.to_string())?;
        write_secure_json(&path, &serde_json::to_value(&console).map_err(|e| e.to_string())?)?;
        Ok(console)
    }
}

#[derive(Deserialize)]
pub struct Listing { conn: String, id: Option<String>, revision: Option<u64>, closed: Option<bool> }

pub async fn list(Extension(store): Extension<Arc<ConsoleStore>>, Extension(services): Extension<Arc<RuntimeServices>>, Query(query): Query<Listing>) -> Response {
    if let Err(e) = services.catalog.lease(&query.conn, "db-consoles") { return admin_error(StatusCode::BAD_REQUEST, &e.to_string()); }
    match store.list(&query.conn) {
        Ok(consoles) => admin_json(StatusCode::OK, json!({ "consoles": consoles })),
        Err(e) => admin_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

pub async fn save(Extension(store): Extension<Arc<ConsoleStore>>, Extension(services): Extension<Arc<RuntimeServices>>, bytes: Bytes) -> Response {
    let body = match crate::llm_api::body_of(&bytes, &["conn", "console", "mode"]) { Ok(body) => body, Err(e) => return crate::llm_api::bad(e) };
    let Some(conn) = body.get("conn").and_then(Value::as_str) else { return crate::llm_api::bad("conn is required"); };
    if let Err(e) = services.catalog.lease(conn, "db-consoles") { return crate::llm_api::bad(e.to_string()); }
    let mode = match body.get("mode") {
        None | Some(Value::Null) => SaveMode::Auto,
        Some(value) => match serde_json::from_value::<SaveMode>(value.clone()) {
            Ok(mode) => mode,
            Err(_) => return crate::llm_api::bad("mode must be auto, create or update"),
        },
    };
    let console = match body.get("console").cloned().map(serde_json::from_value::<Console>) {
        Some(Ok(console)) => console,
        _ => return crate::llm_api::bad("console record is required"),
    };
    match store.put(conn, console, mode) {
        Ok(console) => admin_json(StatusCode::OK, json!({ "console": console })),
        Err(e) => admin_error(if e.starts_with("conflict:") { StatusCode::CONFLICT } else { StatusCode::BAD_REQUEST }, &e),
    }
}

/// DELETE answers the history's Clear (`closed=true`), one record by id, or a single record
/// proven by the revision the page read. An absent record is already gone, not an error.
pub async fn remove(Extension(store): Extension<Arc<ConsoleStore>>, Extension(services): Extension<Arc<RuntimeServices>>, Query(query): Query<Listing>) -> Response {
    if let Err(e) = services.catalog.lease(&query.conn, "db-consoles") { return admin_error(StatusCode::BAD_REQUEST, &e.to_string()); }
    if query.closed == Some(true) {
        return match store.clear_closed(&query.conn) {
            Ok(cleared) => admin_json(StatusCode::OK, json!({ "cleared": cleared })),
            Err(e) => admin_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
        };
    }
    let Some(id) = query.id.as_deref().filter(|id| !id.is_empty()) else { return crate::llm_api::bad("id is required unless closed=true") };
    match store.remove(&query.conn, id, query.revision) {
        Ok(removed) => admin_json(StatusCode::OK, json!({ "removed": removed })),
        Err(e) => admin_error(StatusCode::CONFLICT, &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sealed_consoles_survive_store_restart_with_identity_close_state_and_cas() {
        let root = std::env::temp_dir().join(format!("swiss-console-{}", swiss_core::util::random_hex(8)));
        let store = ConsoleStore::at(root.clone());
        assert!(store.list("../connection").expect("empty").is_empty());
        assert!(!root.exists(), "reads stay lazy");
        let original = Console { id: "../stable".into(), tab: "Report".into(), sql: "select 1\n\n-- draft\nselect 2".into(), agent: true, open: true, ..Console::default() };
        let mut saved = store.save("../connection", original.clone()).expect("save");
        assert!(!std::fs::read_to_string(store.path("../connection", &saved.id)).expect("sealed").contains("select 1"));
        drop(store);
        let store = ConsoleStore::at(root.clone());
        assert_eq!(store.list("../connection").expect("restored")[0].sql, original.sql);
        assert!(store.list("other").expect("other").is_empty());
        assert!(store.save("../connection", original).expect_err("stale").starts_with("conflict:"));
        let created = saved.created_at;
        saved.open = false;
        saved.tab = "Renamed".into();
        let closed = store.save("../connection", saved).expect("close");
        assert_eq!(closed.created_at, created);
        assert!(closed.closed_at.is_some());
        assert!(!ConsoleStore::at(root.clone()).list("../connection").expect("restart")[0].open);
        std::fs::remove_dir_all(root).expect("cleanup scratch");
    }

    #[test]
    fn long_valid_connection_and_console_ids_survive_restart() {
        let root = std::env::temp_dir().join(format!("swiss-console-long-{}", swiss_core::util::random_hex(8)));
        let conn = "connection".repeat(20);
        let id = "console".repeat(20);
        let store = ConsoleStore::at(root.clone());
        let saved = store.save(&conn, Console { id: id.clone(), tab: "Long ID".into(), agent: true, open: true, sql: "SELECT 1".into(), ..Console::default() }).expect("valid long identifiers are storable");
        drop(store);
        let found = ConsoleStore::at(root.clone()).list(&conn).expect("reopen");
        assert_eq!(found.len(), 1);
        assert_eq!((&found[0].id, &found[0].sql, found[0].revision), (&id, &saved.sql, saved.revision));
        let other_conn = format!("{}other", conn);
        let other_id = format!("{}other", id);
        let other = ConsoleStore::at(root.clone());
        other.save(&conn, Console { id: other_id.clone(), tab: "Second".into(), agent: true, sql: "SELECT 2".into(), ..Console::default() }).expect("same connection, another long id");
        other.save(&other_conn, Console { id: id.clone(), tab: "Other".into(), agent: true, sql: "SELECT 3".into(), ..Console::default() }).expect("another long connection");
        let ids: std::collections::HashSet<_> = other.list(&conn).expect("first connection").into_iter().map(|c| c.id).collect();
        assert_eq!(ids, [id.clone(), other_id].into_iter().collect());
        assert_eq!(other.list(&other_conn).expect("other connection")[0].sql, "SELECT 3");
        std::fs::remove_dir_all(root).expect("cleanup scratch");
    }

    #[test]
    fn legacy_import_is_create_only_and_a_removed_record_is_not_recreated() {
        let root = std::env::temp_dir().join(format!("swiss-console-mode-{}", swiss_core::util::random_hex(8)));
        let store = ConsoleStore::at(root.clone());
        let record = Console { id: "one".into(), tab: "Report".into(), agent: false, open: true, sql: "SELECT 1".into(), ..Console::default() };
        let saved = store.put("mm", record.clone(), SaveMode::Create).expect("first import creates");
        assert_eq!(saved.revision, 1);
        let replacement = Console { sql: "SELECT 2".into(), revision: 0, ..record.clone() };
        let refused = store.put("mm", replacement, SaveMode::Create).expect_err("an existing record is never replaced");
        assert!(refused.starts_with("conflict:"));
        assert_eq!(store.list("mm").expect("kept the server record")[0].sql, "SELECT 1");
        assert!(store.remove("mm", "one", Some(saved.revision + 1)).expect_err("stale delete").starts_with("conflict:"));
        assert!(store.remove("mm", "one", Some(saved.revision)).expect("delete at the seen revision"));
        assert!(store.list("mm").expect("empty").is_empty());
        let late = Console { revision: 0, ..record };
        assert!(store.put("mm", late, SaveMode::Update).expect_err("a removed record is not recreated").starts_with("conflict:"));
        std::fs::remove_dir_all(root).expect("cleanup scratch");
    }

    #[test]
    fn clearing_history_drops_closed_consoles_only() {
        let root = std::env::temp_dir().join(format!("swiss-console-clear-{}", swiss_core::util::random_hex(8)));
        let store = ConsoleStore::at(root.clone());
        store.save("mm", Console { id: "open".into(), tab: "Open".into(), open: true, sql: "SELECT 1".into(), ..Console::default() }).expect("open");
        store.save("mm", Console { id: "closed".into(), tab: "Closed".into(), open: false, sql: "SELECT 2".into(), ..Console::default() }).expect("closed");
        assert_eq!(store.clear_closed("mm").expect("clear"), 1);
        let left = store.list("mm").expect("left");
        assert_eq!(left.len(), 1);
        assert_eq!((left[0].id.as_str(), left[0].open), ("open", true));
        assert_eq!(store.clear_closed("mm").expect("already clear"), 0);
        std::fs::remove_dir_all(root).expect("cleanup scratch");
    }
}
