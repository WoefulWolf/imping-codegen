# Runs fmt, clippy, tests and docs.
# Usage, from the repo root: powershell -ExecutionPolicy Bypass -File scripts/check.ps1
#
# No per-target loop: the generator only writes source text.

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

function Invoke-Step([string]$name, [scriptblock]$command) {
    Write-Host "==> $name" -ForegroundColor Cyan
    & $command
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $name" -ForegroundColor Red
        exit $LASTEXITCODE
    }
}

Invoke-Step 'cargo fmt --check' { cargo fmt --all --check }
Invoke-Step 'clippy' { cargo clippy --workspace --all-targets --all-features -- -D warnings }
Invoke-Step 'test' { cargo test --workspace --all-features }

$env:RUSTDOCFLAGS = '-D warnings'
Invoke-Step 'cargo doc' { cargo doc --workspace --no-deps --all-features }

Write-Host 'All checks passed.' -ForegroundColor Green
