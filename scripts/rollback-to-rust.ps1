param(
    [switch]$Production = $false
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$logDirectory = Join-Path $root ".runlogs"
New-Item -ItemType Directory -Force $logDirectory | Out-Null

Write-Host "Initiating rollback to Rulenix Rust backend..."

# 1. Terminate any Python or lingering process recorded in backend.pid
$pidFile = Join-Path $logDirectory "backend.pid"
if (Test-Path $pidFile) {
    $recordedPid = (Get-Content $pidFile -Raw).Trim()
    if ($recordedPid -match '^\d+$') {
        $proc = Get-Process -Id ([int]$recordedPid) -ErrorAction SilentlyContinue
        if ($proc) {
            Write-Host "Stopping backend process (PID $recordedPid: $($proc.ProcessName))..."
            Stop-Process -Id ([int]$recordedPid) -Force -ErrorAction SilentlyContinue
            Start-Sleep -Seconds 2
        }
    }
    Remove-Item -Path $pidFile -Force -ErrorAction SilentlyContinue
}

# 2. Verify port 8080 is clear
$listeners = Get-NetTCPConnection -LocalPort 8080 -State Listen -ErrorAction SilentlyContinue
if ($listeners) {
    foreach ($listener in $listeners) {
        $proc = Get-Process -Id $listener.OwningProcess -ErrorAction SilentlyContinue
        if ($proc -and $proc.ProcessName -ne "rulenix-backend") {
            Write-Host "Stopping conflicting process on port 8080 (PID $($proc.Id): $($proc.ProcessName))..."
            Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        }
    }
    Start-Sleep -Seconds 1
}

# 3. Start Rust Backend
if ($Production -and $IsLinux) {
    Write-Host "Restarting systemd service: rulenix-backend..."
    systemctl restart rulenix-backend
} else {
    Write-Host "Starting Rust backend locally..."
    $backend = Start-Process cargo.exe -ArgumentList @("run", "--bin", "rulenix-backend") `
        -WorkingDirectory (Join-Path $root "backend") -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $logDirectory "backend.out.log") `
        -RedirectStandardError (Join-Path $logDirectory "backend.err.log") -PassThru

    [System.IO.File]::WriteAllText((Join-Path $logDirectory "backend.pid"), [string]$backend.Id)
    Write-Host "Rust backend started with PID $($backend.Id)."
}

# 4. Verify Health / Readiness
Write-Host "Verifying Rust backend readiness on http://127.0.0.1:8080/api/health/ready..."
$maxAttempts = 30
$ready = $false
for ($attempt = 1; $attempt -le $maxAttempts; $attempt++) {
    Start-Sleep -Seconds 2
    try {
        $response = Invoke-RestMethod -Uri "http://127.0.0.1:8080/api/health/ready" -Method Get -TimeoutSec 3 -ErrorAction SilentlyContinue
        if ($response -and $response.status -eq "ready") {
            Write-Host "Rust backend is READY: $($response | ConvertTo-Json -Compress)"
            $ready = $true
            break
        }
    } catch {
        Write-Host "Waiting for backend readiness... ($attempt/$maxAttempts)"
    }
}

if (-not $ready) {
    Write-Error "Rollback to Rust backend verification timed out. Check .runlogs/backend.err.log."
} else {
    Write-Host "Rollback to Rulenix Rust backend completed successfully."
}
