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

# Live-verify boot helper for the swiss skill suite.
#
# Wraps the repo's own scripts/test-instance.ps1 (which owns the boot procedure and the
# state isolation) with the pieces every live check needs anyway: the legacy-name token pin,
# a stale-port guard, and the served-build proof (/health hash vs the exe's --version hash).
# Never touches 19999. Run from anywhere; the repo root is resolved from this script's path.
#
#   live-check.ps1              # build into target-test, boot snapshot instance, prove, leave up
#   live-check.ps1 -SkipBuild   # reuse the existing target-test exe
#   live-check.ps1 -Fresh       # wipe the test home first (clean-instance scenarios)
#   live-check.ps1 -Stop        # stop whatever owns port 19998 and exit

[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$Fresh,
    [switch]$Stop
)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..\..')).Path
$TestInstance = Join-Path $Root 'scripts\test-instance.ps1'
$Exe = Join-Path $Root 'target-test\release\swiss.exe'
$Port = 19998
# The same literal acceptance-16.ps1 pins: arbitrary, but deliberately the legacy name,
# because the snapshotted production config's tokenEnv still says MCP_GATEWAY_TOKEN.
$Token = 'acceptance-token-for-1998'

if ($Stop) { & $TestInstance -Stop; exit $LASTEXITCODE }

if (-not (Test-Path $TestInstance)) { Write-Error "not a swiss checkout: $TestInstance missing"; exit 1 }

# A stale instance from an earlier session holding the port would serve the WRONG build.
$owners = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue |
    Select-Object -ExpandProperty OwningProcess -Unique
if ($owners) {
    Write-Host "port $Port is already held (pid $($owners[0])); run this script with -Stop first"
    exit 1
}

if (-not $SkipBuild) {
    Write-Host '== building target-test exe'
    Push-Location $Root
    try {
        $env:CARGO_TARGET_DIR = 'target-test'
        cargo build --release
        if ($LASTEXITCODE -ne 0) { Write-Error 'build failed'; exit 1 }
    } finally { Pop-Location }
}

if (-not (Test-Path $Exe)) { Write-Error "no test binary at $Exe - drop -SkipBuild or build first"; exit 1 }

# Pin BEFORE boot so the started process (and its children) inherit the known bearer.
$env:MCP_GATEWAY_TOKEN = $Token

Write-Host '== booting the isolated 19998 instance'
if ($Fresh) { & $TestInstance -Fresh } else { & $TestInstance }
if ($LASTEXITCODE -ne 0) { Write-Error 'test instance failed to come up'; exit 1 }

# The proof: what is serving is THIS build. The hash may carry a suffix (-dirty on an
# uncommitted tree, which is exactly when live verification runs); compare it whole.
$version = (& $Exe --version) | Select-Object -First 1
if (-not ($version -match '\((?<hash>[0-9a-f]{7,}(?:-[0-9a-z]+)?)')) { Write-Error "no build hash in version line: $version"; exit 1 }
$built = $Matches.hash
$health = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 5
$hdr = @{ Authorization = "Bearer $Token" }
$info = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/info" -Headers $hdr -TimeoutSec 5

Write-Host "exe --version : $version"
Write-Host "health.build  : $($health.build.hash)"
Write-Host "info.build    : $($info.build.hash)  tokenEnv=$($info.tokenEnv)"
if ($health.build.hash -ne $built -or $info.build.hash -ne $built) {
    Write-Error "served build ($($health.build.hash)) is not this build ($built) - a stale instance answered"
    exit 1
}
Write-Host "19998 is serving this build; state writes go to %LOCALAPPDATA%\swiss-test-home"
Write-Host "bearer for /api/* probes: the pinned MCP_GATEWAY_TOKEN above"
exit 0
