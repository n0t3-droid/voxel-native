param(
    [int] $Port = 8787,
    [ValidateSet("debug", "release")]
    [string] $Profile = "debug",
    [switch] $SkipBuild
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot "build-web.ps1") -Profile $Profile
}

$webRoot = Join-Path $root "web"
$prefix = "http://127.0.0.1:$Port/"

Write-Host "Serving $webRoot at $prefix"
Write-Host "Open $prefix in a browser. Press Ctrl+C to stop."

$listener = [System.Net.HttpListener]::new()
$listener.Prefixes.Add($prefix)
$listener.Start()

try {
    while ($listener.IsListening) {
        $context = $listener.GetContext()
        $relative = [Uri]::UnescapeDataString($context.Request.Url.AbsolutePath.TrimStart("/"))
        if ([string]::IsNullOrWhiteSpace($relative)) {
            $relative = "index.html"
        }

        $full = [System.IO.Path]::GetFullPath((Join-Path $webRoot $relative))
        if (-not $full.StartsWith([System.IO.Path]::GetFullPath($webRoot))) {
            $context.Response.StatusCode = 403
            $context.Response.Close()
            continue
        }

        if (-not (Test-Path $full -PathType Leaf)) {
            $context.Response.StatusCode = 404
            $context.Response.Close()
            continue
        }

        $ext = [System.IO.Path]::GetExtension($full).ToLowerInvariant()
        $contentType = switch ($ext) {
            ".html" { "text/html; charset=utf-8" }
            ".js" { "text/javascript; charset=utf-8" }
            ".wasm" { "application/wasm" }
            ".css" { "text/css; charset=utf-8" }
            default { "application/octet-stream" }
        }
        $bytes = [System.IO.File]::ReadAllBytes($full)
        $context.Response.ContentType = $contentType
        $context.Response.ContentLength64 = $bytes.Length
        $context.Response.OutputStream.Write($bytes, 0, $bytes.Length)
        $context.Response.Close()
    }
}
finally {
    $listener.Stop()
    $listener.Close()
}
