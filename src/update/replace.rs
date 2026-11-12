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

//! The rename dance that replaces a running executable (ADR-041). The second rename is the
//! one a test can fail; the first failure leaves the original in place.

use std::io;
use std::path::{Path, PathBuf};

pub fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    exe.with_file_name(format!("{name}{suffix}"))
}

pub trait SwapIo {
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&mut self, path: &Path) -> io::Result<()>;
    fn exists(&mut self, path: &Path) -> bool;
}

pub struct RealFs;

impl SwapIo for RealFs {
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
    fn remove_file(&mut self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }
    fn exists(&mut self, path: &Path) -> bool {
        path.is_file()
    }
}

/// `<exe>` → `<exe>.old`, then `<exe>.new` → `<exe>`. An `.old` that cannot be deleted is
/// renamed aside. A failure of the second rename puts the original back.
pub fn swap(exe: &Path, fs: &mut dyn SwapIo) -> Result<(), String> {
    let new = sibling(exe, ".new");
    let old = sibling(exe, ".old");
    park_old(exe, fs)?;
    fs.rename(exe, &old).map_err(|err| format!("cannot rename {} to {}: {err}", exe.display(), old.display()))?;
    if let Err(err) = fs.rename(&new, exe) {
        let _ = fs.rename(&old, exe);
        return Err(format!("cannot rename {} to {}: {err}", new.display(), exe.display()));
    }
    Ok(())
}

fn park_old(exe: &Path, fs: &mut dyn SwapIo) -> Result<(), String> {
    let old = sibling(exe, ".old");
    match fs.remove_file(&old) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => {
            let aside = sibling(exe, &format!(".old.{}", swiss_core::util::random_hex(4)));
            fs.rename(&old, &aside).map_err(|err| format!("cannot move {} aside: {err}", old.display()))
        }
    }
}

/// Undo a swap that already moved both names: `<exe>` (the new bytes) → `<exe>.new`,
/// `<exe>.old` → `<exe>`. If `<exe>.old` is not there, the current exe is left where it is.
pub fn rollback(exe: &Path, fs: &mut dyn SwapIo) -> Result<(), String> {
    let new = sibling(exe, ".new");
    let old = sibling(exe, ".old");
    if !fs.exists(&old) {
        return Err(format!("rollback found no {}", old.display()));
    }
    fs.rename(exe, &new).map_err(|err| format!("rollback cannot move the new exe aside: {err}"))?;
    fs.rename(&old, exe).map_err(|err| format!("rollback cannot restore the previous exe: {err}"))?;
    let _ = fs.remove_file(&new);
    Ok(())
}

/// The pid in `update.lock` when it names a different live process. A missing file, an
/// unreadable pid, or a dead pid is not held: the next taker treats it as stale.
pub fn held_lock_pid(data_dir: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(data_dir.join("update").join("update.lock")).ok()?;
    let pid = text.trim().parse::<u32>().ok()?;
    (pid != std::process::id() && swiss_core::platform::pid_alive(pid)).then_some(pid)
}

/// Boot cleanup. A live updater still owns the parked exe, so a held lock skips the delete.
pub fn cleanup_after_boot(exe: &Path, data_dir: &Path) {
    if held_lock_pid(data_dir).is_some() {
        return;
    }
    cleanup_old_siblings(exe);
}

/// Best-effort delete of `<exe>.old` and `<exe>.old.*` next to the binary. A running image
/// on Windows may refuse; the next boot tries again.
pub fn cleanup_old_siblings(exe: &Path) {
    let Some(dir) = exe.parent() else {
        return;
    };
    let Some(name) = exe.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return;
    };
    let prefix = format!("{name}.old");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name == prefix || file_name.starts_with(&format!("{prefix}.")) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub fn dir_writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(format!(".swiss-update-{}.probe", swiss_core::util::random_hex(4)));
    std::fs::write(&probe, b"ok").map_err(|_| format!("exe directory is not writable: {}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

pub fn in_app_bundle(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('\\', "/");
    text.contains(".app/Contents/MacOS/")
}
