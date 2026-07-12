param(
    [int]$BackendPort = 8080,
    [int]$FrontendPort = 5173
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$logDirectory = Join-Path $root ".runlogs"
New-Item -ItemType Directory -Force $logDirectory | Out-Null

$backendOut = Join-Path $logDirectory "backend.out.log"
$backendErr = Join-Path $logDirectory "backend.err.log"
$frontendOut = Join-Path $logDirectory "frontend.out.log"
$frontendErr = Join-Path $logDirectory "frontend.err.log"
$tunnelOut = Join-Path $logDirectory "cloudflared.out.log"
$tunnelErr = Join-Path $logDirectory "cloudflared.err.log"
$tunnelUrlFile = Join-Path $logDirectory "cloudflare-url.txt"

function Assert-Command([string]$Name, [string]$InstallHint) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "$Name was not found on PATH. $InstallHint"
    }
}

function Test-Port([int]$Port) {
    return $null -ne (Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue)
}

function Test-PidFile([string]$Path) {
    if (-not (Test-Path $Path)) { return $false }
    $pidValue = Get-Content $Path -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $pidValue) { return $false }
    return $null -ne (Get-Process -Id ([int]$pidValue) -ErrorAction SilentlyContinue)
}

function Wait-ForHealth([string]$Url, [string]$Name) {
    for ($attempt = 0; $attempt -lt 90; $attempt++) {
        try {
            $health = Invoke-RestMethod -Uri $Url -TimeoutSec 2
            if ($health.status -eq "ok") { return }
        } catch {
            Start-Sleep -Seconds 1
        }
    }
    throw "$Name did not become healthy. Review logs in $logDirectory."
}

function Find-TunnelUrl {
    $content = ""
    foreach ($path in @($tunnelOut, $tunnelErr)) {
        if (Test-Path $path) {
            $content += "`n" + (Get-Content $path -Raw -ErrorAction SilentlyContinue)
        }
    }
    $match = [regex]::Match($content, "https://[a-zA-Z0-9-]+\.trycloudflare\.com")
    if ($match.Success) { return $match.Value }
    return $null
}

Assert-Command "cargo.exe" "Install Rust from https://rustup.rs/."
Assert-Command "npm.cmd" "Install Node.js from https://nodejs.org/."
Assert-Command "cloudflared.exe" "Install Cloudflare Tunnel: winget install --id Cloudflare.cloudflared"

$frontendDirectory = Join-Path $root "frontend"
if (-not (Test-Path (Join-Path $frontendDirectory "node_modules"))) {
    Write-Host "Installing frontend packages..." -ForegroundColor Cyan
    Push-Location $frontendDirectory
    try {
        npm install
    } finally {
        Pop-Location
    }
}

if (-not (Test-Port $BackendPort)) {
    Write-Host "Starting Rulenix backend on port $BackendPort..." -ForegroundColor Cyan
    $backend = Start-Process cargo.exe -ArgumentList @("run") `
        -WorkingDirectory (Join-Path $root "backend") -WindowStyle Hidden `
        -RedirectStandardOutput $backendOut `
        -RedirectStandardError $backendErr -PassThru
    [System.IO.File]::WriteAllText((Join-Path $logDirectory "backend.pid"), [string]$backend.Id)
} else {
    Write-Host "Backend port $BackendPort is already listening." -ForegroundColor DarkYellow
}

if (-not (Test-Port $FrontendPort)) {
    Write-Host "Starting Rulenix frontend on port $FrontendPort..." -ForegroundColor Cyan
    $frontend = Start-Process npm.cmd -ArgumentList @("run", "dev") `
        -WorkingDirectory $frontendDirectory -WindowStyle Hidden `
        -RedirectStandardOutput $frontendOut `
        -RedirectStandardError $frontendErr -PassThru
    [System.IO.File]::WriteAllText((Join-Path $logDirectory "frontend.pid"), [string]$frontend.Id)
} else {
    Write-Host "Frontend port $FrontendPort is already listening." -ForegroundColor DarkYellow
}

Wait-ForHealth "http://127.0.0.1:$FrontendPort/api/health" "Rulenix"

$cloudflaredPid = Join-Path $logDirectory "cloudflared.pid"
if (-not (Test-PidFile $cloudflaredPid)) {
    Remove-Item $tunnelOut, $tunnelErr, $tunnelUrlFile -ErrorAction SilentlyContinue
    Write-Host "Starting Cloudflare quick tunnel..." -ForegroundColor Cyan
    $cloudflared = Start-Process cloudflared.exe `
        -ArgumentList @("tunnel", "--url", "http://localhost:$FrontendPort", "--no-autoupdate") `
        -WorkingDirectory $root -WindowStyle Hidden `
        -RedirectStandardOutput $tunnelOut `
        -RedirectStandardError $tunnelErr -PassThru
    [System.IO.File]::WriteAllText($cloudflaredPid, [string]$cloudflared.Id)
} else {
    Write-Host "Cloudflare tunnel process is already running." -ForegroundColor DarkYellow
}

$tunnelUrl = $null
for ($attempt = 0; $attempt -lt 90; $attempt++) {
    $tunnelUrl = Find-TunnelUrl
    if ($tunnelUrl) { break }
    Start-Sleep -Seconds 1
}

if (-not $tunnelUrl) {
    throw "Cloudflare tunnel did not publish a URL. Review $tunnelErr and $tunnelOut."
}

[System.IO.File]::WriteAllText($tunnelUrlFile, $tunnelUrl)
Write-Host ""
Write-Host "Rulenix local URL: http://localhost:$FrontendPort" -ForegroundColor Green
Write-Host "Share this Cloudflare URL with your friend:" -ForegroundColor Green
Write-Host $tunnelUrl -ForegroundColor Yellow
Write-Host ""
Write-Host "Logs and tunnel URL are in $logDirectory. Stop everything with .\stop-rulenix.ps1" -ForegroundColor Cyan
