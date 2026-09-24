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

# Live acceptance for docs/16 H1/H2/H3 on the isolated 19998 instance.
# Never touches 19999 except a read-only /health at the very end.
$ErrorActionPreference = "Continue"
$Token = "acceptance-token-for-1998"
# The token pin deliberately stays on the legacy name: the boot's one-shot rename flips the
# snapshot's tokenEnv to SWISS_TOKEN, and this script is the regression proof that the Node-era
# pin still authenticates through the pair partner after that rename.
$env:SWISS_TOKEN = $Token
$Exe = "target-test\release\swiss.exe"
$Health98 = "http://127.0.0.1:19998/health"

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
$hdr = @{ Authorization = "Bearer " + $Token }
$info = Invoke-RestMethod -Uri "http://127.0.0.1:19998/api/info" -Headers $hdr -TimeoutSec 5
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

Write-Output "=== [5] H1: a local shell on the daemon reports a scrubbed env ==="
$open = Invoke-RestMethod -Uri "http://127.0.0.1:19998/api/terminal/sessions" -Method Post -Headers $hdr -Body ('{"target":"local","cols":120,"rows":30}') -ContentType "application/json" -TimeoutSec 15
Write-Output ("opened session " + $open.id)
$wsUrl = "ws://127.0.0.1:19998/api/terminal/sessions/" + $open.id + "/stream?ticket=" + [uri]::EscapeDataString($open.ticket)
$ws = New-Object System.Net.WebSockets.ClientWebSocket
$ct = New-Object System.Threading.CancellationTokenSource
$ws.ConnectAsync($wsUrl, $ct.Token).GetAwaiter().GetResult()
Start-Sleep -Milliseconds 1200
$probe = "[void]'' ; Write-Output (''probe nocolor=['' + $env:NO_COLOR + ''] claude=['' + $env:CLAUDECODE + ''] render=['' + $PSStyle.OutputRendering + '']''  + '')''".Replace("''", [string][char]39)
# build the probe command char-safe: single-quoted pieces with doubled quotes
$probe = 'Write-Output ("probe nocolor=[" + $env:NO_COLOR + "] claude=[" + $env:CLAUDECODE + "] render=[" + $PSStyle.OutputRendering + "]")'
$bytes = [System.Text.Encoding]::UTF8.GetBytes($probe + "`r")
$seg = New-Object ArraySegment[byte] (,$bytes)
[void]$ws.SendAsync($seg, [System.Net.WebSockets.WebSocketMessageType]::Binary, $true, $ct.Token).GetAwaiter().GetResult()
$out = New-Object System.Text.StringBuilder
$buf = New-Object byte[] 65536
$deadline = [DateTime]::UtcNow.AddSeconds(8)
while ([DateTime]::UtcNow -lt $deadline) {
  if ($ws.State -ne [System.Net.WebSockets.WebSocketState]::Open) { break }
  $seg2 = New-Object ArraySegment[byte] (,$buf)
  $task = $ws.ReceiveAsync($seg2, $ct.Token)
  $gotFrame = $false
  try { $gotFrame = $task.Wait(1500) } catch { }
  if ($gotFrame -and $task.IsCompleted) {
    $r = $task.Result
    if ($r.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) { break }
    [void]$out.Append([System.Text.Encoding]::UTF8.GetString($buf, 0, $r.Count))
    if ($out.ToString().Contains("render=")) { Start-Sleep -Milliseconds 300; continue }
  }
}
$text = $out.ToString()
$textSplit = $text -split "`r?`n" | Where-Object { $_ -match "probe|render" }
$textSplit | ForEach-Object { Write-Output ("  out: " + $_) }
$probeLine = ($textSplit | Where-Object { $_ -match "nocolor=" } | Select-Object -Last 1)
if (-not $probeLine) { Write-Output ("NO PROBE OUTPUT; raw tail: " + $text.Substring([Math]::Max(0, $text.Length - 400))); exit 1 }
if ($probeLine -notmatch "nocolor=\[\] claude=\[\] render=\[Host\]") { Write-Output ("PROBE FAILED: " + $probeLine); exit 1 }
Write-Output ("OK: " + $probeLine.Trim())
try { $ws.CloseAsync([System.Net.WebSockets.WebSocketCloseStatus]::NormalClosure, "done", $ct.Token).GetAwaiter().GetResult() } catch { }
Invoke-RestMethod -Uri ("http://127.0.0.1:19998/api/terminal/sessions/" + $open.id) -Method Delete -Headers $hdr -TimeoutSec 5 | Out-Null

Write-Output "=== [6] H2: saving terminal config writes the TEST home only ==="
$cfg = Invoke-RestMethod -Uri "http://127.0.0.1:19998/api/plugins/terminal/config" -Headers $hdr -TimeoutSec 5
$newCfg = @{ config = $cfg.config; revision = $cfg.revision }
$body = ConvertTo-Json $newCfg -Depth 12
$saved = Invoke-RestMethod -Uri "http://127.0.0.1:19998/api/plugins/terminal/config" -Method Put -Headers $hdr -ContentType "application/json" -Body $body -TimeoutSec 15
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

