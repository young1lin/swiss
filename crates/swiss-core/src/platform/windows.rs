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

/// One string value under one registry root: its text and whether it is REG_EXPAND_SZ.
/// Ok(None) when the key or the value is absent; an Err when it is there but cannot be read as
/// a string - which a writer must not mistake for "absent" and overwrite. The Run entry, the
/// user PATH and the machine PATH all read through here, and the suite drives it against a
/// scratch key - never the operator's own.
fn hkey_string_read(
    root: HKEY,
    root_label: &str,
    subkey: &str,
    value: &str,
) -> Result<Option<(String, bool)>, String> {
    let label = format!("{root_label}\\{subkey} value {value}");
    let subkey = wide(subkey);
    let value = wide(value);
    // SAFETY: both name buffers outlive the calls; the handle is closed on every path; the data
    // buffer is bounded by the length the size query reported.
    unsafe {
        let mut hkey = HKEY::default();
        let opened = RegOpenKeyExW(root, PCWSTR(subkey.as_ptr()), None, REG_SAM_FLAGS(KEY_READ.0), &mut hkey);
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

/// One string value under HKCU (the contract is on hkey_string_read).
fn hkcu_string_read(subkey: &str, value: &str) -> Result<Option<(String, bool)>, String> {
    hkey_string_read(HKEY_CURRENT_USER, "HKCU", subkey, value)
}

/// One string value under HKLM.
fn hklm_string_read(subkey: &str, value: &str) -> Result<Option<(String, bool)>, String> {
    hkey_string_read(HKEY_LOCAL_MACHINE, "HKLM", subkey, value)
}

/// The session-manager environment key holding the machine-wide PATH.
const MACHINE_ENV_KEY: &str =
    "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment";

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

/// The machine-wide PATH exactly as stored - %VAR% references unexpanded - and whether it is
/// REG_EXPAND_SZ. This is the second source of the PATH a detached spawn rebuilds each time
/// (SPEC §process.detached): a resident gateway's environment is a snapshot taken at boot, so
/// a tool the administrator installed after this gateway started must still be findable.
/// Ok(None) when the machine has no Path value; an Err when there is one that cannot be read
/// as a string.
pub fn machine_path_read() -> Result<Option<(String, bool)>, String> {
    hklm_string_read(MACHINE_ENV_KEY, PATH_VALUE)
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

/// One process's working set, in bytes - the children rows' memory column.
/// `None` when the pid cannot be opened (gone, or elevated): a row without a
/// reading shows none, never 0.
pub fn process_working_set(pid: u32) -> Option<u64> {
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

/// When a process was created, in Unix milliseconds. This is half of a Windows pid's identity:
/// the OS recycles pids quickly, so "is pid N the process I started" is answered by pid PLUS
/// creation time (SPEC §process.detached). None for a pid that cannot be opened - gone, or a
/// process that refuses PROCESS_QUERY_LIMITED_INFORMATION to this one.
pub fn process_created_at(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetProcessTimes;
    use windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
    if pid == 0 {
        return None;
    }
    // SAFETY: the handle is closed on every path; the four FILETIME structs are plain
    // out-parameters owned by this frame.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return None;
        };
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        ok.ok()?;
        Some(filetime_to_unix_ms(&creation))
    }
}

/// FILETIME - 100 ns ticks since 1601-01-01 UTC - to Unix milliseconds. Saturating, so a
/// misread zero struct clamps to the epoch rather than wrapping to a huge number.
fn filetime_to_unix_ms(t: &windows::Win32::Foundation::FILETIME) -> u64 {
    let ticks = ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    // 116444736000000000 ticks span 1601-01-01 to 1970-01-01.
    ticks.saturating_sub(116_444_736_000_000_000) / 10_000
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

/// How far up the tree [ancestor_env] looks: a shell, a wrapper or two, the agent's runtime,
/// its app.
const ANCESTOR_DEPTH: usize = 12;

/// The named variables as THIS process's ancestors hold them, nearest first (one pair per
/// ancestor that has one). An agent may keep a variable from the tool shell it starts - the
/// Codex app runs on its own CODEX_HOME and does not hand it on (2026-11-11) - so where the
/// agent keeps its state is read from the agent's own process. Answers what could be read: a
/// process that refuses PROCESS_VM_READ is passed over. A hint the caller checks against the
/// disk, never an identity claim.
pub fn ancestor_env(names: &[&str]) -> Vec<(String, String)> {
    // SAFETY: the snapshot handle is closed on every path; PROCESSENTRY32W is initialized with
    // its own size as the API requires.
    let parents = unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return Vec::new();
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
        parents
    };
    let mut found = Vec::new();
    let mut child = std::process::id();
    let mut child_born = process_created_at(child);
    for _ in 0..ANCESTOR_DEPTH {
        let Some(&parent) = parents.get(&child) else { break };
        if parent == 0 || parent == child {
            break;
        }
        let born = process_created_at(parent);
        // A parent that exited leaves its pid to be reused: a "parent" younger than its child
        // is a stranger, and so is everything above it.
        if born.zip(child_born).is_some_and(|(parent, child)| parent > child) {
            break;
        }
        for (name, value) in process_environment(parent).unwrap_or_default() {
            if names.iter().any(|wanted| wanted.eq_ignore_ascii_case(&name)) {
                found.push((name, value));
            }
        }
        child = parent;
        child_born = born;
    }
    found
}

/// Another process's environment block, read from its PEB: the PEB's ProcessParameters, then
/// RTL_USER_PROCESS_PARAMETERS' Environment and EnvironmentSize. The two offsets are the
/// 64-bit layout every Windows since Vista keeps (and a WOW64 process keeps a 64-bit PEB
/// beside its 32-bit one, which is the one ProcessBasicInformation names).
#[cfg(target_pointer_width = "64")]
fn process_environment(pid: u32) -> Option<Vec<(String, String)>> {
    use std::ffi::c_void;
    use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;

    /// PROCESS_BASIC_INFORMATION as the kernel writes it, declared here so the PEB type and
    /// its feature gates stay out of this crate.
    #[repr(C)]
    #[derive(Default)]
    struct BasicInformation {
        exit_status: i32,
        peb: usize,
        affinity_mask: usize,
        base_priority: i32,
        process_id: usize,
        parent_process_id: usize,
    }
    const PEB_PROCESS_PARAMETERS: usize = 0x20;
    const PARAMETERS_ENVIRONMENT: usize = 0x80;
    const PARAMETERS_ENVIRONMENT_SIZE: usize = 0x3F0;
    /// A block past this is not an environment worth reading.
    const ENVIRONMENT_MAX: usize = 1 << 20;

    // SAFETY: the handle is closed on every path; every read lands in a buffer this frame owns
    // and sized to the read; a failed read answers None and nothing is dereferenced.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
        let read = |at: usize, into: *mut c_void, len: usize| -> bool {
            ReadProcessMemory(handle, at as *const c_void, into, len, None).is_ok()
        };
        let block = (|| {
            let mut info = BasicInformation::default();
            let status = NtQueryInformationProcess(
                handle,
                ProcessBasicInformation,
                &mut info as *mut BasicInformation as *mut c_void,
                std::mem::size_of::<BasicInformation>() as u32,
                std::ptr::null_mut(),
            );
            if status.0 < 0 || info.peb == 0 {
                return None;
            }
            let mut parameters = 0usize;
            if !read(info.peb + PEB_PROCESS_PARAMETERS, &mut parameters as *mut usize as *mut c_void, 8) || parameters == 0 {
                return None;
            }
            let (mut environment, mut size) = (0usize, 0usize);
            if !read(parameters + PARAMETERS_ENVIRONMENT, &mut environment as *mut usize as *mut c_void, 8)
                || !read(parameters + PARAMETERS_ENVIRONMENT_SIZE, &mut size as *mut usize as *mut c_void, 8)
                || environment == 0
                || size < 2
            {
                return None;
            }
            let mut block = vec![0u16; size.min(ENVIRONMENT_MAX) / 2];
            read(environment, block.as_mut_ptr() as *mut c_void, block.len() * 2).then_some(block)
        })();
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        block.map(|block| environment_pairs(&block))
    }
}

#[cfg(not(target_pointer_width = "64"))]
fn process_environment(_pid: u32) -> Option<Vec<(String, String)>> {
    None
}

/// `NAME=VALUE\0...\0\0` (UTF-16) as pairs. A name never starts the entry with `=`: the
/// `=C:=C:\dir` entries cmd keeps per drive are skipped.
fn environment_pairs(block: &[u16]) -> Vec<(String, String)> {
    block
        .split(|unit| *unit == 0)
        .take_while(|entry| !entry.is_empty())
        // `=C:=C:\src` and its kind are cmd's per-drive working directories, not variables.
        .filter(|entry| entry.first() != Some(&u16::from(b'=')))
        .filter_map(|entry| {
            let text = String::from_utf16_lossy(entry);
            let cut = text.get(1..)?.find('=')? + 1;
            Some((text[..cut].to_string(), text[cut + 1..].to_string()))
        })
        .collect()
}

/// The pid that owns a LISTENING socket on `port`, for either IP family. `None` =
/// nothing is listening or the table could not be read; the caller's diagnostic words cover
/// both. Now a lookup into the full table read (see `tcp_listeners`), so the two never
/// disagree about what the OS said.
pub fn tcp_listener_pid(port: u16) -> Option<u32> {
    tcp_listeners()
        .ok()?
        .into_iter()
        .find(|(p, _)| *p == port)
        .map(|(_, pid)| pid)
}

/// Every LISTENING TCP socket on the machine, as (local port, owning pid), IPv4 and IPv6, on
/// any local address — one GetExtendedTcpTable pass per family with the listener-only class
/// (SPEC §process.apps: one snapshot answers every managed app at once). A pid of 0 is never
/// returned here; on Windows every owner is readable. Err when the OS table itself cannot be
/// read - callers word that as "the listener table cannot be read", never as "nothing
/// listens".
pub fn tcp_listeners() -> std::io::Result<Vec<(u16, u32)>> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    let mut out = Vec::new();
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
                continue; // no sockets of this family - an empty half, not a failure
            }
            let mut buf = vec![0u8; size as usize];
            let code = GetExtendedTcpTable(
                Some(buf.as_mut_ptr().cast()),
                &mut size,
                false,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            );
            if code != 0 {
                // Half a table is worse than none: a port read on the missing family would
                // verdict "stopped" for an app that is running. Fail the whole read instead.
                return Err(std::io::Error::from_raw_os_error(code as i32));
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
                let port = u16::from_be(row.dwLocalPort as u16);
                if row.dwOwningPid > 0 {
                    out.push((port, row.dwOwningPid));
                }
            }
        }
    }
    Ok(out)
}

/// A memoizing creation-time lookup for the walks below: GetProcessTimes once per pid the
/// walk REACHES, never once per edge and never for the whole snapshot (SPEC §process.tree).
fn created_at_probe() -> impl FnMut(u32) -> Option<u64> {
    let mut cache: std::collections::HashMap<u32, Option<u64>> = std::collections::HashMap::new();
    move |pid| *cache.entry(pid).or_insert_with(|| process_created_at(pid))
}

/// `own_pid` and every descendant (any process type), via the Toolhelp parent->child walk —
/// the direct replacement for the Node build's PowerShell CIM query. A reaper uses it to refuse
/// to touch a stale-ledger pid the OS has since handed to one of THIS instance's own children.
///
/// The walk is a KILL-side walk (SPEC §process.tree): each parent edge is checked against
/// creation times, and an edge whose times cannot be read does not count, so a reused pid
/// never swells this set with strangers it "adopted".
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
        let tree = crate::platform::walk_descendants(
            &parents,
            &[own_pid],
            None,
            crate::platform::WalkMode::Kill,
            &mut created_at_probe(),
        );
        Some(tree.into_iter().collect())
    }
}

/// How long a successfully-terminated victim is given to be observed dead. TerminateProcess is
/// asynchronous; in practice the exit lands within the first sleep, and only a victim wedged in
/// kernel cleanup ever uses the whole window.
const TERMINATE_CONFIRM_MS: u64 = 1000;

/// Force-kill a process AND its whole descendant tree, any depth, returning the pids that were
/// STILL ALIVE after it ran (SPEC §process.tree). On Windows killing a parent does NOT cascade
/// to children (the root cause of the Node build's orphan leak — an `npx`-launched MCP leaves
/// cmd.exe -> node -> the real server, and only the top dies), so the tree is walked first
/// (creation-time checked, Kill mode) and every member terminated explicitly. Replaces
/// `taskkill /T /F` with the same snapshot walk — no subprocess.
///
/// A survivor is a victim that refused termination and still stands — an elevated process
/// refusing PROCESS_TERMINATE to a non-elevated gateway, the exact case the caller must name
/// instead of reporting "stopped". This is a synchronous call and may briefly sleep while
/// confirming deaths; callers on the async runtime wrap it in a blocking task.
pub fn tree_kill(pid: u32) -> Vec<u32> {
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    // An unknown tree still kills the root itself (it was named explicitly); only the un-walked
    // descendants are at risk of leaking, and the proc children's kill-on-close job object is
    // the backstop for exactly that.
    let victims = descendant_pids(pid).unwrap_or_else(|| vec![pid]);
    let mut survivors = Vec::new();
    let mut terminated = Vec::new();
    for victim in victims {
        if victim == std::process::id() {
            continue; // a corrupted snapshot edge must never turn this into suicide
        }
        // SAFETY: the handle is closed on every path; TerminateProcess takes no pointers.
        let killed = unsafe {
            match OpenProcess(PROCESS_TERMINATE, false, victim) {
                Ok(handle) => {
                    let ok = TerminateProcess(handle, 1);
                    let _ = windows::Win32::Foundation::CloseHandle(handle);
                    ok.is_ok()
                }
                Err(_) => false,
            }
        };
        if killed {
            terminated.push(victim);
        } else if pid_alive(victim) {
            // Not terminable and still standing: report it. A pid that vanished between the
            // walk and the open is not a survivor - it is dead.
            survivors.push(victim);
        }
    }
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(TERMINATE_CONFIRM_MS);
    while !terminated.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        terminated.retain(|p| pid_alive(*p));
    }
    survivors.extend(terminated);
    survivors
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

/// The spawn posture of a program that must OUTLIVE the gateway (SPEC §process.detached):
/// CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB.
///
/// Each flag avoids one Windows trap. BREAKAWAY: a child inherits its parent's job, and inside
/// the IDE/agent job a gateway runs under, an app would die the moment that tool closed its
/// handle (a job forbidding breakaway makes the spawn fail with os error 5; the caller then
/// retries without the flag and says the app shares the gateway's job). NEW_PROCESS_GROUP:
/// CTRL_CLOSE_EVENT on a shared console reaches everything attached to it, so the gateway's
/// group must not be the app's. NO_WINDOW, never DETACHED_PROCESS: a console-less process
/// makes its first console GRANDCHILD (`cmd` under `npx.cmd`) open a new visible window,
/// while this combination gives each app its own hidden console (one conhost.exe each).
pub fn detach_command(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
}

/// The retry of [`detach_command`] its own doc promises (SPEC §process.detached): the same
/// console isolation minus the escape, for a job that forbids breakaway (os error 5). The
/// child then dies when that job's owner closes its handle — which is exactly why the
/// caller must put "shares the gateway's job" in the start's message.
pub fn detach_command_without_breakaway(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
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
        // in the subtree total would show it twice. Measure mode: an edge whose creation times
        // cannot be read still counts here - a wrong number is repaired next poll (SPEC
        // §process.tree).
        let tree = crate::platform::walk_descendants(
            &parents,
            roots,
            Some(std::process::id()),
            crate::platform::WalkMode::Measure,
            &mut created_at_probe(),
        );

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
    fn an_environment_block_reads_as_pairs() {
        let block: Vec<u16> = "=C:=C:\\dev\0CODEX_HOME=D:\\agents\\codex-work\0A=b=c\0\0junk"
            .encode_utf16()
            .collect();
        assert_eq!(
            environment_pairs(&block),
            [
                ("CODEX_HOME".to_string(), r"D:\agents\codex-work".to_string()),
                ("A".to_string(), "b=c".to_string()),
            ]
        );
    }

    #[test]
    fn the_parents_environment_is_read_from_its_own_process() {
        // The test binary's parent (cargo, a shell) has a PATH of its own: reading it proves
        // the PEB walk lands on a real environment block, not on what this process holds.
        let paths = ancestor_env(&["PATH"]);
        assert!(!paths.is_empty(), "no ancestor's PATH was read");
        assert!(paths.iter().all(|(name, value)| name.eq_ignore_ascii_case("PATH") && !value.is_empty()));
        assert!(ancestor_env(&["SWISS_NO_SUCH_ANCESTOR_VAR"]).is_empty());
    }

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
