param(
    [string]$AdminUser = "postgres",
    [string]$HostName = "localhost",
    [int]$Port = 5432
)

$ErrorActionPreference = "Stop"
$securePassword = Read-Host "PostgreSQL password for $AdminUser" -AsSecureString
$credential = [System.Management.Automation.PSCredential]::new($AdminUser, $securePassword)
$env:PGPASSWORD = $credential.GetNetworkCredential().Password
$rulenixSecurePassword = Read-Host "New password for PostgreSQL role 'rulenix'" -AsSecureString
$rulenixCredential = [System.Management.Automation.PSCredential]::new("rulenix", $rulenixSecurePassword)
$rulenixPasswordSql = $rulenixCredential.GetNetworkCredential().Password -replace "'", "''"

try {
    $roleExists = psql -h $HostName -p $Port -U $AdminUser -d postgres -tAc "SELECT 1 FROM pg_roles WHERE rolname='rulenix'"
    if ($LASTEXITCODE -ne 0) { throw "Unable to connect as PostgreSQL administrator '$AdminUser'." }

    if (-not $roleExists) {
        psql -h $HostName -p $Port -U $AdminUser -d postgres -v ON_ERROR_STOP=1 -c "CREATE ROLE rulenix LOGIN PASSWORD '$rulenixPasswordSql'"
        if ($LASTEXITCODE -ne 0) { throw "Could not create the rulenix role." }
    } else {
        psql -h $HostName -p $Port -U $AdminUser -d postgres -v ON_ERROR_STOP=1 -c "ALTER ROLE rulenix WITH LOGIN PASSWORD '$rulenixPasswordSql'"
        if ($LASTEXITCODE -ne 0) { throw "Could not update the rulenix role." }
    }

    $databaseExists = psql -h $HostName -p $Port -U $AdminUser -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname='rulenix'"
    if (-not $databaseExists) {
        psql -h $HostName -p $Port -U $AdminUser -d postgres -v ON_ERROR_STOP=1 -c "CREATE DATABASE rulenix OWNER rulenix"
        if ($LASTEXITCODE -ne 0) { throw "Could not create the rulenix database." }
    }

    Write-Host "PostgreSQL role and database 'rulenix' are ready." -ForegroundColor Green
} finally {
    Remove-Item Env:PGPASSWORD -ErrorAction SilentlyContinue
}
