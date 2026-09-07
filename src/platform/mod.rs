//! Platform seams: file privacy, the machine id, process walking and DPAPI all live behind this
//! module so the rest of the crate stays platform-agnostic.

mod privfs;
pub use privfs::{
    chmod_private, mkdir_private, private_file_mode, write_file_private, PRIVATE_DIR_MODE,
    PRIVATE_FILE_MODE,
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{
    descendant_pids, dpapi_protect, dpapi_unprotect, machine_id, pid_alive,
    process_tree_working_set, self_private_bytes, self_working_set, tree_kill,
};

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::{machine_id, process_tree_working_set, self_private_bytes, self_working_set};

// Off Windows there is no Toolhelp; the plain-libc forms suffice (this gateway ships for
// Windows, the unix side only has to build and behave sanely).
#[cfg(not(windows))]
pub fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0) tests for existence without delivering anything.
    unsafe { libc_kill_zero(pid) }
}

#[cfg(not(windows))]
fn libc_kill_zero(_pid: u32) -> bool {
    // No libc dependency is carried for the fallback target; `/proc` is the portable probe.
    std::path::Path::new(&format!("/proc/{_pid}")).exists()
}

#[cfg(not(windows))]
pub fn descendant_pids(own_pid: u32) -> Vec<u32> {
    vec![own_pid]
}

#[cfg(not(windows))]
pub fn tree_kill(pid: u32) {
    // The posix fallback mirrors the Node build's: a plain terminate of the direct pid.
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
}

// DPAPI exists only on Windows; the key source list on other platforms omits it.
#[cfg(not(windows))]
pub fn dpapi_unprotect(_blob: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI is a Windows-only key source".into())
}
#[cfg(not(windows))]
pub fn dpapi_protect(_blob: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI is a Windows-only key source".into())
}
