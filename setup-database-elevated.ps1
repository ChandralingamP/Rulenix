#Requires -RunAsAdministrator
$ErrorActionPreference = "Stop"

$serviceName = "postgresql-x64-18"
$dataDirectory = "C:\Program Files\PostgreSQL\18\data"
$binDirectory = "C:\Program Files\PostgreSQL\18\bin"
$hbaPath = Join-Path $dataDirectory "pg_hba.conf"
$backupPath = Join-Path $dataDirectory "pg_hba.conf.rulenix-backup"
$psql = Join-Path $binDirectory "psql.exe"
$createdb = Join-Path $binDirectory "createdb.exe"
$utf8 = [System.Text.UTF8Encoding]::new($false)
$rulenixSecurePassword = Read-Host "New password for PostgreSQL role 'rulenix'" -AsSecureString
$rulenixCredential = [System.Management.Automation.PSCredential]::new("rulenix", $rulenixSecurePassword)
$rulenixPassword = $rulenixCredential.GetNetworkCredential().Password
$rulenixPasswordSql = $rulenixPassword -replace "'", "''"

$originalBytes = [System.IO.File]::ReadAllBytes($hbaPath)
$original = $utf8.GetString($originalBytes)
$originalHash = (Get-FileHash -LiteralPath $hbaPath -Algorithm SHA256).Hash
[System.IO.File]::WriteAllBytes($backupPath, $originalBytes)

$temporary = $original
$temporary = $temporary -replace '(?m)^(host\s+all\s+all\s+127\.0\.0\.1/32\s+scram-sha-256\s*)$', "host    all             postgres        127.0.0.1/32            trust`r`n`$1"
$temporary = $temporary -replace '(?m)^(host\s+all\s+all\s+::1/128\s+scram-sha-256\s*)$', "host    all             postgres        ::1/128                 trust`r`n`$1"

if ($temporary -eq $original) {
    Remove-Item -LiteralPath $backupPath -Force
    throw "Expected PostgreSQL loopback rules were not found; no changes made."
}

try {
    [System.IO.File]::WriteAllText($hbaPath, $temporary, $utf8)
    Restart-Service -Name $serviceName -Force
    (Get-Service -Name $serviceName).WaitForStatus('Running', [TimeSpan]::FromSeconds(30))

    $roleSql = @"
DO `$`$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'rulenix') THEN
        ALTER ROLE rulenix WITH LOGIN PASSWORD '$rulenixPasswordSql';
    ELSE
        CREATE ROLE rulenix LOGIN PASSWORD '$rulenixPasswordSql';
    END IF;
END
`$`$;
"@
    & $psql -h 127.0.0.1 -U postgres -d postgres -v ON_ERROR_STOP=1 -c $roleSql | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Could not create or update role 'rulenix'." }

    $databaseExists = & $psql -h 127.0.0.1 -U postgres -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname='rulenix'"
    if ($LASTEXITCODE -ne 0) { throw "Could not inspect PostgreSQL databases." }
    if (-not $databaseExists) {
        & $createdb -h 127.0.0.1 -U postgres -O rulenix rulenix
        if ($LASTEXITCODE -ne 0) { throw "Could not create database 'rulenix'." }
    } else {
        & $psql -h 127.0.0.1 -U postgres -d postgres -v ON_ERROR_STOP=1 -c "ALTER DATABASE rulenix OWNER TO rulenix" | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Could not set the database owner." }
    }
} finally {
    [System.IO.File]::WriteAllBytes($hbaPath, $originalBytes)
    Restart-Service -Name $serviceName -Force
    (Get-Service -Name $serviceName).WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
    $restoredHash = (Get-FileHash -LiteralPath $hbaPath -Algorithm SHA256).Hash
    if ($restoredHash -ne $originalHash) {
        throw "PostgreSQL authentication configuration did not restore exactly. Backup: $backupPath"
    }
    Remove-Item -LiteralPath $backupPath -Force
}

$env:PGPASSWORD = $rulenixPassword
try {
    $verification = & $psql -h localhost -U rulenix -d rulenix -tAc "SELECT current_user || ':' || current_database()"
    if ($LASTEXITCODE -ne 0 -or $verification.Trim() -ne "rulenix:rulenix") {
        throw "The new database credentials failed verification."
    }
} finally {
    Remove-Item Env:PGPASSWORD -ErrorAction SilentlyContinue
}
