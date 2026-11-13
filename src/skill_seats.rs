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

//! The skill seat ledger (SPEC §host.cli). One seat is one skills directory of one kind.
//! The file is plain JSON: it holds paths the user already has, and no secret. A missing
//! file is an empty ledger. A file that cannot be parsed is left byte for byte.

use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "skill-seats.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeatKind {
    Claude,
    Agents,
}

impl SeatKind {
    pub fn root(self) -> &'static str {
        match self {
            SeatKind::Claude => ".claude",
            SeatKind::Agents => ".agents",
        }
    }

    pub fn from_root(root: &str) -> Option<Self> {
        match root {
            ".claude" => Some(SeatKind::Claude),
            ".agents" => Some(SeatKind::Agents),
            _ => None,
        }
    }
}

/// Where a seat in a status answer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeatSource {
    Env,
    Ledger,
    Both,
}

/// A field a later version adds is ignored on read, so an older binary never treats a newer
/// ledger as broken and refuses to install.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LedgerSeat {
    pub kind: SeatKind,
    pub dir: String,
    #[serde(rename = "addedAt")]
    pub added_at: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct LedgerFile {
    schema: u32,
    seats: Vec<LedgerSeat>,
}

pub enum LedgerLoad {
    Missing,
    Seats(Vec<LedgerSeat>),
    Broken(String),
}

pub fn load(path: &Path) -> LedgerLoad {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return LedgerLoad::Missing,
        Err(err) => return broken(path, &err.to_string()),
    };
    match serde_json::from_slice::<LedgerFile>(&bytes) {
        Ok(file) if file.schema == 1 => LedgerLoad::Seats(file.seats),
        Ok(file) => broken(path, &format!("schema {} is not 1", file.schema)),
        Err(err) => broken(path, &err.to_string()),
    }
}

fn broken(path: &Path, err: &str) -> LedgerLoad {
    LedgerLoad::Broken(format!(
        "skill seats at {} could not be read ({err}); the file was not changed",
        path.display()
    ))
}

/// Refuse before any skill file is written when the ledger cannot accept a new row.
pub fn ensure_writable(path: &Path) -> Result<(), String> {
    match load(path) {
        LedgerLoad::Broken(err) => Err(err),
        LedgerLoad::Missing | LedgerLoad::Seats(_) => Ok(()),
    }
}

/// Record one seat. A seat already present is left as it was, including its `addedAt`.
pub fn remember(path: &Path, kind: SeatKind, dir: &Path) -> Result<(), String> {
    let dir = normalize_dir(dir);
    let _held = lock_ledger(path);
    let mut seats = match load(path) {
        LedgerLoad::Missing => Vec::new(),
        LedgerLoad::Seats(seats) => seats,
        LedgerLoad::Broken(err) => return Err(err),
    };
    if seats.iter().any(|seat| seat.kind == kind && same_dir(Path::new(&seat.dir), &dir)) {
        return Ok(());
    }
    seats.push(LedgerSeat {
        kind,
        dir: dir.to_string_lossy().into_owned(),
        added_at: now_rfc3339(),
    });
    write(path, &seats)
}

/// Drop ledger rows whose directory is this one. A missing ledger is already empty.
pub fn forget(path: &Path, dir: &Path) -> Result<(), String> {
    let dir = normalize_dir(dir);
    let _held = lock_ledger(path);
    let mut seats = match load(path) {
        LedgerLoad::Missing => return Ok(()),
        LedgerLoad::Seats(seats) => seats,
        LedgerLoad::Broken(err) => return Err(err),
    };
    let before = seats.len();
    seats.retain(|seat| !same_dir(Path::new(&seat.dir), &dir));
    if seats.len() == before {
        return Ok(());
    }
    write(path, &seats)
}

/// The ledger's writer lock: an OS lock on `skill-seats.lock` beside the file, held across
/// each load-change-write. Without it two writers (two panel requests on blocking threads, or
/// the CLI and the gateway) load the same rows and the later write drops the other's seat; the
/// temp-and-rename write only keeps the file whole. The OS releases the lock with its process.
/// A directory that cannot hold the lock file leaves the write unlocked rather than refusing it.
fn lock_ledger(path: &Path) -> Option<std::fs::File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok()?;
        }
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .ok()?;
    file.lock().ok()?;
    Some(file)
}

pub fn same_dir(a: &Path, b: &Path) -> bool {
    dir_key(a) == dir_key(b)
}

/// Whether a recorded seat is still there. A claude seat is `<config>/skills`, and the config
/// directory is what the user named: while it exists the seat is live even before its first
/// install creates `skills`. An agents seat is the skills directory itself.
pub fn seat_present(kind: SeatKind, dir: &Path) -> bool {
    match kind {
        SeatKind::Claude if ends_with_skills(dir) => dir.parent().is_some_and(Path::is_dir),
        _ => dir.is_dir(),
    }
}

pub fn ends_with_skills(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
        if cfg!(windows) { name.eq_ignore_ascii_case("skills") } else { name == "skills" }
    })
}

/// Absolute, without a trailing separator. The path need not exist.
pub fn normalize_dir(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let text = abs.to_string_lossy();
    let trimmed = text.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() { abs } else { PathBuf::from(trimmed) }
}

fn dir_key(path: &Path) -> String {
    let text = normalize_dir(path).to_string_lossy().replace('/', "\\");
    if cfg!(windows) { text.to_ascii_lowercase() } else { normalize_dir(path).to_string_lossy().into_owned() }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn write(path: &Path, seats: &[LedgerSeat]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
    }
    let body = serde_json::to_vec_pretty(&LedgerFile { schema: 1, seats: seats.to_vec() })
        .map_err(|err| err.to_string())?;
    let tmp = path.with_file_name(format!(
        ".skill-seats-{}.tmp",
        swiss_core::util::random_hex(8)
    ));
    let wrote = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path));
    if wrote.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    wrote.map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writers_on_many_threads_keep_every_seat() {
        let dir = std::env::temp_dir().join(format!("swiss-seats-{}", swiss_core::util::random_hex(8)));
        let ledger = dir.join(FILE_NAME);
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let ledger = ledger.clone();
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for i in 0..5 {
                        remember(&ledger, SeatKind::Agents, &dir.join(format!("seat-{t}-{i}"))).expect("remember");
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("a writer thread");
        }
        let LedgerLoad::Seats(rows) = load(&ledger) else {
            panic!("the ledger reads back");
        };
        assert_eq!(rows.len(), 40, "a concurrent write lost a seat");

        let gone = dir.join("seat-0-0");
        forget(&ledger, &gone).expect("forget");
        let LedgerLoad::Seats(rows) = load(&ledger) else {
            panic!("the ledger reads back");
        };
        assert_eq!(rows.len(), 39);
        assert!(!rows.iter().any(|row| same_dir(Path::new(&row.dir), &gone)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
