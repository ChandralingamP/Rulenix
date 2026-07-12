param(
    [Parameter(Mandatory=$true)][string]$DatabaseUrl,
    [Parameter(Mandatory=$true)][string]$OutputDirectory,
    [Parameter(Mandatory=$true)][string]$EncryptionPassphraseFile,
    [int]$RetentionDays = 14
)

$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null

if (-not (Get-Command pg_dump -ErrorAction SilentlyContinue)) {
    throw "pg_dump is required on PATH."
}
if (-not (Get-Command openssl -ErrorAction SilentlyContinue)) {
    throw "openssl is required on PATH for encrypted backups."
}
if (-not (Test-Path $EncryptionPassphraseFile)) {
    throw "Encryption passphrase file not found."
}

$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$plain = Join-Path $OutputDirectory "rulenix-$timestamp.dump"
$encrypted = "$plain.enc"

pg_dump --format=custom --no-owner --no-acl --dbname="$DatabaseUrl" --file="$plain"
openssl enc -aes-256-cbc -pbkdf2 -salt -in "$plain" -out "$encrypted" -pass "file:$EncryptionPassphraseFile"
Remove-Item -LiteralPath $plain

Get-ChildItem -Path $OutputDirectory -Filter "rulenix-*.dump.enc" |
    Where-Object { $_.LastWriteTimeUtc -lt (Get-Date).ToUniversalTime().AddDays(-$RetentionDays) } |
    Remove-Item -Force

Write-Host "Encrypted backup written to $encrypted"
