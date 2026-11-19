<#
.SYNOPSIS
    Benchmark memory usage of ai-server under concurrent streaming load.
.DESCRIPTION
    Builds ai-server, starts it on a test port, samples working set memory (RSS)
    at idle, launches concurrent SSE streaming requests to /api/stream-demo
    using a single-process async HttpClient (no child processes), and samples
    peak working set memory throughout the load test.
#>

param(
    [int]$Concurrency = 10,
    [int]$Port = 3001,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Net.Http

if (-not $SkipBuild) {
    Write-Host "=== 1. Building ai-server binary ===" -ForegroundColor Cyan
    $env:SQLX_OFFLINE = "true"
    cargo build -p ai-server
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to build ai-server"
    }
}

$binPath = ".\target\debug\ai-server.exe"
if (-not (Test-Path $binPath)) {
    throw "Binary not found at $binPath"
}

Write-Host "`n=== 2. Starting ai-server on port $Port ===" -ForegroundColor Cyan
$env:PORT = $Port.ToString()
$env:APP_ENV = "test"
$env:RUST_LOG = "warn"

$proc = Start-Process -FilePath $binPath -PassThru -WindowStyle Hidden
$client = New-Object System.Net.Http.HttpClient
$client.Timeout = [System.TimeSpan]::FromSeconds(30)

try {
    $healthUrl = "http://127.0.0.1:$Port/api/health"
    $ready = $false
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Milliseconds 200
        try {
            $resp = Invoke-RestMethod -Uri $healthUrl -TimeoutSec 2 -ErrorAction Stop
            if ($resp.status -eq 'ok') {
                $ready = $true
                break
            }
        } catch {
            # continue waiting
        }
    }

    if (-not $ready) {
        throw "ai-server failed to respond on $healthUrl within 6 seconds"
    }

    Start-Sleep -Milliseconds 500
    $proc.Refresh()
    $idleMemMB = [math]::Round($proc.WorkingSet64 / 1MB, 2)
    Write-Host "Server ready. Baseline Idle Working Set: $idleMemMB MB (PID: $($proc.Id))" -ForegroundColor Green

    Write-Host "`n=== 3. Launching $Concurrency concurrent streaming requests (single-process async) ===" -ForegroundColor Cyan
    $streamUrl = "http://127.0.0.1:$Port/api/stream-demo"
    $tasks = @()
    $startTime = Get-Date
    for ($i = 0; $i -lt $Concurrency; $i++) {
        $tasks += $client.GetStringAsync($streamUrl)
    }

    $peakMemMB = $idleMemMB
    while (($tasks | Where-Object { -not $_.IsCompleted }).Count -gt 0) {
        Start-Sleep -Milliseconds 250
        $proc.Refresh()
        $currentMemMB = [math]::Round($proc.WorkingSet64 / 1MB, 2)
        if ($currentMemMB -gt $peakMemMB) {
            $peakMemMB = $currentMemMB
        }
    }

    $elapsed = (Get-Date) - $startTime
    Write-Host "`n=== 4. Load Test Completed in $([math]::Round($elapsed.TotalSeconds, 2))s ===" -ForegroundColor Cyan

    $successful = 0
    foreach ($task in $tasks) {
        $body = $task.GetAwaiter().GetResult()
        $eventCount = ([regex]::Matches($body, '(?m)^data:')).Count
        if ($eventCount -eq 25) {
            $successful++
        }
    }
    Write-Host "Stream completions: $successful / $Concurrency requests received all 25 SSE chunks." -ForegroundColor Green

    Start-Sleep -Milliseconds 500
    $proc.Refresh()
    $finalMemMB = [math]::Round($proc.WorkingSet64 / 1MB, 2)

    $passStatus = if ($peakMemMB -le 35.0) { 'PASS [OK]' } else { 'CHECK' }
    $statusColor = if ($peakMemMB -le 35.0) { 'Green' } else { 'Yellow' }

    Write-Host "`n================ Memory Benchmark Summary ================" -ForegroundColor Magenta
    Write-Host " Baseline Working Set (Idle):   $idleMemMB MB" -ForegroundColor White
    Write-Host " Peak Working Set ($Concurrency Streams): $peakMemMB MB" -ForegroundColor White
    Write-Host " Final Working Set (Post-load): $finalMemMB MB" -ForegroundColor White
    Write-Host " Memory Delta under Load:       +$([math]::Round($peakMemMB - $idleMemMB, 2)) MB" -ForegroundColor White
    Write-Host " Target Range (< 35MB):         $passStatus" -ForegroundColor $statusColor
    Write-Host "==========================================================" -ForegroundColor Magenta
}
finally {
    $client.Dispose()
    if ($proc -and -not $proc.HasExited) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    }
}
