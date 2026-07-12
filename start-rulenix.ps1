$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$logDirectory = Join-Path $root ".runlogs"
New-Item -ItemType Directory -Force $logDirectory | Out-Null

function Test-Port([int]$Port) {
    return $null -ne (Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue)
}

if (-not (Test-Port 8080)) {
    $backend = Start-Process cargo.exe -ArgumentList @("run") `
        -WorkingDirectory (Join-Path $root "backend") -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $logDirectory "backend.out.log") `
        -RedirectStandardError (Join-Path $logDirectory "backend.err.log") -PassThru
    [System.IO.File]::WriteAllText((Join-Path $logDirectory "backend.pid"), [string]$backend.Id)
}

if (-not (Test-Port 5173)) {
    $frontend = Start-Process npm.cmd -ArgumentList @("run", "dev") `
        -WorkingDirectory (Join-Path $root "frontend") -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $logDirectory "frontend.out.log") `
        -RedirectStandardError (Join-Path $logDirectory "frontend.err.log") -PassThru
    [System.IO.File]::WriteAllText((Join-Path $logDirectory "frontend.pid"), [string]$frontend.Id)
}

$ready = $false
for ($attempt = 0; $attempt -lt 90; $attempt++) {
    try {
        $health = Invoke-RestMethod -Uri "http://127.0.0.1:5173/api/health" -TimeoutSec 2
        if ($health.status -eq "ok") { $ready = $true; break }
    } catch {
        Start-Sleep -Seconds 1
    }
}

if (-not $ready) {
    throw "Rulenix did not become healthy. Review .runlogs/backend.err.log and frontend.err.log."
}

Write-Host "Rulenix is ready at http://localhost:5173" -ForegroundColor Green
