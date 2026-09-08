$ErrorActionPreference = "Stop"
$pythonRoot = Join-Path $PSScriptRoot "..\python-backend"
$rustRoot = Join-Path $PSScriptRoot "..\backend"
if (-not $env:TEST_DATABASE_URL) {
    $env:TEST_DATABASE_URL = "postgresql+asyncpg://rulenix:12345678@localhost:5432/rulenix_test_clear_trades"
}
if ($env:TEST_DATABASE_URL -notmatch "/rulenix_test") {
    throw "TEST_DATABASE_URL must point to an isolated *_test database; production database names are refused."
}
Push-Location $pythonRoot
try {
    Push-Location $rustRoot
    try {
        cargo build --quiet --bin rulenix-backend --features phase10-adapter
    } finally {
        Pop-Location
    }
    $env:RULENIX_RUST_ADAPTER = Join-Path $rustRoot "target\debug\rulenix-backend.exe"
    python -m app.parity.audit
    python -m ruff check app tests
    python -m mypy app
    python -m compileall -q app tests
    python -m pytest -q
} finally {
    Pop-Location
}
