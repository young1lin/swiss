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

# Live acceptance for SPEC §host.daemon, §host.ops on the isolated 19998 instance.
# Never touches 19999 except a read-only /health at the very end.
# Since SPEC §host.session /api/* needs the admin session: every API call goes through `swiss api`, which
# signs it with the test home's CLI key (never printed). H1's terminal-env probe used a raw
# WebSocket that cannot carry the key; it is covered by swiss-terminal's
# a_local_pwsh_keeps_its_colours_under_a_no_color_launcher instead.
$ErrorActionPreference = "Continue"
$Token = "acceptance-token-for-1998"
# The token pin deliberately stays on the legacy name: the boot's one-shot rename flips the
# snapshot's tokenEnv to SWISS_TOKEN, and this script is the regression proof that the Node-era
# pin still authenticates through the pair partner after that rename.
$env:SWISS_TOKEN = $Token
$Exe = "target-test\release\swiss.exe"
$Health98 = "http://127.0.0.1:19998/health"
$TestHome = Join-Path $env:LOCALAPPDATA "swiss-test-home"

# One admin API call against 19998, signed by the test home's CLI key.
function Invoke-Api([string]$Method, [string]$Path, [string]$Body = $null) {
  $prevHome = $env:SWISS_HOME; $prevPort = $env:SWISS_PORT
  $env:SWISS_HOME = $TestHome; $env:SWISS_PORT = "19998"
  try {
    if ($Body) { $out = & $Exe api $Method $Path $Body } else { $out = & $Exe api $Method $Path }
    if ($LASTEXITCODE -ne 0) { throw ("swiss api " + $Method + " " + $Path + " failed") }
    return ($out | Out-String | ConvertFrom-Json)
  } finally {
    $env:SWISS_HOME = $prevHome; $env:SWISS_PORT = $prevPort
  }
}

Write-Output "=== [0] launcher env (this very shell) ==="
Write-Output ("NO_COLOR=[" + $env:NO_COLOR + "] WT_SESSION=[" + $env:WT_SESSION + "] CLAUDECODE=[" + $env:CLAUDECODE + "]")

$prodCfg = Join-Path $env:USERPROFILE ".swiss\gateway.config.json"
$prodBefore = (Get-Item $prodCfg).LastWriteTimeUtc
Write-Output ("=== [1] prod config mtime BEFORE: " + $prodBefore.ToString("o"))

Write-Output "=== [2] starting the isolated instance ==="
& scripts\test-instance.ps1
if ($LASTEXITCODE -ne 0) { Write-Output "START FAILED"; exit 1 }

Write-Output "=== [3] H3: --version vs /health vs /api/info ==="
$ver = (& $Exe --version | Select-Object -First 1)
Write-Output ("version line : " + $ver)
$health = Invoke-RestMethod -Uri $Health98 -TimeoutSec 5
Write-Output ("health build : " + $health.build.hash + " (" + $health.build.time + ")")
$info = Invoke-Api GET /api/info
Write-Output ("api/info     : tokenEnv=" + $info.tokenEnv + " build.hash=" + $info.build.hash)
if ($ver -notmatch [regex]::Escape($health.build.hash)) { Write-Output "MISMATCH version vs health"; exit 1 }
if ($info.build.hash -ne $health.build.hash) { Write-Output "MISMATCH info vs health"; exit 1 }
Write-Output "OK: all three name the same build"

Write-Output "=== [4] H3: swiss status prints a build row ==="
$env:SWISS_HOME = Join-Path $env:LOCALAPPDATA "swiss-test-home"
$env:SWISS_PORT = "19998"
$status = & $Exe status 2>&1 | Out-String
$status.Split("`n") | Where-Object { $_ -match "build|running|url" } | ForEach-Object { Write-Output ("  " + $_.TrimEnd()) }
if ($status -notmatch "build") { Write-Output "NO BUILD ROW"; exit 1 }
Write-Output "OK: status carries the build"
Remove-Item Env:\SWISS_HOME -ErrorAction SilentlyContinue
Remove-Item Env:\SWISS_PORT -ErrorAction SilentlyContinue

Write-Output "=== [5] H1: covered by a_local_pwsh_keeps_its_colours_under_a_no_color_launcher (SPEC §host.session) ==="

Write-Output "=== [6] H2: saving terminal config writes the TEST home only ==="
$cfg = Invoke-Api GET /api/plugins/terminal/config
$newCfg = @{ config = $cfg.config; revision = $cfg.revision }
$body = ConvertTo-Json $newCfg -Depth 12 -Compress
$saved = Invoke-Api PUT /api/plugins/terminal/config $body
Write-Output ("save answered ok: " + ($saved.ok | Out-String).Trim())
Start-Sleep -Milliseconds 800
$prodAfter = (Get-Item $prodCfg).LastWriteTimeUtc
$testCfg = Join-Path $env:LOCALAPPDATA "swiss-test-home\gateway.config.json"
$testAfter = (Get-Item $testCfg).LastWriteTimeUtc
Write-Output ("prod mtime AFTER : " + $prodAfter.ToString("o") + "  (unchanged: " + ($prodBefore -eq $prodAfter) + ")")
Write-Output ("test mtime AFTER : " + $testAfter.ToString("o"))
if ($prodBefore -ne $prodAfter) { Write-Output "H2 FAILED: production config was written"; exit 1 }
Write-Output "OK: production config untouched"

Write-Output "=== [7] stop 19998, verify 19999 still healthy ==="
& scripts\test-instance.ps1 -Stop
$p99 = $null
try { $p99 = Invoke-WebRequest -Uri "http://127.0.0.1:19999/health" -UseBasicParsing -TimeoutSec 5 } catch { }
if ($p99 -and $p99.StatusCode -eq 200) { Write-Output "OK: 19999 /health is 200 (production untouched)" } else { Write-Output "WARN: 19999 did not answer (was it running before?)" }
Write-Output "=== ACCEPTANCE DONE ==="
exit 0

