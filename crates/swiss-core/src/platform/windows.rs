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

//! The Windows platform seam — direct Win32 calls replacing every `powershell.exe` spawn the Node
//! build had to make. Each of those spawns cost ~65 MB of transient working set and ~350 ms; the
//! direct calls are free, which is why the memory view stops being opt-in in this build.

use windows::core::{w, PCWSTR, Result as WinResult};
use windows::Win32::Foundation::{LocalFree, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_OPEN_CREATE_OPTIONS,
    REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};

use super::ParentProcess;

/// An owned `CRYPT_INTEGER_BLOB`. Only ever wraps bytes this side allocated (a Vec that outlives
/// the call), so DPAPI never sees a dangling pointer.
struct EntropyBlob {
    bytes: Vec<u8>,
    blob: CRYPT_INTEGER_BLOB,
}

impl EntropyBlob {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            blob: CRYPT_INTEGER_BLOB {
                cbData: bytes.len() as u32,
                pbData: std::ptr::null_mut(),
            },
        }
    }

    /// The pointer is only valid while `self` is alive; callers keep it alive across the DPAPI
    /// call by borrowing.
    fn as_blob(&mut self) -> CRYPT_INTEGER_BLOB {
        self.blob.pbData = self.bytes.as_mut_ptr();
        self.blob
    }
}

/// The entropy blob every DPAPI call passes: the UTF-8 bytes of the app string, which is part of
/// the on-disk `master.key` format — a different string here and the blob does not open.
fn app_entropy() -> EntropyBlob {
    EntropyBlob::from_bytes(crate::secure::key::APP.as_bytes())
}

/// DPAPI CryptUnprotectData (CurrentUser) with the app entropy. The unsafe is confined to this
/// FFI boundary, per AGENTS.md: pointer lifetimes into DPAPI and the LocalFree of its output.
pub fn dpapi_unprotect(blob: &[u8]) -> WinResult<Vec<u8>> {
    let mut input = EntropyBlob::from_bytes(blob);
    let mut entropy = app_entropy();
    let mut output = CRYPT_INTEGER_BLOB::default();

    // SAFETY: all three blobs point at allocations owned by live locals; the output blob is
    // written by DPAPI and freed below; no prompt UI is requested (None); flags = 0, exactly
    // what the Node build's ProtectedData::Unprotect(..., 'CurrentUser') passes.
    unsafe {
        CryptUnprotectData(
            &input.as_blob(),
            None,
            Some(&entropy.as_blob()),
            None,
            None,
            0,
            &mut output,
        )?;
    }

    // SAFETY: DPAPI allocated output.pbData with LocalAlloc; copying out and freeing it in one
    // scope guarantees no leak and no use-after-free.
    let plain = unsafe {
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(HLOCAL(output.pbData as *mut core::ffi::c_void));
        slice
    };
    Ok(plain)
}

/// DPAPI CryptProtectData (CurrentUser) with the app entropy — the write side of master.key.
pub fn dpapi_protect(blob: &[u8]) -> WinResult<Vec<u8>> {
    let mut input = EntropyBlob::from_bytes(blob);
    let mut entropy = app_entropy();
    let mut output = CRYPT_INTEGER_BLOB::default();

    // SAFETY: same shape as dpapi_unprotect; the output buffer is LocalAlloc'd by DPAPI and
    // freed by us below.
    unsafe {
        CryptProtectData(
            &input.as_blob(),
            None,
            Some(&entropy.as_blob()),
            None,
            None,
            0,
            &mut output,
        )?;
    }

    let plain = unsafe {
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(HLOCAL(output.pbData as *mut core::ffi::c_void));
        slice
    };
    Ok(plain)
}

/// The MachineGuid from HKLM\SOFTWARE\Microsoft\Cryptography — a direct registry read replacing
/// the Node build's `reg query` spawn. None when the value is absent or unreadable.
pub fn machine_id() -> Option<String> {
    // SAFETY: REG_SZ data is read as a wide string; the buffer length bounds the copy and the
    // handle is closed on every path.
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Cryptography"),
            0,
            REG_SAM_FLAGS(KEY_READ.0),
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut ty = REG_VALUE_TYPE::default();
        let mut len = 0u32;
        if RegQueryValueExW(
            hkey,
            w!("MachineGuid"),
            None,
            Some(&mut ty),
            None,
            Some(&mut len),
        ) != ERROR_SUCCESS
            || ty != REG_SZ
            || len == 0
        {
            let _ = RegCloseKey(hkey);
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let ok = RegQueryValueExW(
            hkey,
            w!("MachineGuid"),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        ) == ERROR_SUCCESS;
        let _ = RegCloseKey(hkey);
        if !ok {
            return None;
        }
        // REG_SZ is NUL-terminated UTF-16; trim everything from the first NUL.
        let wide = std::slice::from_raw_parts(buf.as_ptr() as *const u16, len as usize / 2);
        let end = wide.iter().position(|c| *c == 0).unwrap_or(wide.len());
        Some(String::from_utf16_lossy(&wide[..end]))
    }
}

/// The HKCU Run entry the start-at-sign-in setting owns (src/autostart.rs): one REG_SZ value
/// named "swiss" under the per-user Run key. HKCU needs no elevation, and a per-user value is
/// the honest scope for a per-user toolbox. None when the value is absent or unreadable.
pub fn run_entry_read() -> Option<String> {
    // SAFETY: same shape as machine_id — the handle is closed on every path and the buffer is
    // bounded by the length the size query reported.
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            0,
            REG_SAM_FLAGS(KEY_READ.0),
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut ty = REG_VALUE_TYPE::default();
        let mut len = 0u32;
        if RegQueryValueExW(hkey, w!("swiss"), None, Some(&mut ty), None, Some(&mut len))
            != ERROR_SUCCESS
            || ty != REG_SZ
            || len == 0
        {
            let _ = RegCloseKey(hkey);
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let ok = RegQueryValueExW(
            hkey,
            w!("swiss"),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        ) == ERROR_SUCCESS;
        let _ = RegCloseKey(hkey);
        if !ok {
            return None;
        }
        // REG_SZ is NUL-terminated UTF-16; trim everything from the first NUL.
        let wide = std::slice::from_raw_parts(buf.as_ptr() as *const u16, len as usize / 2);
        let end = wide.iter().position(|c| *c == 0).unwrap_or(wide.len());
        Some(String::from_utf16_lossy(&wide[..end]))
    }
}

/// Write the Run value, creating the key if a profile somehow lacks it. The value data is a
/// NUL-terminated UTF-16 copy of the command string, owned by a live Vec across the call.
pub fn run_entry_write(cmd: &str) -> Result<(), String> {
    // SAFETY: the handle is created and closed here; the value data points at a Vec that
    // outlives the call, and every parameter that may be NULL is.
    unsafe {
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            0,
            PCWSTR::null(),
            REG_OPEN_CREATE_OPTIONS(0), // REG_OPTION_NON_VOLATILE — persist across sessions
            REG_SAM_FLAGS(KEY_WRITE.0),
            None,
            &mut hkey,
            None,
        ) != ERROR_SUCCESS
        {
            return Err("could not open the Run key".to_string());
        }
        let mut wide: Vec<u16> = cmd.encode_utf16().collect();
        wide.push(0); // REG_SZ is NUL-terminated
        // The binding takes the value data as a byte slice, length included.
        let mut data: Vec<u8> = Vec::with_capacity(wide.len() * 2);
        for unit in &wide {
            data.extend_from_slice(&unit.to_le_bytes());
        }
        let written = RegSetValueExW(hkey, w!("swiss"), 0, REG_SZ, Some(&data));
        let _ = RegCloseKey(hkey);
        if written != ERROR_SUCCESS {
            return Err(format!("could not write the Run value ({written:?})"));
        }
        Ok(())
    }
}

/// Delete the Run value. A value that is already gone is success — "off" must be idempotent.
pub fn run_entry_remove() -> Result<(), String> {
    // SAFETY: the handle is opened and closed here; RegDeleteValueW takes no pointers.
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            0,
            REG_SAM_FLAGS(KEY_WRITE.0),
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return Err("could not open the Run key".to_string());
        }
        let deleted = RegDeleteValueW(hkey, w!("swiss"));
        let _ = RegCloseKey(hkey);
        if deleted == ERROR_SUCCESS || deleted == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        Err(format!("could not delete the Run value ({deleted:?})"))
    }
}

/// This process's own working set, in bytes. Free and always current — the in-process half of the
/// memory view (`process.memoryUsage().rss` in the Node build).
pub fn self_working_set() -> u64 {
    // SAFETY: GetCurrentProcess() returns a pseudo-handle needing no close, and the counters
    // struct is a plain out-parameter sized to itself.
    unsafe {
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        let ok = GetProcessMemoryInfo(
            windows::Win32::System::Threading::GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        );
        if ok.is_ok() {
            counters.WorkingSetSize as u64
        } else {
            0
        }
    }
}

/// This process's private commit charge, in bytes — the honest stand-in for the heap figures the
/// Node build read out of V8 (`PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`). 0 when the OS declines.
pub fn self_private_bytes() -> u64 {
    // SAFETY: as above; the EX struct is passed through the base type's pointer, which is what
    // the API documents for callers wanting PrivateUsage.
    unsafe {
        let mut counters =
            windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX::default();
        let ok = GetProcessMemoryInfo(
            windows::Win32::System::Threading::GetCurrentProcess(),
            &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            std::mem::size_of::<windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX>()
                as u32,
        );
        if ok.is_ok() {
            counters.PrivateUsage as u64
        } else {
            0
        }
    }
}

fn process_working_set(pid: u32) -> Option<u64> {
    // SAFETY: the handle is closed on every path; the counters struct is a plain out-parameter.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        let ok = GetProcessMemoryInfo(
            handle,
            &mut counters,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        ok.is_ok().then_some(counters.WorkingSetSize as u64)
    }
}

/// Whether a pid names a live process — the pid-file reader's existence probe.
///
/// SAFETY: the handle is closed on every path; GetExitCodeProcess writes a plain u32.
pub fn pid_alive(pid: u32) -> bool {
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut exit_code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        // STILL_ACTIVE (259) is the sentinel for "running" — a process that really exited with
        // code 259 is indistinguishable, which every Windows supervisor accepts.
        ok.is_ok() && exit_code == 259
    }
}

/// The process that spawned THIS one: parent pid + image name, both from one Toolhelp
/// snapshot pass (the snapshot carries th32ParentProcessID and szExeFile side by side, so
/// attribution costs no extra walk and no subprocess). The parent of a detached `swiss
/// start` child has usually exited by inspection time - the boot log reads this while the
/// launcher is typically still alive, which is exactly when the answer matters.
pub fn parent_process() -> Option<ParentProcess> {
    // SAFETY: the snapshot handle is closed on every path; PROCESSENTRY32W is initialized
    // with its own size as the API requires.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return None; // no snapshot - callers log "parent unknown", never a guess
        };
        let own = std::process::id();
        let mut parent: Option<u32> = None;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == own {
                    parent = Some(entry.th32ParentProcessID);
                    break; // one pass finds self; the parent entry comes from a second look
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        // Reset the walk and look the parent up by pid for its image name. A pid reused
        // between the two passes can misname the parent - logged attribution is a hint,
        // not an identity claim.
        let mut name = None;
        if let Some(parent_pid) = parent {
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            if Process32FirstW(snapshot, &mut entry).is_ok() {
                loop {
                    if entry.th32ProcessID == parent_pid {
                        let end = entry
                            .szExeFile
                            .iter()
                            .position(|c| *c == 0)
                            .unwrap_or(entry.szExeFile.len());
                        name = Some(String::from_utf16_lossy(&entry.szExeFile[..end]));
                        break;
                    }
                    if Process32NextW(snapshot, &mut entry).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
        parent.map(|pid| ParentProcess {
            pid,
            name: name.unwrap_or_default(),
        })
    }
}

/// `own_pid` and every descendant (any process type), via the Toolhelp parent->child walk —
/// the direct replacement for the Node build's PowerShell CIM query. A reaper uses it to refuse
/// to touch a stale-ledger pid the OS has since handed to one of THIS instance's own children.
///
/// `None` = the process snapshot itself failed, so the tree is UNKNOWN. Callers fail closed on
/// it (the reaper skips the whole sweep): a walk that under-reports would let a reaper kill a
/// live child of this very instance, which is the one outcome worse than leaving an orphan.
pub fn descendant_pids(own_pid: u32) -> Option<Vec<u32>> {
    // SAFETY: the snapshot handle is closed on every path; PROCESSENTRY32W is initialized with
    // its own size as the API requires.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return None; // no snapshot — the caller must not guess at the tree
        };
        let mut parents: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);

        // No exclusion: the root IS this process, and the caller's whole question is which pids
        // belong to this instance.
        let tree = crate::platform::walk_descendants(&parents, &[own_pid], None);
        Some(tree.into_iter().collect())
    }
}

/// Force-kill a process AND its whole descendant tree, any depth. On Windows killing a parent
/// does NOT cascade to children (the root cause of the Node build's orphan leak — an
/// `npx`-launched MCP leaves cmd.exe -> node -> the real server, and only the top dies), so the
/// tree is walked first and every member terminated explicitly. Replaces `taskkill /T /F` with
/// the same snapshot walk — no subprocess.
pub fn tree_kill(pid: u32) {
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    // An unknown tree still kills the root itself (it was named explicitly); only the un-walked
    // descendants are at risk of leaking, and the proc children's kill-on-close job object is
    // the backstop for exactly that.
    let victims = descendant_pids(pid).unwrap_or_else(|| vec![pid]);
    for victim in victims {
        if victim == std::process::id() {
            continue; // a corrupted snapshot edge must never turn this into suicide
        }
        // SAFETY: the handle is closed on every path; TerminateProcess takes no pointers.
        unsafe {
            if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, victim) {
                let _ = TerminateProcess(handle, 1);
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
        }
    }
}

/// Sum of the working sets of every process in the subtrees rooted at `roots` (BFS by
/// ParentProcessId — a proc MCP is cmd.exe -> npx -> the real server, so descendants are what
/// matter). Toolhelp32 snapshot walk — the direct replacement for the Node build's PowerShell
/// CIM query, minus the ~65 MB spawn. Returns None when the snapshot cannot be taken at all, so
/// the caller can report "pending" rather than a confident zero.
pub fn process_tree_working_set(roots: &[u32]) -> Option<(u64, usize)> {
    // SAFETY: the snapshot handle is closed on every path; PROCESSENTRY32W is initialized with
    // its own size as the API requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;

        // One pass builds pid -> parent map + working set per pid; the BFS then runs over it.
        let mut parents: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut seen = 0usize;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                seen += 1;
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
        if seen == 0 {
            return None;
        }

        // The gateway is excluded: the panel reports its own footprint separately, so counting it
        // in the subtree total would show it twice.
        let tree = crate::platform::walk_descendants(&parents, roots, Some(std::process::id()));

        // Roots sampled but ALL gone by walk time (children exiting) would sum to a confident 0
        // MB — the walker reports "no measurement" instead, matching the Node build's contract
        // of "report pending, never a confident zero".
        let mut total = 0u64;
        let mut count = 0usize;
        for pid in &tree {
            if let Some(ws) = process_working_set(*pid) {
                total += ws;
                count += 1;
            }
        }
        if count == 0 {
            return None;
        }
        Some((total, count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_process_names_the_test_runner_that_spawned_it() {
        // Whatever runs this binary (cargo, a harness shell) is the parent: a real pid,
        // never our own, with a resolvable image name. This is exactly the line the boot
        // log prints as attribution - a daemon started by a launcher answers the same way.
        let parent = parent_process().expect("snapshot walk finds our own entry");
        assert_ne!(parent.pid, 0);
        assert_ne!(parent.pid, std::process::id());
        assert!(!parent.name.is_empty());
    }
}

