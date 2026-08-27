param(
    [switch]$SkipWorkspaceTests,
    [switch]$SkipWasm
)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))

function Invoke-CheckedCommand {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Label,
        [Parameter(Mandatory = $true)]
        [string]$Executable,
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    Write-Host "`n[$Label] $Executable $($Arguments -join ' ')"
    & $Executable @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Label failed with exit code $LASTEXITCODE"
    }
}

Push-Location $projectRoot
try {
    $protectedPrefixes = @(
        'saves/', 'qa_runs/', 'agent_runs/', 'output/',
        '.codex/', '.codex_tmp/', '.playwright-mcp/',
        'voxel-native-save.ron', 'sketchup-home.png', 'sketchup-share-model.png'
    )
    $stagedPaths = @(& git diff --cached --name-only --diff-filter=ACDMRTUXB)
    if ($LASTEXITCODE -ne 0) {
        throw 'Unable to inspect the staged path set.'
    }
    $protectedStaged = @(
        $stagedPaths | Where-Object {
            $candidate = $_.Replace('\', '/')
            $protectedPrefixes | Where-Object { $candidate.StartsWith($_) }
        }
    )
    if ($protectedStaged.Count -gt 0) {
        throw "Protected runtime evidence is staged:`n$($protectedStaged -join "`n")"
    }

    $requiredViewportTokens = @(
        '320x480', '800x600', '960x540', '1280x720',
        '1920x1080', '2560x1440', '3440x1440'
    )
    $eliteContract = Get-Content -LiteralPath 'docs\ELITE_WORLD_SYSTEMS_STANDARD.md' -Raw
    $responsiveContract = Get-Content -LiteralPath 'docs\RESPONSIVE_VISUAL_QA.md' -Raw
    foreach ($token in $requiredViewportTokens) {
        if (-not $eliteContract.Contains($token)) {
            throw "Elite viewport contract is missing $token."
        }
        if (-not $responsiveContract.Contains($token.Replace('x', ' x '))) {
            throw "Responsive visual QA contract is missing $token."
        }
    }
    foreach ($scale in @('100%', '150%', '200%')) {
        if (-not $eliteContract.Contains($scale)) {
            throw "Elite DPI contract is missing $scale."
        }
        if (-not $responsiveContract.Contains($scale)) {
            throw "Responsive visual QA contract is missing $scale."
        }
    }

    Invoke-CheckedCommand -Label 'Rust formatting' -Executable 'cargo' -Arguments @(
        'fmt', '--all', '--', '--check'
    )
    Invoke-CheckedCommand -Label 'Native binary check' -Executable 'cargo' -Arguments @(
        'check', '--bin', 'voxel-native'
    )

    if (-not $SkipWasm) {
        $installedTargets = @(& rustup target list --installed)
        if ($LASTEXITCODE -ne 0) {
            throw 'Unable to inspect installed Rust targets.'
        }
        if ($installedTargets -notcontains 'wasm32-unknown-unknown') {
            throw 'wasm32-unknown-unknown is required for the elite release gate.'
        }
        Invoke-CheckedCommand -Label 'WebAssembly binary check' -Executable 'cargo' -Arguments @(
            'check', '--target', 'wasm32-unknown-unknown', '--bin', 'voxel-native'
        )
    }

    if (-not $SkipWorkspaceTests) {
        Invoke-CheckedCommand -Label 'Workspace tests' -Executable 'cargo' -Arguments @(
            'test', '--workspace', '--quiet'
        )
    }

    Write-Host "`nElite non-visual gates passed. No GUI was launched and no save or QA artifact was changed."
}
finally {
    Pop-Location
}
