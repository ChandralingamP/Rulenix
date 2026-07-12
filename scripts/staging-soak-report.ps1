param(
    [Parameter(Mandatory=$true)][string]$ApiBaseUrl,
    [Parameter(Mandatory=$true)][string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$stamp = Get-Date -Format "yyyy-MM-dd"
$report = Join-Path $OutputDirectory "soak-report-$stamp.json"

$health = Invoke-RestMethod -Uri "$ApiBaseUrl/api/health/ready" -TimeoutSec 10
$metrics = Invoke-RestMethod -Uri "$ApiBaseUrl/api/metrics" -TimeoutSec 10

[ordered]@{
    generated_at = (Get-Date).ToUniversalTime().ToString("o")
    health = $health
    metrics = $metrics
    expected = [ordered]@{
        real_orders = 0
        trading_mode = "demo-forced"
        alerts_reviewed = $true
    }
} | ConvertTo-Json -Depth 10 | Set-Content -Encoding UTF8 $report

Write-Host "Staging soak report written to $report"
