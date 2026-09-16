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

//! The Windows Job Object seam (ADR-008): one owned kill-on-close handle per spawned child.
//!
//! This lived in `swiss-host`'s process supervisor until the local PTY needed it too. A pty child
//! is spawned by [`crate::platform::pty`], which sits BELOW the host, so the guard moved down
//! here rather than growing a second copy — and the services layer lost its last `unsafe` on the
//! way. Every child this gateway starts (a proc MCP, a scheduled job, a local terminal) is
//! assigned through this one function, so "a hard-killed gateway leaves no orphans" is one
//! mechanism to trust rather than four to keep in step.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// An owned kill-on-close job handle. Dropping it closes the handle, and the job carries
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` — so the drop TERMINATES every process in the job: the
/// child plus its whole subtree, since processes created by a job member inherit the job. This is
/// what makes both a graceful close and a hard gateway kill tear down the tree.
pub struct KillOnCloseJob(HANDLE);

// SAFETY: the HANDLE inside is owned exclusively by this wrapper (never cloned out), and a Win32
// job handle is a plain kernel object reference that CloseHandle accepts from any thread — so
// moving or sharing the wrapper is as safe as moving the integer it holds.
unsafe impl Send for KillOnCloseJob {}
unsafe impl Sync for KillOnCloseJob {}

impl KillOnCloseJob {
    /// Put the just-spawned child (by its raw process handle) into a fresh kill-on-close job.
    /// `None` when the job cannot be created, configured or assigned — the caller falls back to
    /// killing by PID.
    pub fn assign(child: *mut core::ffi::c_void) -> Option<Self> {
        // SAFETY: null attributes and a null name are the documented "default security, unnamed
        // object" form; the returned handle is checked before any use.
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is a live local outliving the call; the class/length pair matches its
        // type exactly as SetInformationJobObject documents.
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured.is_err() {
            // SAFETY: plain close of an owned, member-less job handle.
            unsafe {
                let _ = CloseHandle(job);
            };
            return None;
        }
        // SAFETY: the child handle comes straight from the spawn and stays open for the child's
        // lifetime; assignment only records job membership.
        let assigned = unsafe { AssignProcessToJobObject(job, HANDLE(child)) };
        if assigned.is_err() {
            // SAFETY: as above — the job has no members once assignment failed.
            unsafe {
                let _ = CloseHandle(job);
            };
            return None;
        }
        Some(Self(job))
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        // SAFETY: the handle is owned (never cloned or shared) and closed exactly here; closing a
        // kill-on-close job handle is precisely the subtree-kill mechanism.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
