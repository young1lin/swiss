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
