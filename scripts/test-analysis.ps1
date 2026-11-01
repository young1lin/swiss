# Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0.
# Real database acceptance on disposable containers, never the user's database.
[CmdletBinding()]
param([string]$Distro = 'Ubuntu', [switch]$Full)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$prefix = 'swiss-analysis-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
$fixturePassword = [guid]::NewGuid().ToString('N')
$created = [System.Collections.Generic.List[string]]::new()
$bridgeProcess = $null
$keys = @('SWISS_ANALYSIS_MYSQL_PORT','SWISS_ANALYSIS_MYSQL_PASSWORD','SWISS_ANALYSIS_PG_URL','SWISS_IT_MYSQL_URL','SWISS_IT_POSTGRES_URL','SWISS_IT_REDIS_URL','SWISS_IT_MONGO_URL','DOCKER_HOST')
$previousEnv = @{}
foreach ($key in $keys) { $previousEnv[$key] = [Environment]::GetEnvironmentVariable($key, 'Process') }
function Docker {
    $output = & wsl -d $Distro -- docker @args
    if ($LASTEXITCODE -ne 0) { throw 'fixture Docker command failed' }
    return $output
}
function Start-Fixture([string]$suffix, [string]$inside, [string[]]$options) {
    $name = "$prefix-$suffix"
    $null = Docker run -d --name $name --cpus 1 --memory 512m -p "127.0.0.1::$inside" @options
    $created.Add($name)
    $info = (Docker inspect $name | Out-String | ConvertFrom-Json)[0]
    return [int]$info.NetworkSettings.Ports.$inside[0].HostPort
}
try {
    $myPort = Start-Fixture 'mysql' '3306/tcp' @('-e', "MYSQL_ROOT_PASSWORD=$fixturePassword", '-e', 'MYSQL_ROOT_HOST=%', '-e', 'MYSQL_DATABASE=analysis_fixture', 'mysql:8.4', '--innodb-buffer-pool-size=32M')
    $pgPort = Start-Fixture 'postgres' '5432/tcp' @('-e', "POSTGRES_PASSWORD=$fixturePassword", '-e', 'POSTGRES_DB=analysis_fixture', 'postgres:17', '-c', 'shared_preload_libraries=pg_stat_statements')
    $env:SWISS_ANALYSIS_MYSQL_PORT = "$myPort"
    $env:SWISS_ANALYSIS_MYSQL_PASSWORD = $fixturePassword
    $env:SWISS_ANALYSIS_PG_URL = "postgres://postgres:${fixturePassword}@127.0.0.1:$pgPort/analysis_fixture"
    $env:SWISS_IT_MYSQL_URL = "mysql://root:${fixturePassword}@127.0.0.1:$myPort"
    $env:SWISS_IT_POSTGRES_URL = $env:SWISS_ANALYSIS_PG_URL
    if ($Full) {
        # The Windows harness needs the WSL Docker API for its actual cleanup acceptance.
        # Expose it only on loopback, for the lifetime of this test run.
        $reservation = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
        $reservation.Start()
        $bridgePort = $reservation.LocalEndpoint.Port
        $reservation.Stop()
        $bridgeStart = [Diagnostics.ProcessStartInfo]::new('wsl.exe')
        $bridgeStart.UseShellExecute = $false
        $bridgeStart.CreateNoWindow = $true
        $bridgeStart.RedirectStandardError = $true
        foreach ($arg in @('-d', $Distro, '--exec', 'socat', "TCP4-LISTEN:${bridgePort},bind=127.0.0.1,reuseaddr,fork", 'UNIX-CONNECT:/var/run/docker.sock')) { $bridgeStart.ArgumentList.Add($arg) }
        $bridgeProcess = [Diagnostics.Process]::Start($bridgeStart)
        $bridgeReady = $false
        for ($attempt = 0; $attempt -lt 30; $attempt++) {
            try { $null = Invoke-RestMethod "http://127.0.0.1:$bridgePort/_ping" -TimeoutSec 1; $bridgeReady = $true; break } catch { Start-Sleep -Milliseconds 100 }
        }
        if (-not $bridgeReady) { throw 'fixture Docker API bridge did not start (socat is required)' }
        $env:DOCKER_HOST = "tcp://127.0.0.1:$bridgePort"
        $redisPort = Start-Fixture 'redis' '6379/tcp' @('redis:7')
        $mongoPort = Start-Fixture 'mongo' '27017/tcp' @('mongo:8', '--replSet', 'rs0', '--bind_ip_all')
        $env:SWISS_IT_REDIS_URL = "redis://127.0.0.1:$redisPort"
        $env:SWISS_IT_MONGO_URL = "mongodb://127.0.0.1:$mongoPort/?directConnection=true&replicaSet=rs0"
        $mongoReady = $false
        for ($attempt = 0; $attempt -lt 30; $attempt++) {
            & wsl -d $Distro -- docker exec "$prefix-mongo" mongosh --quiet --eval "try { rs.initiate({_id:'rs0',members:[{_id:0,host:'127.0.0.1:27017'}]}) } catch(e) {}" 2>$null | Out-Null
            if ($LASTEXITCODE -eq 0) { $mongoReady = $true; break }
            Start-Sleep -Seconds 1
        }
        if (-not $mongoReady) { throw 'fixture MongoDB did not start' }
    }
    # Wait on engine readiness, not a fixed boot delay. Credentials never appear in output.
    $ready = $false
    for ($attempt = 0; $attempt -lt 60; $attempt++) {
        & wsl -d $Distro -- docker exec -e "MYSQL_PWD=$fixturePassword" "$prefix-mysql" mysqladmin --protocol=TCP --host=127.0.0.1 ping -uroot --silent 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) { $ready = $true; break }
        Start-Sleep -Seconds 1
    }
    if (-not $ready) { throw 'fixture MySQL did not start' }
    Push-Location $Root
    try {
        $env:CARGO_BUILD_JOBS = '4'
        cargo test -p swiss-plugins --test analysis_engines --locked -j 4 -- --ignored
        if ($LASTEXITCODE -ne 0) { throw 'native analysis acceptance failed' }
        if ($Full) {
            cargo test -p swiss-it --features it --locked -j 4
            if ($LASTEXITCODE -ne 0) { throw 'database integration gate failed' }
        }
    } finally { Pop-Location }
} finally {
    foreach ($name in $created) { & wsl -d $Distro -- docker rm -f $name 2>$null | Out-Null }
    if ($bridgeProcess) {
        & wsl -d $Distro --exec pkill -f -- "^socat TCP4-LISTEN:${bridgePort}," 2>$null | Out-Null
        if (-not $bridgeProcess.HasExited) { $bridgeProcess.Kill() }
        $bridgeProcess.Dispose()
    }
    foreach ($key in $keys) { [Environment]::SetEnvironmentVariable($key, $previousEnv[$key], 'Process') }
}
