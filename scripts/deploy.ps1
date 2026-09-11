# One-step deploy to production 19999 (docs/16 H3).
#
# The deploy used to be a three-step ritual (stop, build, start) where doing it out of order
# failed with "Access is denied (os error 5)" - the linker cannot overwrite the exe a running
# daemon holds - and nothing anywhere said WHICH build was running afterwards. This script
# fixes the order, then proves the result: /health must report the hash this build stamped
# into swiss.exe --version, or the script fails loudly with both values.
#
# Run it from YOUR OWN terminal at the repo root, not from an agent tool shell: a deploy is
# the operator's decision (H1 scrubs the daemon's environment anyway, but the habit stands).
# -SkipGates jumps the test/clippy gates for a hotfix; the default is to run them.

[CmdletBinding()]
param(
    [switch]$SkipGates
)

$ErrorActionPreference = 'Stop'
$Exe = 'target\release\swiss.exe'

function Fail($message) {
    Write-Host $message -ForegroundColor Red
    exit 1
}

# Gates first, while production is still up: a failing gate must not take the daemon down.
if (-not $SkipGates) {
    Write-Host '== cargo test --workspace'
    cargo test --workspace
    if ($LASTEXITCODE -ne 0) { Fail "tests failed - production left untouched" }
    Write-Host '== cargo clippy --workspace --all-targets -- -D warnings'
    cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { Fail "clippy failed - production left untouched" }
} else {
    Write-Host "== gates SKIPPED (-SkipGates)"
}

# Stop BEFORE building: the linker cannot overwrite the exe the running daemon holds
# (os error 5). Exit 3 = nothing was running, which is fine to deploy over.
Write-Host '== stopping the daemon'
& $Exe stop
if ($LASTEXITCODE -eq 1) { Fail "stop was refused - resolve it by hand (see above), then re-run" }

Write-Host '== cargo build --release'
cargo build --release
if ($LASTEXITCODE -ne 0) { Fail "build failed - start the old daemon again by hand: & $Exe start --no-open" }

Write-Host '== starting the new daemon'
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
Write-Host "deployed: $running is serving on 19999"
exit 0

