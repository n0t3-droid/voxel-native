param(
    [switch] $Qa,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $PassThru
)

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

$minRust = "1.77"
$rustupUrl = "https://rustup.rs"

function Test-OnPath([string] $Name) {
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

if (-not (Test-OnPath "cargo") -or -not (Test-OnPath "rustc")) {
    Write-Host @"
Rust is not on PATH (need both cargo and rustc).

Install rustup from $rustupUrl
Then open a new terminal and run:
  rustup default stable

This project needs Rust $minRust or newer (Bevy 0.14). Current stable is fine.
blake3 is pinned to 1.8.2 so you do not need Rust 1.85+ / edition2024.
"@
    exit 1
}

$cargoArgs = @("run", "--release")
$appArgs = @()

if ($Qa) {
    if (-not $env:VOXEL_NATIVE_QA) { $env:VOXEL_NATIVE_QA = "1" }
    if (-not $env:VOXEL_NATIVE_QA_SECONDS) { $env:VOXEL_NATIVE_QA_SECONDS = "45" }
    if (-not $env:VOXEL_NATIVE_QA_SCREENSHOT_INTERVAL) {
        $env:VOXEL_NATIVE_QA_SCREENSHOT_INTERVAL = "7"
    }
    $appArgs += "--qa"
}

if ($PassThru) {
    $appArgs += $PassThru
}

if ($appArgs.Count -gt 0) {
    $cargoArgs += "--"
    $cargoArgs += $appArgs
}

Write-Host "Running: cargo $($cargoArgs -join ' ')"
cargo @cargoArgs
