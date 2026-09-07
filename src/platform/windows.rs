//! The Windows platform seam — direct Win32 calls replacing every `powershell.exe` spawn the Node
//! build had to make. Each of those spawns cost ~65 MB of transient working set and ~350 ms; the
//! direct calls are free, which is why the memory view stops being opt-in in this build.

use windows::core::{w, Result as WinResult};
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};

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

/// `own_pid` and every descendant (any process type), via the Toolhelp parent->child walk —
/// the direct replacement for the Node build's PowerShell CIM query. A reaper uses it to refuse
/// to touch a stale-ledger pid the OS has since handed to one of THIS instance's own children.
pub fn descendant_pids(own_pid: u32) -> Vec<u32> {
    // SAFETY: the snapshot handle is closed on every path; PROCESSENTRY32W is initialized with
    // its own size as the API requires.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return vec![own_pid]; // no snapshot — degrade to "only self", safe (reaps nothing extra)
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

        let mut queue: std::collections::VecDeque<u32> =
            std::collections::VecDeque::from([own_pid]);
        let mut tree: std::collections::HashSet<u32> = std::collections::HashSet::new();
        while let Some(pid) = queue.pop_front() {
            if !tree.insert(pid) {
                continue;
            }
            for (child, parent) in parents.iter() {
                if *parent == pid {
                    queue.push_back(*child);
                }
            }
        }
        tree.into_iter().collect()
    }
}

/// Force-kill a process AND its whole descendant tree, any depth. On Windows killing a parent
/// does NOT cascade to children (the root cause of the Node build's orphan leak — an
/// `npx`-launched MCP leaves cmd.exe -> node -> the real server, and only the top dies), so the
/// tree is walked first and every member terminated explicitly. Replaces `taskkill /T /F` with
/// the same snapshot walk — no subprocess.
pub fn tree_kill(pid: u32) {
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    for victim in descendant_pids(pid) {
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

        let self_pid = std::process::id();
        let mut queue: std::collections::VecDeque<u32> = roots.iter().copied().collect();
        let mut tree: std::collections::HashSet<u32> = std::collections::HashSet::new();
        while let Some(pid) = queue.pop_front() {
            if pid == self_pid || !tree.insert(pid) {
                continue;
            }
            for (child, parent) in parents.iter() {
                if *parent == pid {
                    queue.push_back(*child);
                }
            }
        }

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
