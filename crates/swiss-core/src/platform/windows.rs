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

//! The Windows platform seam — direct Win32 calls replacing every `powershell.exe` spawn the Node
//! build had to make. Each of those spawns cost ~65 MB of transient working set and ~350 ms; the
//! direct calls are free, which is why the memory view stops being opt-in in this build.

use windows::core::{w, Result as WinResult, PCWSTR};
use windows::Win32::Foundation::{
    LocalFree, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HLOCAL, LPARAM, WPARAM,
};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_OPEN_CREATE_OPTIONS,
    REG_EXPAND_SZ, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

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
        LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
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
        LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
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
            None,
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

/// A NUL-terminated UTF-16 copy of `s`, for a PCWSTR that must outlive the call it is passed to.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// One string value under HKCU: its text and whether it is REG_EXPAND_SZ. Ok(None) when the key
/// or the value is absent; an Err when it is there but cannot be read as a string - which a
/// writer must not mistake for "absent" and overwrite. The Run entry and the user PATH both
/// read through here, and the suite drives it against a scratch key - never the operator's own.
fn hkcu_string_read(subkey: &str, value: &str) -> Result<Option<(String, bool)>, String> {
    let label = format!("HKCU\\{subkey} value {value}");
    let subkey = wide(subkey);
    let value = wide(value);
    // SAFETY: both name buffers outlive the calls; the handle is closed on every path; the data
    // buffer is bounded by the length the size query reported.
    unsafe {
        let mut hkey = HKEY::default();
        let opened = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            REG_SAM_FLAGS(KEY_READ.0),
            &mut hkey,
        );
        if opened == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if opened != ERROR_SUCCESS {
            return Err(format!("could not open {label} ({opened:?})"));
        }
        let name = PCWSTR(value.as_ptr());
        let mut ty = REG_VALUE_TYPE::default();
        let mut len = 0u32;
        let sized = RegQueryValueExW(hkey, name, None, Some(&mut ty), None, Some(&mut len));
        if sized != ERROR_SUCCESS || (ty != REG_SZ && ty != REG_EXPAND_SZ) {
            let _ = RegCloseKey(hkey);
            return match sized {
                ERROR_FILE_NOT_FOUND => Ok(None),
                ERROR_SUCCESS => Err(format!("{label} is not a string value ({ty:?})")),
                err => Err(format!("could not read {label} ({err:?})")),
            };
        }
        let mut buf = vec![0u8; len as usize];
        let read = RegQueryValueExW(hkey, name, None, None, Some(buf.as_mut_ptr()), Some(&mut len));
        let _ = RegCloseKey(hkey);
        if read != ERROR_SUCCESS {
            return Err(format!("could not read {label} ({read:?})"));
        }
        // A string value is UTF-16 and normally NUL-terminated; trim everything from the first
        // NUL. Paired up byte by byte, so an odd length or an unaligned buffer cannot misread.
        let units: Vec<u16> = buf[..(len as usize).min(buf.len())]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let end = units.iter().position(|c| *c == 0).unwrap_or(units.len());
        Ok(Some((String::from_utf16_lossy(&units[..end]), ty == REG_EXPAND_SZ)))
    }
}

/// Write one string value under HKCU, creating the key if a profile somehow lacks it. `expand`
/// picks REG_EXPAND_SZ - what a PATH holding `%USERPROFILE%` must stay - over REG_SZ.
fn hkcu_string_write(subkey: &str, value: &str, data: &str, expand: bool) -> Result<(), String> {
    let key = wide(subkey);
    let name = wide(value);
    // The binding takes the value data as bytes, the terminating NUL included.
    let bytes: Vec<u8> = wide(data).iter().flat_map(|unit| unit.to_le_bytes()).collect();
    // SAFETY: the handle is created and closed here; every buffer outlives the call it is
    // passed to, and every parameter that may be NULL is.
    unsafe {
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPEN_CREATE_OPTIONS(0), // REG_OPTION_NON_VOLATILE - persist across sessions
            REG_SAM_FLAGS(KEY_WRITE.0),
            None,
            &mut hkey,
            None,
        ) != ERROR_SUCCESS
        {
            return Err(format!("could not open HKCU\\{subkey}"));
        }
        let ty = if expand { REG_EXPAND_SZ } else { REG_SZ };
        let written = RegSetValueExW(hkey, PCWSTR(name.as_ptr()), None, ty, Some(&bytes));
        let _ = RegCloseKey(hkey);
        if written != ERROR_SUCCESS {
            return Err(format!("could not write HKCU\\{subkey} value {value} ({written:?})"));
        }
        Ok(())
    }
}

/// Delete one value under HKCU. A value - or a key - that is already gone is success: "off"
/// must be idempotent.
fn hkcu_value_remove(subkey: &str, value: &str) -> Result<(), String> {
    let key = wide(subkey);
    let name = wide(value);
    // SAFETY: the handle is opened and closed here; both name buffers outlive the calls.
    unsafe {
        let mut hkey = HKEY::default();
        let opened = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            None,
            REG_SAM_FLAGS(KEY_WRITE.0),
            &mut hkey,
        );
        if opened == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        if opened != ERROR_SUCCESS {
            return Err(format!("could not open HKCU\\{subkey}"));
        }
        let deleted = RegDeleteValueW(hkey, PCWSTR(name.as_ptr()));
        let _ = RegCloseKey(hkey);
        if deleted == ERROR_SUCCESS || deleted == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        Err(format!("could not delete HKCU\\{subkey} value {value} ({deleted:?})"))
    }
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "swiss";

/// The HKCU Run entry the start-at-sign-in setting owns (src/autostart.rs): one string value
/// named "swiss" under the per-user Run key. HKCU needs no elevation, and a per-user value is
/// the honest scope for a per-user toolbox. None when the value is absent, empty or unreadable.
pub fn run_entry_read() -> Option<String> {
    hkcu_string_read(RUN_KEY, RUN_VALUE)
        .ok()
        .flatten()
        .map(|(cmd, _)| cmd)
        .filter(|cmd| !cmd.is_empty())
}

/// Write the Run value as REG_SZ, creating the key if a profile somehow lacks it.
pub fn run_entry_write(cmd: &str) -> Result<(), String> {
    hkcu_string_write(RUN_KEY, RUN_VALUE, cmd, false)
}

/// Delete the Run value. A value that is already gone is success.
pub fn run_entry_remove() -> Result<(), String> {
    hkcu_value_remove(RUN_KEY, RUN_VALUE)
}

const ENV_KEY: &str = "Environment";
const PATH_VALUE: &str = "Path";

/// The current user's own PATH exactly as stored - `%VAR%` references unexpanded - and whether
/// it is REG_EXPAND_SZ (src/userpath.rs). Ok(None) when the user has no PATH value of their own;
/// an Err when there is one that cannot be read, so nothing writes over it.
pub fn user_path_read() -> Result<Option<(String, bool)>, String> {
    hkcu_string_read(ENV_KEY, PATH_VALUE)
}

/// Store the current user's PATH, then announce the change the way setx and the System
/// Properties dialog do, so a terminal opened from Explorer afterwards sees it. Programs that
/// are already running - this gateway, an open terminal - keep the PATH they started with.
pub fn user_path_write(path: &str, expand: bool) -> Result<(), String> {
    hkcu_string_write(ENV_KEY, PATH_VALUE, path, expand)?;
    broadcast_environment_change();
    Ok(())
}

/// WM_SETTINGCHANGE("Environment") to every top-level window - the message Explorer rereads
/// the user environment on. A hung window is skipped (SMTO_ABORTIFHUNG) and a slow one gets a
/// second, so no other program can hold the caller; the registry write already stands either way.
fn broadcast_environment_change() {
    let area = wide("Environment");
    // SAFETY: lParam points at a NUL-terminated wide string that outlives the call, which is
    // all WM_SETTINGCHANGE reads; no result pointer is requested.
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(area.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            1000,
            None,
        );
    }
}

/// `s` with its `%VAR%` references expanded against this process's environment - how a PATH
/// entry is compared with a directory. An unknown variable stays as written, as cmd leaves it.
pub fn expand_env(s: &str) -> String {
    let src = wide(s);
    // SAFETY: the source buffer outlives both calls; the destination is sized from the first
    // call's answer, and the second call writes at most that many units.
    unsafe {
        let need = ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), None);
        if need == 0 {
            return s.to_string();
        }
        let mut buf = vec![0u16; need as usize];
        let got = ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut buf));
        if got == 0 || got as usize > buf.len() {
            return s.to_string();
        }
        let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
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

/// The image name of an arbitrary pid, from one Toolhelp snapshot pass — the direct
/// replacement for `tasklist /FI "PID eq n" /FO CSV /NH` (no subprocess, no CSV to
/// parse). `None` = the pid is gone or the snapshot failed; callers word their diagnostic
/// for that case rather than guessing.
pub fn process_name(pid: u32) -> Option<String> {
    // SAFETY: the snapshot handle is closed on every path; PROCESSENTRY32W is initialized
    // with its own size as the API requires.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return None;
        };
        let mut name = None;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == pid {
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
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
        name
    }
}

/// The pid that owns a LISTENING socket on `port`, for either IP family —
/// GetExtendedTcpTable with the listener-only class, the direct replacement for parsing
/// `netstat -ano` text output. `None` = nothing is listening or the table could not be
/// read; the caller's diagnostic words cover both.
pub fn tcp_listener_pid(port: u16) -> Option<u32> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    for family in [AF_INET.0 as u32, AF_INET6.0 as u32] {
        // SAFETY: the size-then-buffer dance is the API's own contract; the buffer is owned
        // memory of the size the first call asked for, and every row read stays inside the
        // count the second call reported.
        unsafe {
            let mut size = 0u32;
            let _ = GetExtendedTcpTable(
                None,
                &mut size,
                false,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            );
            if size == 0 {
                continue;
            }
            let mut buf = vec![0u8; size as usize];
            if GetExtendedTcpTable(
                Some(buf.as_mut_ptr().cast()),
                &mut size,
                false,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            ) != 0
            {
                continue;
            }
            let count = *(buf.as_ptr() as *const u32);
            let rows = buf
                .as_ptr()
                .add(std::mem::size_of::<u32>())
                .cast::<MIB_TCPROW_OWNER_PID>();
            let fit = (buf.len().saturating_sub(4) / std::mem::size_of::<MIB_TCPROW_OWNER_PID>())
                as u32;
            for i in 0..count.min(fit) {
                let row = &*rows.add(i as usize);
                // dwLocalPort is network byte order; LISTENER rows are all in LISTEN state.
                if u16::from_be(row.dwLocalPort as u16) == port && row.dwOwningPid > 0 {
                    return Some(row.dwOwningPid);
                }
            }
        }
    }
    None
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

/// Keep this process's standard handles out of the next child's inheritance.
///
/// Rust's `Command::spawn` passes `bInheritHandles = TRUE` whenever a stdio is configured, and
/// Windows then hands the child EVERY inheritable handle this process holds — the parent's own
/// std handles included, even when the child's stdio was pointed elsewhere. A daemon started
/// under a wrapper that reads our stdout through a pipe (PowerShell's `& swiss start … |
/// Out-File`, a CI step, `| tee`) inherited that pipe, the wrapper waited for its EOF, and EOF
/// never came while the daemon lived: a finished `swiss start` looked hung, and deploy.ps1
/// never reached its proof step or released its lock (2026-09-20). The inherit flag is per
/// handle and per process — clearing it changes nothing about our own reads and writes, and
/// `Stdio::inherit()` children are unaffected (std duplicates the handle inheritable for them).
pub fn keep_std_handles_from_children() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    for raw in [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ] {
        if raw.is_null() {
            continue; // no such std handle in this process (a detached start)
        }
        let _ = clear_inherit(HANDLE(raw));
    }
}

/// Clear HANDLE_FLAG_INHERIT on one handle. Failure (an invalid or pseudo handle) is the
/// caller's to ignore: an uninheritable handle we could not touch is no worse than before.
fn clear_inherit(handle: windows::Win32::Foundation::HANDLE) -> WinResult<()> {
    use windows::Win32::Foundation::{SetHandleInformation, HANDLE_FLAGS, HANDLE_FLAG_INHERIT};
    // SAFETY: SetHandleInformation only flips flags on a handle this process owns; it takes no
    // pointers and closes nothing.
    unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
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

    fn inherit_flag(handle: windows::Win32::Foundation::HANDLE) -> bool {
        use windows::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
        let mut flags = 0u32;
        // SAFETY: writes one u32 through a valid pointer; the handle is ours and open.
        unsafe { GetHandleInformation(handle, &mut flags) }.expect("handle information");
        flags & HANDLE_FLAG_INHERIT.0 != 0
    }

    #[test]
    fn clear_inherit_takes_the_flag_off_a_handle_that_had_it() {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};
        // A file handle of our own, made inheritable the way a wrapper's pipe arrives: the
        // mechanism under test is the flag flip, on a handle no other test shares.
        let dir = std::env::temp_dir().join(format!("swiss-inherit-{}", crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let file = std::fs::File::create(dir.join("h")).expect("file");
        let handle = HANDLE(file.as_raw_handle());
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
            .expect("make inheritable");
        assert!(inherit_flag(handle), "precondition: the handle is inheritable");
        clear_inherit(handle).expect("clear");
        assert!(!inherit_flag(handle), "the flag is gone");
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scratch HKCU key of the test's own, deleted on drop - so a failing assert still
    /// leaves the profile as it found it, and the Run key and the PATH are never touched.
    struct ScratchKey(String);

    impl ScratchKey {
        fn new(tag: &str) -> Self {
            Self(format!("Software\\swiss-suite-{tag}-{}", crate::util::random_hex(8)))
        }
    }

    impl Drop for ScratchKey {
        fn drop(&mut self) {
            use windows::Win32::System::Registry::RegDeleteKeyW;
            let key = wide(&self.0);
            // SAFETY: the name buffer outlives the call; the key has values only, no subkeys.
            unsafe {
                let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()));
            }
        }
    }

    #[test]
    fn a_string_value_round_trips_with_its_type() {
        let key = ScratchKey::new("string");
        assert_eq!(hkcu_string_read(&key.0, "v"), Ok(None), "no key reads as absent");
        hkcu_string_write(&key.0, "v", "C:\\a b;%USERPROFILE%\\bin", true).expect("write");
        assert_eq!(
            hkcu_string_read(&key.0, "v"),
            Ok(Some(("C:\\a b;%USERPROFILE%\\bin".to_string(), true))),
            "REG_EXPAND_SZ reads back unexpanded, and says so"
        );
        hkcu_string_write(&key.0, "v", "plain", false).expect("rewrite");
        assert_eq!(hkcu_string_read(&key.0, "v"), Ok(Some(("plain".to_string(), false))));
        hkcu_value_remove(&key.0, "v").expect("remove");
        assert_eq!(hkcu_string_read(&key.0, "v"), Ok(None), "no value reads as absent");
        hkcu_value_remove(&key.0, "v").expect("removing a missing value is success");
    }

    #[test]
    fn removing_from_a_missing_key_is_success() {
        let key = ScratchKey::new("missing");
        hkcu_value_remove(&key.0, "v").expect("no key, nothing to remove");
    }

    #[test]
    fn a_value_that_is_not_a_string_is_an_error_not_absent() {
        // The PATH writer appends to what it read; a REG_DWORD read as "absent" would be
        // replaced by a one-entry PATH. It must refuse instead.
        use windows::Win32::System::Registry::{RegSetValueExW, REG_DWORD};
        let key = ScratchKey::new("dword");
        hkcu_string_write(&key.0, "s", "", false).expect("create the key");
        let sub = wide(&key.0);
        let name = wide("v");
        // SAFETY: the handle is opened and closed here; every buffer outlives its call.
        unsafe {
            let mut hkey = HKEY::default();
            assert_eq!(
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(sub.as_ptr()),
                    None,
                    REG_SAM_FLAGS(KEY_WRITE.0),
                    &mut hkey,
                ),
                ERROR_SUCCESS
            );
            let seven = 7u32.to_le_bytes();
            let set = RegSetValueExW(hkey, PCWSTR(name.as_ptr()), None, REG_DWORD, Some(&seven));
            let _ = RegCloseKey(hkey);
            assert_eq!(set, ERROR_SUCCESS);
        }
        let read = hkcu_string_read(&key.0, "v");
        assert!(read.as_ref().is_err_and(|e| e.contains("not a string")), "{read:?}");
    }

    #[test]
    fn expand_env_expands_known_references_and_keeps_unknown_ones() {
        let windir = std::env::var("SystemRoot").expect("Windows sets SystemRoot");
        assert_eq!(expand_env("%SystemRoot%\\x"), format!("{windir}\\x"));
        assert_eq!(
            expand_env("%SWISS_SURELY_UNSET_VAR%\\x"),
            "%SWISS_SURELY_UNSET_VAR%\\x"
        );
        assert_eq!(expand_env("C:\\plain"), "C:\\plain");
    }

    #[test]
    fn std_handles_are_not_inheritable_after_the_call() {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        // The daemon spawn's precondition: whatever the test runner handed us as stdout (a
        // pipe under cargo, a console in a terminal) is not inheritable once this ran.
        keep_std_handles_from_children();
        for raw in [std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()] {
            if raw.is_null() {
                continue;
            }
            assert!(!inherit_flag(HANDLE(raw)), "a std handle stayed inheritable");
        }
    }
}
