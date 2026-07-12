$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$logDirectory = Join-Path $root ".runlogs"

foreach ($pidFileName in @("cloudflared.pid", "frontend.pid", "backend.pid")) {
    $pidFile = Join-Path $logDirectory $pidFileName
    if (Test-Path $pidFile) {
        $pidValue = Get-Content $pidFile -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($pidValue) {
            Stop-Process -Id ([int]$pidValue) -Force -ErrorAction SilentlyContinue
        }
        Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
    }
}

foreach ($port in @(5173, 8080)) {
    $listeners = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue
    foreach ($listener in $listeners) {
        $process = Get-CimInstance Win32_Process -Filter "ProcessId=$($listener.OwningProcess)"
        if ($process.CommandLine -notlike "*$root*") {
            throw "Refusing to stop unexpected process $($listener.OwningProcess) on port $port."
        }
        Stop-Process -Id $listener.OwningProcess -Force
    }
}

Write-Host "Rulenix services stopped." -ForegroundColor Yellow
