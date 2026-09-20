# Copyright 2026 The swiss authors
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

# One-step deploy to production 19999 (docs/16 H3).
#
# The deploy used to be a three-step ritual (stop, build, start) where doing it out of order
# failed with "Access is denied (os error 5)" - the linker cannot overwrite the exe a running
# daemon holds - and nothing anywhere said WHICH build was running afterwards. This script
# fixes the order, then proves the result: /health must report the hash this build stamped
# into swiss.exe --version, or the script fails loudly with both values.
#
# Run it from the repo root; a deploy is the operator's decision, from whichever shell they
# choose (H1 scrubs the daemon's environment, so an agent or CI shell is safe too). Until
# 2026-09-20 a shell that CAPTURED this script's output (`| Out-File`, a tool wrapper, `| tee`)
# hung after "gateway started": the daemon inherited the wrapper's stdout pipe and the wrapper
# waited for an EOF that never came, so the proof step below never ran and the lock stayed.
# `swiss start` now keeps its std handles out of the daemon's inheritance (swiss-core
# platform::keep_std_handles_from_children), and a captured deploy runs to the end.
# -SkipGates jumps the test/clippy gates for a hotfix; the default is to run them.
#
# The single-deployer lock (deploy-lock.ps1) is taken FIRST and held for the whole run: the
# 2026-09-14 incident was two sessions deploying the same production four minutes apart,
# each unaware the other had already stopped it. Phase lines also land in the gateway home's
# deploy.log as they happen, so a background deploy can be watched even when its stdout is
# buffered away by a wrapper.
[CmdletBinding()]
param(
    [switch]$SkipGates
)
$ErrorActionPreference = 'Stop'
$Exe = 'target\release\swiss.exe'
. "$PSScriptRoot\deploy-lock.ps1"
function Fail($message) {
    Write-Host $message -ForegroundColor Red
    Phase "FAILED: $message"
    exit 1
}
function Phase($message) {
    # Timestamped everywhere, immediately: console (flushed, so a piped consumer sees
    # progress as it happens) and the gateway home's deploy.log (survives wrappers that
    # buffer stdout into silence).
    # The home is resolved per line, not once: the start below may move a pre-rename
    # ~\.mcp-gateway to ~\.swiss (deploy.log travels with it), and the lines after that
    # must land where the log now lives.
    Write-Host "== $message"
    try {
        $log = Join-Path (Get-SwissProdHome) 'deploy.log'
        Add-Content -Path $log -Value ("{0} deploy: {1}" -f (Get-Date).ToString('s'), $message)
    } catch { }
    [Console]::Out.Flush()
}
# Mutual exclusion before anything else - gates included: a 90-minute gate window is
# exactly long enough for another session to start a deploy of its own.
$lock = Acquire-DeployLock
if (-not $lock.Acquired) {
    $who = if ($lock.Holder) { "pid $($lock.Holder.pid) (started $($lock.Holder.started))" } else { "an unknown holder" }
    Fail "another deploy holds the lock - $who; if that pid is dead it is stale, delete $($lock.LockPath) and re-run"
}
if ($lock.StaleTookOver) {
    Phase "took over a STALE deploy lock (previous holder is gone)"
}
try {
# Gates first, while production is still up: a failing gate must not take the daemon down.
if (-not $SkipGates) {
    Phase 'cargo test --workspace'
    cargo test --workspace
    if ($LASTEXITCODE -ne 0) { Fail "tests failed - production left untouched" }
    Phase 'cargo clippy --workspace --all-targets -- -D warnings'
    cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { Fail "clippy failed - production left untouched" }
    # The panel's own gate (docs/37 R6): typecheck + lint + emit-freshness + vitest, run
    # where its package.json lives. A stale emit is a failure here, so a deploy can never
    # serve a panel whose committed JS predates its TypeScript sources.
    Phase 'panel: npm run check (crates/swiss-panel/panel)'
    Push-Location 'crates\swiss-panel\panel'
    npm run check
    $panelCode = $LASTEXITCODE
    Pop-Location
    if ($panelCode -ne 0) { Fail "panel check failed - production left untouched" }
} else {
    Phase "gates SKIPPED (-SkipGates)"
}
# Stop BEFORE building: the linker cannot overwrite the exe the running daemon holds
# (os error 5). Exit 3 = nothing was running, which is fine to deploy over.
Phase 'stopping the daemon'
& $Exe stop
if ($LASTEXITCODE -eq 1) { Fail "stop was refused - resolve it by hand (see above), then re-run" }
Phase 'cargo build --release'
cargo build --release
if ($LASTEXITCODE -ne 0) { Fail "build failed - start the old daemon again by hand: & $Exe start --no-open" }
Phase 'starting the new daemon'
& $Exe start --no-open
if ($LASTEXITCODE -ne 0) { Fail "start failed - log tail above" }
& $Exe status
if ($LASTEXITCODE -ne 0) { Fail "status says the daemon is not running" }
# The proof: the daemon now serving is THIS build, not the previous one still holding the port.
$version = (& $Exe --version) | Select-Object -First 1
if (-not ($version -match '^swiss \S+ \((?<hash>[^,]+), ')) {
    Fail "cannot read a build hash from version line: $version"
}
$built = $Matches.hash
$health = Invoke-WebRequest -Uri "http://127.0.0.1:19999/health" -UseBasicParsing -TimeoutSec 5
$running = ($health.Content | ConvertFrom-Json).build.hash
if ($running -ne $built) {
    Fail "deploy did not take: running daemon is $running, freshly built is $built"
}
Phase "deployed: $running is serving on 19999"
} finally {
    # The lock MUST go even on failure - a failed deploy that keeps the lock blocks the
    # retry that fixes it. Stale takeover inside the run is Release's problem, not ours.
    [void](Release-DeployLock)
}
exit 0