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

# One-step deploy to production 19999 (SPEC §host.daemon, bin\ since 2026-09-21).
#
# The deploy used to be a three-step ritual (stop, build, start) where doing it out of order
# failed with "Access is denied (os error 5)" - the linker cannot overwrite the exe a running
# daemon holds - and nothing anywhere said WHICH build was running afterwards. This script
# fixes the order, then proves the result: /health must report the hash this build stamped
# into swiss.exe --version, or the script fails loudly with both values.
#
# Production runs from bin\swiss.exe, a copy of the build output, never from target\ itself
# (SPEC §host.ops): the linker and the daemon no longer share a file, so the build happens
# while the old daemon is still serving and the outage is stop + copy + start - seconds, not
# the four minutes a release build takes. Since 2026-09-28 the new daemon also binds the port
# BEFORE starting its plugins (src/server.rs), so "start" is a second rather than however long
# the slowest MCP takes to wake; the last line of a deploy states the measured outage. bin\ is
# a stable path for PATH and for
# `swiss autostart on`, and git ignores it. A daemon still running out of target\ (the
# pre-bin layout) is stopped BEFORE the build, once, so the linker can write.
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
$Built = 'target\release\swiss.exe'     # what cargo writes
$BinDir = 'bin'
$Exe = Join-Path $BinDir 'swiss.exe'     # what 19999 runs
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
    # The home is resolved per line, not once, so every line lands in the home the
    # gateway is actually using — including one named by $SWISS_HOME.
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
    # Gate 2 (SPEC §testing.it): the real-database suite. The deploy machine is this one and
    # Docker lives in WSL at tcp://127.0.0.1:2375 - set DOCKER_HOST for the run the
    # same way every dev shell does. A diff that never touched a database path can
    # still afford the ~20 s: a deploy is the last place to learn the engines broke.
    Phase 'cargo test -p swiss-it --features it'
    $env:DOCKER_HOST = 'tcp://127.0.0.1:2375'
    cargo test -p swiss-it --features it
    $gate2 = $LASTEXITCODE
    Remove-Item Env:\DOCKER_HOST -ErrorAction SilentlyContinue
    if ($gate2 -ne 0) { Fail "integration gate failed - production left untouched" }
    Phase 'cargo clippy --workspace --all-targets -- -D warnings'
    cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { Fail "clippy failed - production left untouched" }
    # The panel's own gate (SPEC §panel.toolchain): typecheck + lint + emit-freshness + vitest, run
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
# The daemon that is up right now, from its pid file: only a daemon running out of target\
# (the pre-bin layout) has to stop before the build, because the linker must overwrite the
# very file it holds. One running from bin\ keeps serving through the build.
$stopper = if (Test-Path $Exe) { $Exe } else { $Built }
$entry = $null
try {
    $pidFile = Join-Path (Get-SwissProdHome) 'gateway-19999.pid'
    if (Test-Path $pidFile) { $entry = (Get-Content $pidFile -Raw | ConvertFrom-Json).entry }
} catch { }
$holdsBuildOutput = $entry -and ((Resolve-Path $Built -ErrorAction SilentlyContinue).Path -eq $entry)
if ($holdsBuildOutput) {
    Phase "stopping the daemon first: it runs out of $Built, which the build must overwrite"
    & $stopper stop
    if ($LASTEXITCODE -eq 1) { Fail "stop was refused - resolve it by hand (see above), then re-run" }
}
Phase 'cargo build --release'
cargo build --release
if ($LASTEXITCODE -ne 0) {
    if ($holdsBuildOutput) { Fail "build failed - start the old daemon again by hand: & $stopper start --no-open" }
    Fail "build failed - production left untouched"
}
# From here the outage clock runs: stop, copy, start. It is measured and reported (2026-09-28),
# because "how long was 19999 down" is the number this whole ordering exists to keep small and
# nothing used to state it.
$outage = [System.Diagnostics.Stopwatch]::StartNew()
if (-not $holdsBuildOutput) {
    Phase 'stopping the daemon'
    & $stopper stop
    if ($LASTEXITCODE -eq 1) { Fail "stop was refused - resolve it by hand (see above), then re-run" }
}
# A `stop` that found no pid record says "not running" and exits 0 - which is also what it says
# when a daemon IS serving whose record was lost (the 2026-09-28 start-timeout bug). Copying
# over an exe that process still holds fails with "Access is denied", eight words away from the
# cause. Ask the port instead: it answers for whatever is actually there.
$stillUp = $null
try { $stillUp = Invoke-WebRequest -Uri 'http://127.0.0.1:19999/health' -UseBasicParsing -TimeoutSec 2 } catch { }
if ($stillUp) {
    $owner = (Get-NetTCPConnection -LocalPort 19999 -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1).OwningProcess
    Fail "something is still serving 19999 (pid $owner) after the stop - its pid record is probably gone; stop it by hand (POST http://127.0.0.1:19999/api/shutdown, or Stop-Process -Id $owner), then re-run"
}
Phase "installing $Exe"
if (-not (Test-Path $BinDir)) { [void](New-Item -ItemType Directory -Path $BinDir) }
# The stopped process can take a moment to let go of its exe; the copy retries, not fails.
$copied = $false
foreach ($attempt in 1..20) {
    try { Copy-Item -Path $Built -Destination $Exe -Force; $copied = $true; break } catch { Start-Sleep -Milliseconds 250 }
}
if (-not $copied) { Fail "could not overwrite $Exe - something still holds it (the old daemon?): start it again by hand: & $Exe start --no-open" }
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
$outage.Stop()
Phase ("deployed: $running is serving on 19999 from $Exe (19999 was down {0:N1} s)" -f $outage.Elapsed.TotalSeconds)
# Other programs reach `swiss remote ...` through PATH; the script only says when bin\ is
# missing from it - changing a user's PATH is their call.
$binAbs = (Resolve-Path $BinDir).Path
$onPath = ($env:PATH -split ';') | Where-Object { $_ -and ((Resolve-Path $_ -ErrorAction SilentlyContinue).Path -eq $binAbs) }
if (-not $onPath) {
    Write-Host "note: $binAbs is not on PATH; to call swiss from anywhere add it (User scope): [Environment]::SetEnvironmentVariable('Path', ([Environment]::GetEnvironmentVariable('Path','User') + ';$binAbs'), 'User')"
}
} finally {
    # The lock MUST go even on failure - a failed deploy that keeps the lock blocks the
    # retry that fixes it. Stale takeover inside the run is Release's problem, not ours.
    [void](Release-DeployLock)
}
exit 0