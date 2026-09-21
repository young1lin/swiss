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

# Tests for scripts/deploy-lock.ps1 - the mutual exclusion deploy.ps1 relies on.
# Run: pwsh -File scripts/tests/deploy-lock.test.ps1  (or powershell.exe -File).
# Uses a temp lock path throughout; the production lock file is never touched.
$ErrorActionPreference = 'Stop'
$failures = 0
function Check($name, $cond) {
    if ($cond) { Write-Host "  PASS $name" }
    else { Write-Host "  FAIL $name" -ForegroundColor Red; $script:failures++ }
}

. "$PSScriptRoot\..\deploy-lock.ps1"
$lockPath = Join-Path ([System.IO.Path]::GetTempPath()) ("deploy-lock-test-" + [guid]::NewGuid() + ".lock")

try {
    # 1. Fresh acquire.
    $r1 = Acquire-DeployLock -Path $lockPath
    Check 'fresh acquire succeeds' $r1.Acquired
    Check 'fresh acquire is not a takeover' (-not $r1.StaleTookOver)

    # 2. Contention: our own pid is alive, so a second acquire must be refused and
    #    report the holder - this is exactly what a concurrent deploy session sees.
    $r2 = Acquire-DeployLock -Path $lockPath
    Check 'second acquire is refused' (-not $r2.Acquired)
    Check 'refusal reports the holder pid' ($r2.Holder -and $r2.Holder.pid -eq $PID)

    # 3. Release, then acquire again.
    Check 'release succeeds' (Release-DeployLock -Path $lockPath)
    $r3 = Acquire-DeployLock -Path $lockPath
    Check 'acquire after release succeeds' $r3.Acquired
    [void](Release-DeployLock -Path $lockPath)

    # 4. Stale takeover: a dead holder's lock must not block the next deployer.
    $deadPid = 1073741823  # INT32_MAX: never a live pid on this machine
    @{ pid = $deadPid; started = (Get-Date).ToString('o'); script = 'deploy.ps1' } | ConvertTo-Json -Compress | Set-Content -Path $lockPath
    $r4 = Acquire-DeployLock -Path $lockPath
    Check 'stale lock is taken over' ($r4.Acquired -and $r4.StaleTookOver)

    # 5. Release must not remove a LIVE foreign holder's lock (post-takeover safety).
    $foreignPid = (Get-Process | Where-Object { $_.Id -ne $PID } | Select-Object -First 1).Id
    @{ pid = $foreignPid; started = (Get-Date).ToString('o'); script = 'deploy.ps1' } | ConvertTo-Json -Compress | Set-Content -Path $lockPath
    Check 'foreign live lock is not released' (-not (Release-DeployLock -Path $lockPath))
    Check 'foreign lock file still present' (Test-Path $lockPath)
} finally {
    Remove-Item $lockPath -Force -ErrorAction SilentlyContinue
}

if ($failures -gt 0) {
    Write-Host "$failures FAILED" -ForegroundColor Red
    exit 1
}
Write-Host 'all deploy-lock tests passed'
exit 0