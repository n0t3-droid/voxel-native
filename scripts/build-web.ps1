param(
    [ValidateSet("debug", "release")]
    [string] $Profile = "debug"
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

rustup target add wasm32-unknown-unknown | Out-Host

if (-not (Get-Command wasm-bindgen -ErrorAction SilentlyContinue)) {
    Write-Host "Installing wasm-bindgen-cli 0.2.118..."
    cargo install wasm-bindgen-cli --version 0.2.118 --locked
}

$cargoArgs = @("build", "--target", "wasm32-unknown-unknown")
if ($Profile -eq "release") {
    $cargoArgs += "--release"
}

cargo @cargoArgs

$profileDir = if ($Profile -eq "release") { "release" } else { "debug" }
$wasmPath = Join-Path $root "target\wasm32-unknown-unknown\$profileDir\voxel-native.wasm"
$outDir = Join-Path $root "web\pkg"

if (Test-Path $outDir) {
    Remove-Item -Recurse -Force $outDir
}
New-Item -ItemType Directory -Force $outDir | Out-Null

wasm-bindgen --target web --out-dir $outDir $wasmPath

Write-Host "Web build ready: web\index.html"
