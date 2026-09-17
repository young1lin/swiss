/*
 * Copyright 2026 The swiss authors
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

//! Owner-only file modes — port of `privfs.ts`. On Windows chmod is a no-op beyond the writable
//! bit, exactly as in the Node build.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

pub const PRIVATE_DIR_MODE: u32 = 0o700;
pub const PRIVATE_FILE_MODE: u32 = 0o600;

/// The restrictive mode to apply on this platform. Windows has no POSIX mode bit to set through
/// std; the Node build's chmod there is equally a no-op, so we match it rather than invent ACL
/// surgery the original never did.
pub fn private_file_mode() -> u32 {
    PRIVATE_FILE_MODE
}

pub fn chmod_private(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

pub fn mkdir_private(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    chmod_private(path, PRIVATE_DIR_MODE);
}

pub fn write_file_private(path: &Path, data: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(PRIVATE_FILE_MODE)
            .open(path)?;
        f.write_all(data.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        f.write_all(data.as_bytes())?;
    }
    chmod_private(path, PRIVATE_FILE_MODE);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("swiss-privfs-{}", crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        dir
    }

    #[test]
    fn makes_the_whole_path_not_just_the_last_segment() {
        // The data dir is created on first run, under a home that may not exist yet either.
        let dir = scratch();
        let nested = dir.join("a").join("b").join(".mcp-gateway");
        mkdir_private(&nested);
        assert!(nested.is_dir());
        // Idempotent: it runs on every boot, over a directory that is usually already there.
        mkdir_private(&nested);
        assert!(nested.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_a_file_and_truncates_a_longer_one_it_replaces() {
        // master.key is rewritten in place; a leftover tail would be read back as a malformed key.
        let dir = scratch();
        let path = dir.join("master.key");
        write_file_private(&path, "a-long-first-value").expect("write");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "a-long-first-value"
        );
        write_file_private(&path, "short").expect("rewrite");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "short");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_into_a_directory_that_is_not_there_is_an_error_not_a_panic() {
        let dir = scratch();
        assert!(write_file_private(&dir.join("missing").join("k"), "x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chmod_on_a_path_that_is_not_there_is_quiet() {
        // The first-run pass walks a fixed list of state files, most of which do not exist yet.
        let dir = scratch();
        chmod_private(&dir.join("never-written.json"), PRIVATE_FILE_MODE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_modes_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch();
        let path = dir.join("env.json");
        write_file_private(&path, "{}").expect("write");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, PRIVATE_FILE_MODE);

        let sub = dir.join("home");
        mkdir_private(&sub);
        let mode = std::fs::metadata(&sub).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, PRIVATE_DIR_MODE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(unix))]
    #[test]
    fn the_mode_is_the_posix_one_even_where_it_cannot_be_applied() {
        // Windows has no POSIX mode bit to set through std, and the Node build's chmod there was
        // equally a no-op; the constant stays so the unix build and the file format agree.
        assert_eq!(private_file_mode(), PRIVATE_FILE_MODE);
        assert_eq!(PRIVATE_FILE_MODE, 0o600);
        assert_eq!(PRIVATE_DIR_MODE, 0o700);
    }
}
