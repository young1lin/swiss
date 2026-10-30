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

//! The Data agent's memory (SPEC §data.agent Memory): what earlier conversations on one
//! connection learned about where its data lives and how to reach it - the live room's bound
//! user sits in live_room.anchor_uid, then users by id - so the next one walks the route
//! instead of finding it again. Not swiss memory: no graph, no embedding, no sync - a
//! folder of markdown per connection under the home (`llm/db-memory/<connection>/`), one
//! note a file (`<name>.md`, its name and one-line description in a front matter) and
//! `MEMORY.md` the index swiss rewrites from them on every change.
//!
//! A new conversation's opening carries the index (DbMemory::prompt_section); the agent
//! reads a note whole, saves one and deletes one through its tools (llm_db's read_memory,
//! save_memory, delete_memory), and the pane's Memory view lists, edits and deletes them
//! through the routes below.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use axum::body::Bytes;
use axum::extract::Query;
use axum::response::Response;
use serde_json::{json, Value};

use crate::llm_api::{bad, body_of, field, ok};

/// The longest note name: a short kebab-case slug.
const NAME_MAX: usize = 64;
/// The longest description: one line the index carries.
const DESCRIPTION_MAX: usize = 200;
/// The longest note: a route and its pitfalls, not a dump.
pub(crate) const BODY_MAX: usize = 8_000;
/// The most notes one connection keeps.
const NOTES_MAX: usize = 100;
/// How much of the index a conversation's opening carries; past it the agent lists the rest.
const INDEX_IN_PROMPT: usize = 6_000;
/// The longest connection name a route takes.
const CONN_MAX: usize = 200;
const INDEX_FILE: &str = "MEMORY.md";

/// One write at a time across every connection: a save and its index rewrite land together.
static WRITES: Mutex<()> = Mutex::new(());

/// A note as the index lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteHead {
    pub name: String,
    pub description: String,
    /// When it was last saved (RFC 3339), empty when the file says nothing.
    pub updated: String,
    /// The body's length in characters.
    pub chars: usize,
}

/// A note whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub head: NoteHead,
    pub body: String,
}

/// One connection's notes, in their folder.
#[derive(Debug, Clone)]
pub struct DbMemory {
    dir: PathBuf,
    conn: String,
}

/// The folder name a connection keeps its notes under: its name when that is a plain file
/// name, else a cleaned one with a hash of the name after it, so two names never share one.
fn dir_name(conn: &str) -> String {
    let plain = !conn.is_empty() && !conn.starts_with('.') && conn.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if plain {
        return conn.to_string();
    }
    let cleaned: String = conn.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(48).collect();
    // FNV-1a: stable across builds, which a std hasher is not.
    let hash = conn.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("{cleaned}-{:08x}", hash as u32)
}

/// A note's name as a file name: lower-case ASCII letters and digits, any run of other
/// characters one dash. Empty, too long or `memory` (the index's own file on a case-blind
/// disk) is refused.
pub(crate) fn slug(name: &str) -> Result<String, String> {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        return Err("name: a short kebab-case slug of ASCII letters and digits, e.g. live-room-bound-user".into());
    }
    if out.chars().count() > NAME_MAX {
        return Err(format!("name: at most {NAME_MAX} characters"));
    }
    if out == "memory" {
        return Err("name: memory is the index's own file; pick another".into());
    }
    Ok(out)
}

/// The description on one line, bounded.
fn one_line(text: &str) -> Result<String, String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return Err("description is required: one line naming the question the note answers".into());
    }
    if line.chars().count() > DESCRIPTION_MAX {
        return Err(format!("description: at most {DESCRIPTION_MAX} characters"));
    }
    Ok(line)
}

/// A note file: its front matter's fields and the body after it. A file with none is all body.
fn parse(text: &str) -> (Option<String>, Option<String>, String) {
    let text = text.replace("\r\n", "\n");
    let Some(rest) = text.strip_prefix("---\n") else { return (None, None, text.trim().to_string()) };
    let Some(end) = rest.find("\n---") else { return (None, None, text.trim().to_string()) };
    let (mut description, mut updated) = (None, None);
    for line in rest[..end].lines() {
        if let Some((key, value)) = line.split_once(':') {
            match key.trim() {
                "description" => description = Some(value.trim().to_string()),
                "updated" => updated = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    let body = rest[end + 4..].trim().to_string();
    (description, updated, body)
}

impl DbMemory {
    /// The connection's notes under the home.
    pub fn of(conn: &str) -> DbMemory {
        DbMemory::at(&swiss_core::paths::data_path(&["llm", "db-memory"]), conn)
    }

    /// The connection's notes under `root` (a test's own folder).
    pub fn at(root: &Path, conn: &str) -> DbMemory {
        DbMemory { dir: root.join(dir_name(conn)), conn: conn.to_string() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_of(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.md"))
    }

    fn head_of(&self, name: &str, text: &str) -> Note {
        let (description, updated, body) = parse(text);
        Note {
            head: NoteHead {
                name: name.to_string(),
                description: description.unwrap_or_default(),
                updated: updated.unwrap_or_default(),
                chars: body.chars().count(),
            },
            body,
        }
    }

    /// Every note, by name. A folder not made yet holds none; a file that will not read is
    /// left out.
    pub fn list(&self) -> Vec<NoteHead> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut notes: Vec<NoteHead> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let stem = path.file_stem()?.to_str()?.to_string();
                let is_note = path.extension().is_some_and(|ext| ext == "md") && !stem.eq_ignore_ascii_case("memory");
                if !is_note {
                    return None;
                }
                let text = std::fs::read_to_string(&path).ok()?;
                Some(self.head_of(&stem, &text).head)
            })
            .collect();
        notes.sort_by(|a, b| a.name.cmp(&b.name));
        notes
    }

    /// One note whole.
    pub fn read(&self, name: &str) -> Result<Note, String> {
        let name = slug(name)?;
        match std::fs::read_to_string(self.path_of(&name)) {
            Ok(text) => Ok(self.head_of(&name, &text)),
            Err(why) if why.kind() == std::io::ErrorKind::NotFound => Err(format!("no note named {name}")),
            Err(why) => Err(format!("{name}: {why}")),
        }
    }

    /// Save a note, replacing one of that name: (its name as filed, whether one was replaced).
    pub fn save(&self, name: &str, description: &str, body: &str) -> Result<(String, bool), String> {
        let name = slug(name)?;
        let description = one_line(description)?;
        let body = body.trim();
        if body.is_empty() {
            return Err("body is required: the route - what holds the data, how it links, the query that worked".into());
        }
        if body.chars().count() > BODY_MAX {
            return Err(format!("body: at most {BODY_MAX} characters - keep the route, not the data"));
        }
        let _held = WRITES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.path_of(&name);
        let replaced = path.exists();
        if !replaced && self.list().len() >= NOTES_MAX {
            return Err(format!("this connection already keeps {NOTES_MAX} notes: replace or delete one first"));
        }
        std::fs::create_dir_all(&self.dir).map_err(|why| format!("the memory folder: {why}"))?;
        let updated = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        let text = format!("---\nname: {name}\ndescription: {description}\nupdated: {updated}\n---\n\n{body}\n");
        write_whole(&path, &text)?;
        self.write_index();
        Ok((name, replaced))
    }

    /// Delete a note: whether there was one.
    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let name = slug(name)?;
        let _held = WRITES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match std::fs::remove_file(self.path_of(&name)) {
            Ok(()) => {
                self.write_index();
                Ok(true)
            }
            Err(why) if why.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(why) => Err(format!("{name}: {why}")),
        }
    }

    /// MEMORY.md from the notes as they stand - the index a person opens; the agent's
    /// reads list the notes themselves, so a note edited by hand never reads stale.
    fn write_index(&self) {
        let notes = self.list();
        let mut text = format!("# Memory of {}\n\n<!-- swiss rewrites this index on every save: edit a note, not this file. -->\n\n", self.conn);
        for note in &notes {
            text.push_str(&format!("- [{0}]({0}.md) — {1}\n", note.name, note.description));
        }
        if let Err(why) = write_whole(&self.dir.join(INDEX_FILE), &text) {
            swiss_core::log::warn(&format!("the Data agent's memory index did not write: {why}"), None);
        }
    }

    /// The index as the agent reads it: a line a note, or that there is none.
    pub fn index_text(&self) -> String {
        let notes = self.list();
        if notes.is_empty() {
            return "No notes yet on this connection.".into();
        }
        let mut text = format!("{} notes on this connection:", notes.len());
        for note in &notes {
            text.push_str(&format!("\n- {}: {}", note.name, note.description));
            if let Some(day) = note.updated.get(..10) {
                text.push_str(&format!(" (saved {day})"));
            }
        }
        text
    }

    /// The opening's Memory section: the index - as much as INDEX_IN_PROMPT holds, the rest
    /// counted for read_memory to list. When to read, save, correct or delete a note is
    /// DB_AGENT_PROMPT's, in the static head every conversation shares.
    pub fn prompt_section(&self) -> String {
        let notes = self.list();
        let mut text = String::from(
            "## Memory of this connection\n\
             The notes saved so far, each a name and the question it answers; read_memory one by its name.\n",
        );
        if notes.is_empty() {
            text.push_str("No notes yet.");
            return text;
        }
        let mut shown = 0;
        for note in &notes {
            let line = format!("- {}: {}\n", note.name, note.description);
            if text.len() + line.len() > INDEX_IN_PROMPT {
                break;
            }
            text.push_str(&line);
            shown += 1;
        }
        if shown < notes.len() {
            text.push_str(&format!("…and {} more: read_memory with no name lists them all.\n", notes.len() - shown));
        }
        text.trim_end().to_string()
    }
}

/// Write a file whole through a sibling, so a reader never meets half of it.
fn write_whole(path: &Path, text: &str) -> Result<(), String> {
    let partial = path.with_extension("md.partial");
    std::fs::write(&partial, text).map_err(|why| format!("{}: {why}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|why| {
        let _ = std::fs::remove_file(&partial);
        format!("{}: {why}", path.display())
    })
}

fn head_json(note: &NoteHead) -> Value {
    json!({ "name": note.name, "description": note.description, "updated": note.updated, "chars": note.chars })
}

/// The query's or body's connection, required and bounded.
fn conn_of(conn: Option<String>) -> Result<String, String> {
    match conn {
        Some(conn) if conn.chars().count() <= CONN_MAX => Ok(conn),
        Some(_) => Err(format!("conn: at most {CONN_MAX} characters")),
        None => Err("conn is required".into()),
    }
}

/// GET /api/llm/db/memory?conn: the connection's notes as the index lists them, by name,
/// and the folder they live in: {ok, dir, notes: [{name, description, updated, chars}]}.
pub async fn notes(Query(params): Query<std::collections::HashMap<String, String>>) -> Response {
    let conn = match conn_of(params.get("conn").filter(|c| !c.is_empty()).cloned()) {
        Ok(conn) => conn,
        Err(why) => return bad(why),
    };
    let memory = DbMemory::of(&conn);
    let notes: Vec<Value> = memory.list().iter().map(head_json).collect();
    ok(json!({ "ok": true, "dir": memory.dir().display().to_string(), "notes": notes }))
}

/// GET /api/llm/db/memory/note?conn&name: one note whole, {ok, name, description, updated,
/// chars, body}; 400 for a name the connection keeps no note under.
pub async fn note(Query(params): Query<std::collections::HashMap<String, String>>) -> Response {
    let conn = match conn_of(params.get("conn").filter(|c| !c.is_empty()).cloned()) {
        Ok(conn) => conn,
        Err(why) => return bad(why),
    };
    let Some(name) = params.get("name").filter(|n| !n.is_empty()) else { return bad("name is required") };
    match DbMemory::of(&conn).read(name) {
        Ok(note) => {
            let mut reply = head_json(&note.head);
            reply["ok"] = json!(true);
            reply["body"] = json!(note.body);
            ok(reply)
        }
        Err(why) => bad(why),
    }
}

/// POST /api/llm/db/memory/save {conn, name, description, body}: save a note as the agent's
/// save_memory does, replacing one of that name - {ok, name, replaced}.
pub async fn save(bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["conn", "name", "description", "body"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let fields = (field(&body, "conn").and_then(conn_of), field(&body, "name"), field(&body, "description"), field(&body, "body"));
    let (conn, name, description, text) = match fields {
        (Ok(conn), Ok(name), Ok(description), Ok(text)) => (conn, name.unwrap_or_default(), description.unwrap_or_default(), text.unwrap_or_default()),
        (Err(why), ..) | (_, Err(why), ..) | (_, _, Err(why), _) | (.., Err(why)) => return bad(why),
    };
    match DbMemory::of(&conn).save(&name, &description, &text) {
        Ok((name, replaced)) => ok(json!({ "ok": true, "name": name, "replaced": replaced })),
        Err(why) => bad(why),
    }
}

/// POST /api/llm/db/memory/delete {conn, name}: delete a note - {ok, deleted}, false when
/// there was none.
pub async fn delete(bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["conn", "name"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (conn, name) = match (field(&body, "conn").and_then(conn_of), field(&body, "name")) {
        (Ok(conn), Ok(Some(name))) => (conn, name),
        (_, Ok(None)) => return bad("name is required"),
        (Err(why), _) | (_, Err(why)) => return bad(why),
    };
    match DbMemory::of(&conn).delete(&name) {
        Ok(deleted) => ok(json!({ "ok": true, "deleted": deleted })),
        Err(why) => bad(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("swiss-db-memory-{name}-{}", swiss_core::util::random_hex(6)));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn a_note_saves_reads_back_and_lists_in_the_index() {
        let root = scratch("save");
        let memory = DbMemory::at(&root, "u-live");
        assert_eq!(memory.list(), Vec::new(), "a folder not made yet holds none");
        assert_eq!(memory.index_text(), "No notes yet on this connection.");
        let (name, replaced) = memory
            .save("Live room bound user", "  live room -> its bound user\n and their posts ", "live_room.anchor_uid, then users.id\n\n```sql\nSELECT 1\n```")
            .expect("saved");
        assert_eq!((name.as_str(), replaced), ("live-room-bound-user", false), "the name as a slug, the description one line");
        let note = memory.read("live-room-bound-user").expect("read");
        assert_eq!(note.head.description, "live room -> its bound user and their posts");
        assert!(note.body.starts_with("live_room.anchor_uid") && note.body.ends_with("```"), "{}", note.body);
        assert_eq!(note.head.chars, note.body.chars().count());
        assert!(note.head.updated.len() >= 19, "saved at a moment: {}", note.head.updated);
        // The index a person opens names it with its file.
        let index = std::fs::read_to_string(memory.dir().join("MEMORY.md")).expect("index");
        assert!(index.contains("- [live-room-bound-user](live-room-bound-user.md) — live room -> its bound user and their posts"), "{index}");
        // Saved again under its name, it replaces.
        let (_, replaced) = memory.save("live-room-bound-user", "the bound user", "live_room.bind_uid").expect("replaced");
        assert!(replaced);
        assert_eq!(memory.read("live-room-bound-user").expect("read").body, "live_room.bind_uid");
        memory.save("orders-by-day", "orders a day", "orders.created_at").expect("second");
        assert_eq!(memory.list().iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["live-room-bound-user", "orders-by-day"], "by name, the index not among them");
        let listed = memory.index_text();
        assert!(listed.starts_with("2 notes on this connection:\n- live-room-bound-user: the bound user (saved 20"), "{listed}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_note_deletes_and_the_index_follows() {
        let root = scratch("delete");
        let memory = DbMemory::at(&root, "pg");
        memory.save("a", "first", "one").expect("a");
        memory.save("b", "second", "two").expect("b");
        assert_eq!(memory.delete("a"), Ok(true));
        assert_eq!(memory.delete("a"), Ok(false), "gone already");
        assert_eq!(memory.read("a").unwrap_err(), "no note named a");
        let index = std::fs::read_to_string(memory.dir().join("MEMORY.md")).expect("index");
        assert!(!index.contains("[a]") && index.contains("- [b](b.md) — second"), "{index}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn what_a_note_takes_is_bounded() {
        let root = scratch("bounds");
        let memory = DbMemory::at(&root, "pg");
        assert!(memory.save("直播", "x", "y").unwrap_err().starts_with("name: a short kebab-case slug"));
        assert!(memory.save("Memory", "x", "y").unwrap_err().contains("index's own file"));
        assert!(memory.save(&"a".repeat(65), "x", "y").unwrap_err().contains("at most 64"));
        assert!(memory.save("a", " ", "y").unwrap_err().starts_with("description is required"));
        assert!(memory.save("a", &"d".repeat(201), "y").unwrap_err().contains("at most 200"));
        assert!(memory.save("a", "x", "  ").unwrap_err().starts_with("body is required"));
        assert!(memory.save("a", "x", &"b".repeat(BODY_MAX + 1)).unwrap_err().contains("not the data"));
        assert_eq!(memory.list(), Vec::new(), "nothing refused was filed");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_hand_edited_note_reads_as_it_stands_and_a_bare_file_is_all_body() {
        let root = scratch("hand");
        let memory = DbMemory::at(&root, "pg");
        std::fs::create_dir_all(memory.dir()).expect("dir");
        std::fs::write(memory.dir().join("edited.md"), "---\r\nname: edited\r\ndescription: changed by hand\r\n---\r\n\r\nthe route\r\n").expect("write");
        std::fs::write(memory.dir().join("bare.md"), "just words").expect("write");
        std::fs::write(memory.dir().join("MEMORY.md"), "# stale").expect("write");
        let heads = memory.list();
        assert_eq!(heads.iter().map(|n| (n.name.as_str(), n.description.as_str())).collect::<Vec<_>>(), [("bare", ""), ("edited", "changed by hand")]);
        assert_eq!(memory.read("edited").expect("read").body, "the route");
        assert_eq!(memory.read("bare").expect("read").body, "just words");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_opening_carries_the_index_as_far_as_it_fits() {
        let root = scratch("prompt");
        let memory = DbMemory::at(&root, "pg");
        let empty = memory.prompt_section();
        assert!(empty.starts_with("## Memory of this connection\n") && empty.ends_with("No notes yet."), "{empty}");
        memory.save("live-room-bound-user", "live room -> bound user", "route").expect("saved");
        let one = memory.prompt_section();
        assert!(one.ends_with("\n- live-room-bound-user: live room -> bound user"), "{one}");
        for i in 0..60 {
            memory.save(&format!("note-{i:02}"), &"a description long enough to fill the opening ".repeat(2), "route").expect("saved");
        }
        let many = memory.prompt_section();
        assert!(many.len() <= INDEX_IN_PROMPT + 80, "{}", many.len());
        assert!(many.contains("more: read_memory with no name lists them all."), "{many}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_connection_name_is_its_folder_when_it_can_be() {
        assert_eq!(dir_name("u-shop"), "u-shop");
        assert_eq!(dir_name("mysql.prod_1"), "mysql.prod_1");
        let odd = dir_name("team/prod db");
        assert!(odd.starts_with("team_prod_db-") && odd.len() == "team_prod_db-".len() + 8, "{odd}");
        assert_ne!(dir_name("a/b"), dir_name("a:b"), "two names never share a folder");
        assert!(dir_name("..").starts_with("__-"));
    }
}
