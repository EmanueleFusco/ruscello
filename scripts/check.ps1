# Run all local checks (same gates as CI).
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

function Invoke-Step([string]$cmd) {
    Write-Host "==> $cmd" -ForegroundColor Cyan
    Invoke-Expression $cmd
    if ($LASTEXITCODE -ne 0) { throw "Failed: $cmd" }
}

Invoke-Step 'cargo fmt --all --check'
Invoke-Step 'cargo clippy --workspace --all-targets -- -D warnings'
if (Get-Command cargo-nextest -ErrorAction SilentlyContinue) {
    Invoke-Step 'cargo nextest run --workspace --no-tests=pass'
    Invoke-Step 'cargo test --workspace --doc'
} else {
    Invoke-Step 'cargo test --workspace'
}
if (Get-Command cargo-deny -ErrorAction SilentlyContinue) {
    Invoke-Step 'cargo deny check'
} else {
    Write-Host 'cargo-deny not installed (cargo install cargo-deny) - skipping' -ForegroundColor Yellow
}
