param(
    [Parameter(Mandatory=$true)][string]$BackupFile,
    [Parameter(Mandatory=$true)][string]$MaintenanceDatabaseUrl,
    [Parameter(Mandatory=$true)][string]$VerificationDatabase,
    [Parameter(Mandatory=$true)][string]$EncryptionPassphraseFile
)

$ErrorActionPreference = "Stop"
foreach ($tool in @("psql", "pg_restore", "openssl")) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "$tool is required on PATH."
    }
}

$work = Join-Path ([System.IO.Path]::GetTempPath()) ("rulenix-restore-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force $work | Out-Null
$plain = Join-Path $work "backup.dump"

try {
    openssl enc -d -aes-256-cbc -pbkdf2 -in "$BackupFile" -out "$plain" -pass "file:$EncryptionPassphraseFile"
    psql "$MaintenanceDatabaseUrl" -v ON_ERROR_STOP=1 -c "DROP DATABASE IF EXISTS `"$VerificationDatabase`" WITH (FORCE);"
    psql "$MaintenanceDatabaseUrl" -v ON_ERROR_STOP=1 -c "CREATE DATABASE `"$VerificationDatabase`";"

    $builder = [System.UriBuilder]$MaintenanceDatabaseUrl
    $builder.Path = "/$VerificationDatabase"
    $verificationUrl = $builder.Uri.AbsoluteUri

    pg_restore --clean --if-exists --no-owner --no-acl --dbname="$verificationUrl" "$plain"
    psql "$verificationUrl" -v ON_ERROR_STOP=1 -c "SELECT COUNT(*) AS users FROM users;"
    psql "$MaintenanceDatabaseUrl" -v ON_ERROR_STOP=1 -c "DROP DATABASE IF EXISTS `"$VerificationDatabase`" WITH (FORCE);"
    Write-Host "Restore verification succeeded."
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
