# Copyright 2026 young1lin
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     https://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# Single-deployer lock for the production gateway - dot-sourced by deploy.ps1 and its test.
#
# The 2026-09-14 incident: two agent sessions deployed the SAME production four minutes
# apart. Session A's `swiss stop` had just landed when session B saw "production down",
# restarted the OLD exe "to repair it", and that daemon then locked the file session A's
# build was replacing (os error 5). Nobody was at fault and every piece behaved as
# designed - there was simply no mutual exclusion between deployers.
#
# The lock file lives in the GATEWAY HOME, not the repo: worktrees and sibling checkouts
# deploy the same daemon from different build trees, and only the shared home sees them
# all. Creation is exclusive (FileShare None), so two acquires cannot both win; a lock
# whose holder pid is dead is stale by definition (crash, hard kill) and is taken over
# with a warning - a lock file must never outlive its ability to block.
# The production home: ~\.swiss.
function Get-SwissProdHome {
    if ($env:SWISS_HOME) { return $env:SWISS_HOME }
    Join-Path $env:USERPROFILE '.swiss'
}
function Get-DeployLockPath {
    param([string]$Path)
    if ($Path) { return $Path }
    Join-Path (Get-SwissProdHome) 'deploy.lock'
}
function Test-PidAlive([int]$Pid_) {
    if ($Pid_ -le 0) { return $false }
    [bool](Get-Process -Id $Pid_ -ErrorAction SilentlyContinue)
}
# Returns a hashtable @{ Acquired = $bool; Holder = <hashtable or $null>; StaleTookOver = <bool> }.
# NEVER throws for a contended lock - a refusal is data the caller reports, not an error.
function Acquire-DeployLock {
    param([string]$Path)
    $lockPath = Get-DeployLockPath -Path $Path
    $dir = Split-Path $lockPath -Parent
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    $takeOver = $false
    if (Test-Path $lockPath) {
        $holder = $null
        try { $holder = Get-Content $lockPath -Raw | ConvertFrom-Json } catch { }
        if ($holder -and (Test-PidAlive -Pid_ $holder.pid)) {
            return @{ Acquired = $false; Holder = $holder; StaleTookOver = $false; LockPath = $lockPath }
        }
        # Dead holder or unparsable file: nothing is deploying, the file just outlived its writer.
        $takeOver = $true
        Remove-Item $lockPath -Force -ErrorAction SilentlyContinue
    }
    # Exclusive create: the second of two racing acquires gets an IOException, not the lock.
    $record = @{
        pid       = $PID
        started   = (Get-Date).ToString('o')
        script    = 'deploy.ps1'
        pwsh      = $PSHOME
    }
    try {
        $fs = [System.IO.File]::Open($lockPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    } catch [System.IO.IOException] {
        # Lost the race: re-read for the winner's record and report the refusal.
        $winner = $null
        try { $winner = Get-Content $lockPath -Raw | ConvertFrom-Json } catch { }
        return @{ Acquired = $false; Holder = $winner; StaleTookOver = $false; LockPath = $lockPath }
    }
    try {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes(($record | ConvertTo-Json -Compress))
        $fs.Write($bytes, 0, $bytes.Length)
    } finally { $fs.Dispose() }
    return @{ Acquired = $true; Holder = $record; StaleTookOver = $takeOver; LockPath = $lockPath }
}
# Releases only OUR lock: after a stale takeover the path may already name the next
# deployer, and releasing that one would re-open the race the lock exists to close.
function Release-DeployLock {
    param([string]$Path)
    $lockPath = Get-DeployLockPath -Path $Path
    if (-not (Test-Path $lockPath)) { return $false }
    $holder = $null
    try { $holder = Get-Content $lockPath -Raw | ConvertFrom-Json } catch { }
    if ($holder -and $holder.pid -ne $PID) { return $false }
    Remove-Item $lockPath -Force -ErrorAction SilentlyContinue
    return $true
}