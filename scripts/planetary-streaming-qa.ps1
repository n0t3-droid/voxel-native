param(
    [ValidateSet('natural', 'astral', 'both')]
    [string]$Profile = 'both',

    [uint32]$Seed = 12345,

    [ValidateSet('off', 'lean', 'balanced', 'lush')]
    [string]$Scenery = 'lush',

    [ValidateRange(1.0, 100.0)]
    [double]$DistanceKm = 8.0,

    [ValidateRange(8.0, 600.0)]
    [double]$Seconds = 45.0,

    [ValidateRange(1.0, 120.0)]
    [double]$ScreenshotInterval = 7.0,

    [ValidateRange(320, 8192)]
    [int]$Width = 1280,

    [ValidateRange(240, 8192)]
    [int]$Height = 720,

    [ValidateRange(0.0, 24.0)]
    [double]$Hour = 15.65,

    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Release',

    [ValidateSet('bridge-v2', 'bridge-v1', 'legacy', 'lod-provenance-v1')]
    [string]$SurfaceMaterial = 'bridge-v2',

    [ValidateSet('v1', 'off')]
    [string]$Hydro = 'v1',

    [ValidateSet('scenic', 'waypoint', 'streaming', 'river', 'lava', 'near-far')]
    [string]$Focus = 'streaming',

    [ValidateSet('off', 'v1')]
    [string]$Cohorts = 'off',

    [ValidateSet('v1', 'v2', 'v3')]
    [string]$TerrainGrammar = 'v3',

    [ValidateSet('point-16-v1', 'cardinal-trimmed-8-v1')]
    [string]$L0HeightMode = 'point-16-v1',

    [switch]$Build,

    # Computes and validates the exact launch environment, but never builds,
    # starts the executable, or inspects QA output folders.
    [switch]$StaticDryRun
)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$targetFolder = if ($Configuration -eq 'Release') { 'release' } else { 'debug' }
$executable = Join-Path $projectRoot "target\$targetFolder\voxel-native.exe"

if ($StaticDryRun -and $Build) {
    throw '-StaticDryRun and -Build are mutually exclusive: a dry run must never invoke Cargo.'
}

function Assert-QaRouteCompatibility {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RouteProfile,

        [Parameter(Mandatory = $true)]
        [string]$RouteFocus
    )

    $compatible = switch ($RouteFocus) {
        'scenic' { $true }
        'streaming' { $true }
        'near-far' { $true }
        'river' { $RouteProfile -eq 'natural' }
        'waypoint' { $RouteProfile -eq 'astral' }
        'lava' { $RouteProfile -eq 'astral' }
        default { $false }
    }
    if (-not $compatible) {
        throw "QA focus '$RouteFocus' is incompatible with profile '$RouteProfile'. Natural supports scenic, streaming, river, and near-far; Astral supports scenic, waypoint, streaming, lava, and near-far."
    }
}

function Assert-QaSurfaceEvidenceCompatibility {
    param(
        [Parameter(Mandatory = $true)]
        [string]$SurfaceMode,

        [Parameter(Mandatory = $true)]
        [string]$HydroMode,

        [Parameter(Mandatory = $true)]
        [string]$CohortMode,

        [Parameter(Mandatory = $true)]
        [int]$ViewportWidth,

        [Parameter(Mandatory = $true)]
        [int]$ViewportHeight
    )

    if ($SurfaceMode -ne 'lod-provenance-v1') {
        return
    }
    if ($HydroMode -ne 'off' -or $CohortMode -ne 'off') {
        throw 'LOD-provenance evidence isolates terrain LOD colors: -Hydro off and -Cohorts off are both required.'
    }
    if ($ViewportWidth -ne 1920 -or $ViewportHeight -ne 1080) {
        throw 'LOD-provenance evidence has a fixed analyzer contract: -Width 1920 and -Height 1080 are required.'
    }
}

$profiles = if ($Profile -eq 'both') { @('natural', 'astral') } else { @($Profile) }
foreach ($routeProfile in $profiles) {
    Assert-QaRouteCompatibility -RouteProfile $routeProfile -RouteFocus $Focus
}
Assert-QaSurfaceEvidenceCompatibility `
    -SurfaceMode $SurfaceMaterial `
    -HydroMode $Hydro `
    -CohortMode $Cohorts `
    -ViewportWidth $Width `
    -ViewportHeight $Height

if (-not $StaticDryRun -and ($Build -or -not (Test-Path -LiteralPath $executable -PathType Leaf))) {
    Push-Location $projectRoot
    try {
        $cargoArguments = @('build', '--bin', 'voxel-native')
        if ($Configuration -eq 'Release') {
            $cargoArguments += '--release'
        }
        & cargo @cargoArguments
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "QA executable not found: $executable. Static dry runs require an existing binary."
}

# Provenance bounds are deliberately independent of repository size. A source
# tree outside these ceilings fails before Process.Start instead of silently
# producing partial or unbounded evidence.
$MaxSourceFileCount = 4096
$MaxSourceDirectoryCount = 2048
$MaxSourceTreeEntries = 8192
$MaxSourceFileBytes = [uint64](64MB)
$MaxTotalSourceBytes = [uint64](512MB)
$MaxRelativePathUtf8Bytes = 2048
$MaxExecutableBytes = [uint64](1GB)
$MaxGitStatusLines = 100000
$MaxToolchainChars = 160
$MaxHardwareChars = 320
$MaxQaRunDirectoryCount = 8192
$MaxQaRunTreeEntries = 65536
$MaxQaReportCount = 8192
$MaxQaReportBytes = [uint64](16MB)
$MaxQaScreenshotBytes = [uint64](384MB)
$MaxPersistentSettingsBytes = [uint64](16MB)
$MaxQaScreenshotDimension = [uint64]16384
$MaxQaScreenshotPixels = [uint64]268435456
$MaxQaPngChunkCount = 65536
$QaPngReadBufferBytes = 65536
$MaxQaWorldNameChars = 72
$MaxQaDerivedWorldPathChars = 240

# Process.Start happens only after any requested Cargo build has completed.
# This wall-clock budget therefore covers the requested in-engine route plus
# the engine's independent 30-second streaming-settlement and 3-second PNG
# write bounds, with two minutes reserved for process startup, world warm-up,
# camera preflight, and orderly teardown. Every wait below is bounded, and the
# largest supported route remains far below Process.WaitForExit(Int32)'s limit.
$QaProcessCompletionSettleSeconds = [uint64]30
$QaProcessScreenshotWriteSeconds = [uint64]3
$QaProcessStartupAndTeardownReserveSeconds = [uint64]120
$QaProcessGracefulShutdownMilliseconds = 5000
$QaProcessForcedShutdownMilliseconds = 5000

function Get-QaProcessTimeoutMilliseconds {
    param(
        [Parameter(Mandatory = $true)]
        [double]$RouteSeconds
    )

    if ([double]::IsNaN($RouteSeconds) -or
        [double]::IsInfinity($RouteSeconds) -or
        $RouteSeconds -lt 8.0 -or
        $RouteSeconds -gt 600.0) {
        throw 'QA process timeout requires a finite route duration in the public 8..600 second range.'
    }

    $routeMillisecondsDouble = [Math]::Ceiling($RouteSeconds * 1000.0)
    $fixedSeconds = $QaProcessCompletionSettleSeconds +
        $QaProcessScreenshotWriteSeconds +
        $QaProcessStartupAndTeardownReserveSeconds
    $fixedMilliseconds = [uint64]$fixedSeconds * [uint64]1000
    if ($routeMillisecondsDouble -lt 1.0 -or
        $routeMillisecondsDouble -gt [double][int]::MaxValue) {
        throw 'QA route duration cannot be represented as bounded process-wait milliseconds.'
    }
    $routeMilliseconds = [uint64]$routeMillisecondsDouble
    if ($routeMilliseconds -gt ([uint64][int]::MaxValue - $fixedMilliseconds)) {
        throw 'QA process deadline exceeds Process.WaitForExit(Int32) capacity.'
    }
    $timeoutMilliseconds = $routeMilliseconds + $fixedMilliseconds
    if ($timeoutMilliseconds -eq 0 -or $timeoutMilliseconds -gt [uint64][int]::MaxValue) {
        throw 'QA process deadline is outside the positive Int32 millisecond range.'
    }
    return [int]$timeoutMilliseconds
}

function Stop-QaProcessBounded {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$Process
    )

    $gracefulRequested = $false
    $forcedKillRequested = $false
    $exitObserved = $false
    $shutdownErrors = [System.Collections.Generic.List[string]]::new()

    try {
        $gracefulRequested = $Process.CloseMainWindow()
    }
    catch {
        $shutdownErrors.Add("CloseMainWindow failed: $($_.Exception.Message)")
    }
    try {
        $exitObserved = $Process.WaitForExit($QaProcessGracefulShutdownMilliseconds)
    }
    catch {
        $shutdownErrors.Add("bounded graceful wait failed: $($_.Exception.Message)")
    }

    if (-not $exitObserved) {
        try {
            $Process.Kill()
            $forcedKillRequested = $true
        }
        catch [System.InvalidOperationException] {
            # The process may exit between the bounded wait and Kill(). The
            # following bounded wait is the authority for that race.
            $shutdownErrors.Add("forced termination raced with process exit: $($_.Exception.Message)")
        }
        catch {
            $shutdownErrors.Add("forced termination failed: $($_.Exception.Message)")
        }
        try {
            $exitObserved = $Process.WaitForExit($QaProcessForcedShutdownMilliseconds)
        }
        catch {
            $shutdownErrors.Add("bounded forced-termination wait failed: $($_.Exception.Message)")
        }
    }

    return [pscustomobject]@{
        ExitObserved = $exitObserved
        GracefulRequested = $gracefulRequested
        ForcedKillRequested = $forcedKillRequested
        Errors = @($shutdownErrors)
    }
}

function ConvertTo-BoundedProvenanceText {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value,

        [Parameter(Mandatory = $true)]
        [ValidateRange(1, 4096)]
        [int]$MaxChars,

        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    $builder = [System.Text.StringBuilder]::new($Value.Length)
    foreach ($character in $Value.ToCharArray()) {
        if ([char]::IsControl($character)) {
            [void]$builder.Append(' ')
        }
        else {
            [void]$builder.Append($character)
        }
    }
    $normalized = [regex]::Replace($builder.ToString(), '\s+', ' ').Trim()
    if ([string]::IsNullOrWhiteSpace($normalized)) {
        throw "$Label provenance is empty after sanitization."
    }
    if ($normalized.Length -gt $MaxChars) {
        $normalized = $normalized.Substring(0, $MaxChars).TrimEnd()
    }
    if ([string]::IsNullOrWhiteSpace($normalized)) {
        throw "$Label provenance is empty after bounding."
    }
    return $normalized
}

function ConvertTo-LowerHex {
    param(
        [Parameter(Mandatory = $true)]
        [byte[]]$Bytes
    )

    return ([BitConverter]::ToString($Bytes)).Replace('-', '').ToLowerInvariant()
}

function Get-BoundedFileSha256 {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $true)]
        [uint64]$MaxBytes,

        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer) {
        throw "$Label is not a file."
    }
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label is a reparse point; provenance refuses sources outside the fixed tree."
    }
    $initialLength = [uint64]$item.Length
    $initialWriteTimeUtcTicks = [int64]$item.LastWriteTimeUtc.Ticks

    $stream = [System.IO.File]::Open(
        $item.FullName,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
    try {
        $length = [uint64]$stream.Length
        if ($length -gt $MaxBytes) {
            throw "$Label exceeds the hard byte ceiling of $MaxBytes bytes."
        }
        $sha256 = [System.Security.Cryptography.SHA256]::Create()
        try {
            $digest = $sha256.ComputeHash($stream)
        }
        finally {
            $sha256.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }

    $finalItem = Get-Item -LiteralPath $item.FullName -Force -ErrorAction Stop
    if ($finalItem.PSIsContainer -or
        ($finalItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        [uint64]$finalItem.Length -ne $initialLength -or
        [uint64]$finalItem.Length -ne $length -or
        [int64]$finalItem.LastWriteTimeUtc.Ticks -ne $initialWriteTimeUtcTicks) {
        throw "$Label changed while its bounded provenance snapshot was being captured."
    }

    return [pscustomobject]@{
        Length = $length
        Hex = ConvertTo-LowerHex -Bytes $digest
        LastWriteTimeUtcTicks = $initialWriteTimeUtcTicks
    }
}

function Get-QaOptionalFileIdentity {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $true)]
        [uint64]$MaxBytes,

        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    if (-not (Test-Path -LiteralPath $Path)) {
        return [pscustomobject]@{
            Exists = $false
            Length = [uint64]0
            Hex = ''
            LastWriteTimeUtcTicks = [int64]0
        }
    }

    $digest = Get-BoundedFileSha256 -Path $Path -MaxBytes $MaxBytes -Label $Label
    return [pscustomobject]@{
        Exists = $true
        Length = [uint64]$digest.Length
        Hex = [string]$digest.Hex
        LastWriteTimeUtcTicks = [int64]$digest.LastWriteTimeUtcTicks
    }
}

function Assert-QaOptionalFileUnchanged {
    param(
        [Parameter(Mandatory = $true)]
        [psobject]$Expected,

        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $true)]
        [uint64]$MaxBytes,

        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    # This is an endpoint identity assertion, not a filesystem journal.  It
    # detects every lasting existence, byte, length, and timestamp change, but
    # cannot prove that an external actor did not restore an identical file
    # (including its timestamp) between the two snapshots.
    $actual = Get-QaOptionalFileIdentity -Path $Path -MaxBytes $MaxBytes -Label $Label
    if ([bool]$actual.Exists -ne [bool]$Expected.Exists -or
        [uint64]$actual.Length -ne [uint64]$Expected.Length -or
        -not ([string]$actual.Hex).Equals([string]$Expected.Hex, [StringComparison]::Ordinal) -or
        [int64]$actual.LastWriteTimeUtcTicks -ne [int64]$Expected.LastWriteTimeUtcTicks) {
        throw "$Label endpoint identity changed across QA boundaries. Persistent user settings are outside the isolated evidence run and must have identical existence, bytes, length, and timestamp at both endpoints."
    }
}

function Open-QaExecutableReadLock {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $true)]
        [uint64]$MaxBytes
    )

    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        [uint64]$item.Length -gt $MaxBytes) {
        throw 'QA executable cannot be opened as a bounded direct regular file.'
    }

    # FileShare.Read lets the Windows loader map this exact file while denying
    # new write, rename, and delete handles.  Keeping the handle alive across
    # the pre-launch hash, Process.Start, process lifetime, and post-exit hash
    # closes the path swap/restore interval between those checks.
    return [System.IO.File]::Open(
        $item.FullName,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
}

function Get-RootRelativePath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,

        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    # PowerShell 5.1 targets .NET Framework and does not expose
    # Path.GetRelativePath, so use a validated prefix subtraction.
    $canonicalRoot = [System.IO.Path]::GetFullPath($Root).TrimEnd([char[]]'\/')
    $rootPrefix = $canonicalRoot + [System.IO.Path]::DirectorySeparatorChar
    $canonicalPath = [System.IO.Path]::GetFullPath($Path)
    if (-not $canonicalPath.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Build source resolved outside the repository root.'
    }
    $relative = $canonicalPath.Substring($rootPrefix.Length).Replace('\', '/')
    if ([string]::IsNullOrWhiteSpace($relative) -or $relative.StartsWith('../')) {
        throw 'Build source produced an invalid repository-relative path.'
    }
    return $relative
}

function Add-FixedTreeSourceFiles {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,

        [Parameter(Mandatory = $true)]
        [string]$TreeRoot,

        [Parameter(Mandatory = $true)]
        [string]$Extension,

        [Parameter(Mandatory = $true)]
        [System.Collections.Generic.HashSet[string]]$RelativePathSet
    )

    $pendingDirectories = [System.Collections.Generic.Stack[string]]::new()
    $pendingDirectories.Push([System.IO.Path]::GetFullPath($TreeRoot))
    $directoryCount = 0
    $entryCount = 0
    while ($pendingDirectories.Count -gt 0) {
        $directory = $pendingDirectories.Pop()
        $directoryItem = Get-Item -LiteralPath $directory -Force -ErrorAction Stop
        if (($directoryItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw 'Build source tree contains a directory reparse point.'
        }
        $directoryCount++
        if ($directoryCount -gt $MaxSourceDirectoryCount) {
            throw "Build source tree exceeds the $MaxSourceDirectoryCount-directory bound."
        }

        foreach ($entry in Get-ChildItem -LiteralPath $directory -Force -ErrorAction Stop) {
            $entryCount++
            if ($entryCount -gt $MaxSourceTreeEntries) {
                throw "Build source tree exceeds the $MaxSourceTreeEntries-entry bound."
            }
            if ($entry.PSIsContainer) {
                if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                    throw 'Build source tree contains a directory reparse point.'
                }
                $pendingDirectories.Push($entry.FullName)
                continue
            }
            if (-not $entry.Extension.Equals($Extension, [StringComparison]::OrdinalIgnoreCase)) {
                continue
            }
            [void]$RelativePathSet.Add((Get-RootRelativePath -Root $Root -Path $entry.FullName))
            if ($RelativePathSet.Count -gt $MaxSourceFileCount) {
                throw "Build source count exceeds the $MaxSourceFileCount-file bound."
            }
        }
    }
}

function Get-SourceFingerprint {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root
    )

    # source-fingerprint-v1 semantics:
    #   scope  = Cargo.toml, Cargo.lock, optional build/toolchain/.cargo config,
    #            every current-worktree src/**/*.rs (including untracked), and
    #            every current-worktree assets/shaders/**/*.wgsl;
    #   order  = normalized '/' relative paths sorted with ordinal comparison;
    #   record = UTF-8 "pathByteCount:base64(path):fileByteCount:fileSha256\n";
    #   digest = SHA-256("voxel-native-source-fingerprint-v1\n" + records),
    #            using SHA-256 as specified by NIST FIPS 180-4.
    # File timestamps, ACLs and Git index state are intentionally excluded.
    # Generated dependency source, compiler flags and runtime assets outside
    # this fixed scope are known limits and are represented separately where
    # possible by executable/toolchain hashes.
    $requiredManifest = Join-Path $Root 'Cargo.toml'
    if (-not (Test-Path -LiteralPath $requiredManifest -PathType Leaf)) {
        throw 'Cargo.toml is missing; source provenance cannot be complete.'
    }

    $relativePathSet = [System.Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    $rootFiles = @(
        'Cargo.toml',
        'Cargo.lock',
        'build.rs',
        'rust-toolchain.toml',
        'rust-toolchain',
        '.cargo/config.toml',
        '.cargo/config'
    )
    foreach ($relativePath in $rootFiles) {
        $path = Join-Path $Root $relativePath
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            [void]$relativePathSet.Add((Get-RootRelativePath -Root $Root -Path $path))
        }
    }

    $sourceRoot = Join-Path $Root 'src'
    $shaderRoot = Join-Path $Root 'assets\shaders'
    foreach ($fixedRoot in @($sourceRoot, $shaderRoot)) {
        if (-not (Test-Path -LiteralPath $fixedRoot -PathType Container)) {
            throw "Required provenance source root is missing: $fixedRoot"
        }
    }
    Add-FixedTreeSourceFiles `
        -Root $Root `
        -TreeRoot $sourceRoot `
        -Extension '.rs' `
        -RelativePathSet $relativePathSet
    Add-FixedTreeSourceFiles `
        -Root $Root `
        -TreeRoot $shaderRoot `
        -Extension '.wgsl' `
        -RelativePathSet $relativePathSet

    if ($relativePathSet.Count -eq 0 -or $relativePathSet.Count -gt $MaxSourceFileCount) {
        throw "Build source count $($relativePathSet.Count) is outside the 1..$MaxSourceFileCount bound."
    }
    [string[]]$relativePaths = @($relativePathSet)
    [Array]::Sort($relativePaths, [StringComparer]::Ordinal)

    $utf8 = [System.Text.UTF8Encoding]::new($false, $true)
    $aggregate = [System.Security.Cryptography.IncrementalHash]::CreateHash(
        [System.Security.Cryptography.HashAlgorithmName]::SHA256
    )
    [uint64]$totalBytes = 0
    [int64]$newestWriteTimeUtcTicks = 0
    try {
        $aggregate.AppendData($utf8.GetBytes("voxel-native-source-fingerprint-v1`n"))
        foreach ($relativePath in $relativePaths) {
            $pathBytes = $utf8.GetBytes($relativePath)
            if ($pathBytes.Length -gt $MaxRelativePathUtf8Bytes) {
                throw "Build source path exceeds $MaxRelativePathUtf8Bytes UTF-8 bytes."
            }
            $platformPath = $relativePath.Replace(
                '/',
                [System.IO.Path]::DirectorySeparatorChar
            )
            $fileDigest = Get-BoundedFileSha256 `
                -Path (Join-Path $Root $platformPath) `
                -MaxBytes $MaxSourceFileBytes `
                -Label 'Build source'
            if ($fileDigest.Length -gt ($MaxTotalSourceBytes - $totalBytes)) {
                throw "Build source bytes exceed the hard total ceiling of $MaxTotalSourceBytes."
            }
            $totalBytes += [uint64]$fileDigest.Length
            if ($fileDigest.LastWriteTimeUtcTicks -gt $newestWriteTimeUtcTicks) {
                $newestWriteTimeUtcTicks = [int64]$fileDigest.LastWriteTimeUtcTicks
            }
            $record = [string]::Format(
                [Globalization.CultureInfo]::InvariantCulture,
                "{0}:{1}:{2}:{3}`n",
                $pathBytes.Length,
                [Convert]::ToBase64String($pathBytes),
                $fileDigest.Length,
                $fileDigest.Hex
            )
            $aggregate.AppendData($utf8.GetBytes($record))
        }
        $fingerprintHex = ConvertTo-LowerHex -Bytes $aggregate.GetHashAndReset()
    }
    finally {
        $aggregate.Dispose()
    }

    if ($fingerprintHex -notmatch '^[0-9a-f]{64}$') {
        throw 'Source fingerprint did not produce a canonical SHA-256 digest.'
    }
    return [pscustomobject]@{
        Token = "sha256:$fingerprintHex"
        FileCount = $relativePaths.Count
        TotalBytes = $totalBytes
        NewestWriteTimeUtcTicks = $newestWriteTimeUtcTicks
    }
}

function Invoke-GitCapture {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$GitArguments,

        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    $git = @(Get-Command git -CommandType Application -ErrorAction Stop)[0]
    $rawOutput = & $git.Source @GitArguments 2>&1
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) {
        throw "$Label failed with git exit code $exitCode."
    }
    return @($rawOutput | ForEach-Object { [string]$_ })
}

function Get-GitProvenance {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root
    )

    $topLevelOutput = @(Invoke-GitCapture `
            -GitArguments @('-C', $Root, 'rev-parse', '--show-toplevel') `
            -Label 'Git root provenance')
    if ($topLevelOutput.Count -ne 1) {
        throw 'Git root provenance returned an ambiguous result.'
    }
    $gitRoot = [System.IO.Path]::GetFullPath($topLevelOutput[0].Trim()).TrimEnd([char[]]'\/')
    $expectedRoot = [System.IO.Path]::GetFullPath($Root).TrimEnd([char[]]'\/')
    if (-not $gitRoot.Equals($expectedRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'QA project root does not match the Git worktree root.'
    }

    $shaOutput = @(Invoke-GitCapture `
            -GitArguments @('-C', $Root, 'rev-parse', '--verify', 'HEAD^{commit}') `
            -Label 'Git SHA provenance')
    if ($shaOutput.Count -ne 1) {
        throw 'Git SHA provenance returned an ambiguous result.'
    }
    $gitSha = $shaOutput[0].Trim().ToLowerInvariant()
    if ($gitSha -notmatch '^(?:[0-9a-f]{40}|[0-9a-f]{64})$') {
        throw 'Git SHA provenance is not a canonical SHA-1 or SHA-256 object id.'
    }

    # Protected runtime evidence is excluded from the dirty query. No names or
    # contents from saves/, qa_runs/ or agent_runs/ enter provenance or logs.
    $statusOutput = @(Invoke-GitCapture -GitArguments @(
            '-C', $Root,
            'status', '--porcelain=v1', '--untracked-files=normal',
        '--', '.',
        ':(exclude)saves',
        ':(exclude)saves/**',
        ':(exclude)qa_runs',
        ':(exclude)qa_runs/**',
        ':(exclude)agent_runs',
        ':(exclude)agent_runs/**'
        ) -Label 'Git dirty provenance')
    if ($statusOutput.Count -gt $MaxGitStatusLines) {
        throw "Git dirty provenance exceeds the $MaxGitStatusLines-line bound."
    }
    $gitDirty = @($statusOutput | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }).Count -gt 0

    return [pscustomobject]@{
        Sha = $gitSha
        Dirty = $gitDirty
    }
}

function Get-RustcToolchainProvenance {
    $rustc = @(Get-Command rustc -CommandType Application -ErrorAction Stop)[0]
    $rawOutput = & $rustc.Source '-vV' 2>&1
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) {
        throw "rustc toolchain provenance failed with exit code $exitCode."
    }
    [string[]]$lines = @($rawOutput | ForEach-Object { ([string]$_).Trim() })
    $versionLine = $lines | Where-Object { $_ -like 'rustc *' } | Select-Object -First 1
    $hostLine = $lines | Where-Object { $_ -like 'host:*' } | Select-Object -First 1
    $llvmLine = $lines | Where-Object { $_ -like 'LLVM version:*' } | Select-Object -First 1
    if ([string]::IsNullOrWhiteSpace($versionLine) -or
        [string]::IsNullOrWhiteSpace($hostLine) -or
        [string]::IsNullOrWhiteSpace($llvmLine)) {
        throw 'rustc toolchain provenance is missing version, host, or LLVM data.'
    }
    return ConvertTo-BoundedProvenanceText `
        -Value "$versionLine; $hostLine; $llvmLine" `
        -MaxChars $MaxToolchainChars `
        -Label 'Toolchain'
}

function Get-HardwareProvenance {
    $processors = @(Get-CimInstance -ClassName Win32_Processor -ErrorAction Stop)
    $computerSystems = @(Get-CimInstance -ClassName Win32_ComputerSystem -ErrorAction Stop)
    $videoControllers = @(Get-CimInstance -ClassName Win32_VideoController -ErrorAction Stop)
    if ($processors.Count -eq 0 -or $computerSystems.Count -ne 1 -or $videoControllers.Count -eq 0) {
        throw 'Hardware provenance requires CPU, one computer-system record, and at least one GPU.'
    }

    $cpuName = ([string]($processors | Select-Object -First 1 -ExpandProperty Name)).Trim()
    $logicalProcessors = ($processors |
        Measure-Object -Property NumberOfLogicalProcessors -Sum).Sum
    $memoryBytes = [uint64]$computerSystems[0].TotalPhysicalMemory
    if ([string]::IsNullOrWhiteSpace($cpuName) -or $logicalProcessors -le 0 -or $memoryBytes -eq 0) {
        throw 'Hardware provenance returned invalid CPU, logical-core, or memory data.'
    }
    $gpuNames = @($videoControllers |
        ForEach-Object { ([string]$_.Name).Trim() } |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
        Select-Object -Unique -First 2)
    if ($gpuNames.Count -eq 0) {
        throw 'Hardware provenance returned no bounded GPU name.'
    }
    $memoryGiB = [double]$memoryBytes / [double](1GB)
    $memoryText = [string]::Format(
        [Globalization.CultureInfo]::InvariantCulture,
        '{0:0.0}',
        $memoryGiB
    )
    $hardware = "CPU=$cpuName; logical=$logicalProcessors; RAM=${memoryText}GiB; GPU=$($gpuNames -join ' + ')"
    return ConvertTo-BoundedProvenanceText `
        -Value $hardware `
        -MaxChars $MaxHardwareChars `
        -Label 'Hardware'
}

function Assert-QaExecutableFreshness {
    param(
        [Parameter(Mandatory = $true)]
        [int64]$SourceNewestWriteTimeUtcTicks,

        [Parameter(Mandatory = $true)]
        [int64]$ExecutableWriteTimeUtcTicks
    )

    # This is a deliberately conservative freshness boundary over the exact
    # source-fingerprint-v1 file set. It proves only that the selected binary
    # was written strictly after every controlled source file; it cannot prove
    # historical compiler inputs. Exact source/executable hashes are therefore
    # captured again immediately before and after every launch and bound into
    # the report. Timestamp equality, clock rollback, or missing metadata fail
    # closed and require an explicit rebuild.
    if ($SourceNewestWriteTimeUtcTicks -le 0 -or $ExecutableWriteTimeUtcTicks -le 0) {
        throw 'QA source/executable freshness metadata is unavailable.'
    }
    if ($ExecutableWriteTimeUtcTicks -le $SourceNewestWriteTimeUtcTicks) {
        throw 'QA executable is stale relative to the controlled source snapshot; rebuild it before collecting evidence.'
    }
}

function Get-QaControlledArtifactSnapshot {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,

        [Parameter(Mandatory = $true)]
        [string]$ExecutablePath
    )

    $source = Get-SourceFingerprint -Root $Root
    $executableDigest = Get-BoundedFileSha256 `
        -Path $ExecutablePath `
        -MaxBytes $MaxExecutableBytes `
        -Label 'QA executable'
    if ($executableDigest.Hex -notmatch '^[0-9a-f]{64}$') {
        throw 'Executable provenance did not produce a canonical SHA-256 digest.'
    }
    Assert-QaExecutableFreshness `
        -SourceNewestWriteTimeUtcTicks $source.NewestWriteTimeUtcTicks `
        -ExecutableWriteTimeUtcTicks $executableDigest.LastWriteTimeUtcTicks

    return [pscustomobject]@{
        SourceFingerprint = $source.Token
        SourceFileCount = $source.FileCount
        SourceBytes = $source.TotalBytes
        SourceNewestWriteTimeUtcTicks = $source.NewestWriteTimeUtcTicks
        ExecutableHash = "sha256:$($executableDigest.Hex)"
        ExecutableBytes = $executableDigest.Length
        ExecutableWriteTimeUtcTicks = $executableDigest.LastWriteTimeUtcTicks
    }
}

function Assert-QaArtifactSnapshotIdentity {
    param(
        [Parameter(Mandatory = $true)]
        $Expected,

        [Parameter(Mandatory = $true)]
        $Actual,

        [Parameter(Mandatory = $true)]
        [string]$Boundary
    )

    foreach ($field in @(
            'SourceFingerprint',
            'SourceFileCount',
            'SourceBytes',
            'SourceNewestWriteTimeUtcTicks',
            'ExecutableHash',
            'ExecutableBytes',
            'ExecutableWriteTimeUtcTicks'
        )) {
        if (-not ([string]$Actual.$field).Equals(
                [string]$Expected.$field,
                [StringComparison]::Ordinal)) {
            throw "QA controlled artifact field $field changed at the $Boundary boundary."
        }
    }
}

function Assert-QaControlledArtifactsUnchanged {
    param(
        [Parameter(Mandatory = $true)]
        $Expected,

        [Parameter(Mandatory = $true)]
        [string]$Root,

        [Parameter(Mandatory = $true)]
        [string]$ExecutablePath,

        [Parameter(Mandatory = $true)]
        [string]$Boundary
    )

    $actual = Get-QaControlledArtifactSnapshot -Root $Root -ExecutablePath $ExecutablePath
    Assert-QaArtifactSnapshotIdentity -Expected $Expected -Actual $actual -Boundary $Boundary
}

function Get-QaProvenance {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,

        [Parameter(Mandatory = $true)]
        [string]$ExecutablePath
    )

    $git = Get-GitProvenance -Root $Root
    $artifacts = Get-QaControlledArtifactSnapshot -Root $Root -ExecutablePath $ExecutablePath
    $toolchain = Get-RustcToolchainProvenance
    $hardware = Get-HardwareProvenance

    return [pscustomobject]@{
        GitSha = $git.Sha
        GitDirty = [bool]$git.Dirty
        SourceFingerprint = $artifacts.SourceFingerprint
        SourceFileCount = $artifacts.SourceFileCount
        SourceBytes = $artifacts.SourceBytes
        SourceNewestWriteTimeUtcTicks = $artifacts.SourceNewestWriteTimeUtcTicks
        ExecutableHash = $artifacts.ExecutableHash
        ExecutableBytes = $artifacts.ExecutableBytes
        ExecutableWriteTimeUtcTicks = $artifacts.ExecutableWriteTimeUtcTicks
        Toolchain = $toolchain
        Hardware = $hardware
    }
}

function Write-QaProvenance {
    param(
        [Parameter(Mandatory = $true)]
        $Provenance
    )

    $dirtyText = if ($Provenance.GitDirty) { 'true' } else { 'false' }
    Write-Host "QA provenance: git_sha=$($Provenance.GitSha) git_dirty=$dirtyText"
    Write-Host "QA provenance: source=$($Provenance.SourceFingerprint) files=$($Provenance.SourceFileCount) bytes=$($Provenance.SourceBytes)"
    Write-Host "QA provenance: executable=$($Provenance.ExecutableHash) bytes=$($Provenance.ExecutableBytes)"
    Write-Host "QA provenance: toolchain=$($Provenance.Toolchain)"
    Write-Host "QA provenance: hardware=$($Provenance.Hardware)"
}

function Get-BoundedQaReportPaths {
    param(
        [Parameter(Mandatory = $true)]
        [string]$QaRunsRoot
    )

    if (-not (Test-Path -LiteralPath $QaRunsRoot -PathType Container)) {
        return @()
    }

    $canonicalRoot = [System.IO.Path]::GetFullPath($QaRunsRoot).TrimEnd([char[]]'\/')
    $rootItem = Get-Item -LiteralPath $canonicalRoot -Force -ErrorAction Stop
    if (($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'QA evidence root is a reparse point; report discovery refuses an indirect root.'
    }

    $pendingDirectories = [System.Collections.Generic.Stack[string]]::new()
    $reportPaths = [System.Collections.Generic.List[string]]::new()
    $pendingDirectories.Push($canonicalRoot)
    $directoryCount = 0
    $entryCount = 0
    while ($pendingDirectories.Count -gt 0) {
        $directory = $pendingDirectories.Pop()
        $directoryCount++
        if ($directoryCount -gt $MaxQaRunDirectoryCount) {
            throw "QA evidence discovery exceeds the $MaxQaRunDirectoryCount-directory bound."
        }

        foreach ($entry in Get-ChildItem -LiteralPath $directory -Force -ErrorAction Stop) {
            $entryCount++
            if ($entryCount -gt $MaxQaRunTreeEntries) {
                throw "QA evidence discovery exceeds the $MaxQaRunTreeEntries-entry bound."
            }
            if ($entry.PSIsContainer) {
                if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                    throw 'QA evidence discovery encountered a directory reparse point.'
                }
                $pendingDirectories.Push($entry.FullName)
                continue
            }
            if (-not $entry.Name.Equals('report.ron', [StringComparison]::OrdinalIgnoreCase)) {
                continue
            }
            if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw 'QA evidence discovery encountered a report reparse point.'
            }
            $reportPaths.Add([System.IO.Path]::GetFullPath($entry.FullName))
            if ($reportPaths.Count -gt $MaxQaReportCount) {
                throw "QA evidence discovery exceeds the $MaxQaReportCount-report bound."
            }
        }
    }

    [string[]]$sortedPaths = $reportPaths.ToArray()
    [Array]::Sort($sortedPaths, [StringComparer]::OrdinalIgnoreCase)
    return $sortedPaths
}

function ConvertTo-RonStringContent {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value
    )

    $builder = [System.Text.StringBuilder]::new($Value.Length)
    foreach ($character in $Value.ToCharArray()) {
        if ([char]::IsControl($character)) {
            throw 'Expected report identity contains a control character that cannot be matched safely.'
        }
        if ([int]$character -eq 0x5c) {
            [void]$builder.Append('\\')
        }
        elseif ([int]$character -eq 0x22) {
            [void]$builder.Append('\"')
        }
        else {
            [void]$builder.Append($character)
        }
    }
    return $builder.ToString()
}

function Get-QaUniqueUnsignedFieldValue {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ReportText,

        [Parameter(Mandatory = $true)]
        [string]$FieldName,

        [Parameter(Mandatory = $true)]
        [uint64]$Maximum
    )

    $matches = [regex]::Matches(
        $ReportText,
        '(?m)^\s*' + [regex]::Escape($FieldName) + ':\s*([0-9]+),?\s*$'
    )
    if ($matches.Count -ne 1) {
        throw "Selected QA report contains $($matches.Count) $FieldName field declarations; exactly one bounded integer is required."
    }
    $value = [uint64]0
    if (-not [uint64]::TryParse(
            $matches[0].Groups[1].Value,
            [Globalization.NumberStyles]::None,
            [Globalization.CultureInfo]::InvariantCulture,
            [ref]$value) -or $value -gt $Maximum) {
        throw "Selected QA report $FieldName exceeds its $Maximum bound."
    }
    return $value
}

function Get-QaUniqueSignedFieldValue {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ReportText,

        [Parameter(Mandatory = $true)]
        [string]$FieldName,

        [Parameter(Mandatory = $true)]
        [int64]$Minimum,

        [Parameter(Mandatory = $true)]
        [int64]$Maximum
    )

    $matches = [regex]::Matches(
        $ReportText,
        '(?m)^\s*' + [regex]::Escape($FieldName) + ':\s*(-?[0-9]+),?\s*$'
    )
    if ($matches.Count -ne 1) {
        throw "Selected QA report contains $($matches.Count) $FieldName field declarations; exactly one bounded signed integer is required."
    }
    $value = [int64]0
    if (-not [int64]::TryParse(
            $matches[0].Groups[1].Value,
            [Globalization.NumberStyles]::AllowLeadingSign,
            [Globalization.CultureInfo]::InvariantCulture,
            [ref]$value) -or $value -lt $Minimum -or $value -gt $Maximum) {
        throw "Selected QA report $FieldName is outside its $Minimum..$Maximum bound."
    }
    return $value
}

function Assert-QaL0SamplingIdentity {
    param(
        [Parameter(Mandatory = $true)]
        [string]$L0HeightModeLabel,

        [Parameter(Mandatory = $true)]
        [string]$CacheUpdate,

        [Parameter(Mandatory = $true)]
        [int64]$ShiftX,

        [Parameter(Mandatory = $true)]
        [int64]$ShiftZ,

        [Parameter(Mandatory = $true)]
        [uint64]$CenterQueries,

        [Parameter(Mandatory = $true)]
        [uint64]$HalfXQueries,

        [Parameter(Mandatory = $true)]
        [uint64]$HalfZQueries,

        [Parameter(Mandatory = $true)]
        [uint64]$ReusedHeightSamples
    )

    $candidate = $L0HeightModeLabel -eq 'CardinalTrimmed8V1'
    $absX = [uint64][Math]::Abs($ShiftX)
    $absZ = [uint64][Math]::Abs($ShiftZ)
    [uint64]$expectedCenter = 0
    [uint64]$expectedHalfX = 0
    [uint64]$expectedHalfZ = 0
    [uint64]$expectedReused = 0

    switch ($CacheUpdate) {
        { $_ -eq 'Cold' -or $_ -eq 'IncompatibleFallback' } {
            if ($ShiftX -ne 0 -or $ShiftZ -ne 0) {
                throw "Selected QA report $CacheUpdate L0 cache update must have a zero shift."
            }
            $expectedCenter = 4225
            if ($candidate) {
                $expectedHalfX = 4290
                $expectedHalfZ = 4290
            }
        }
        'TeleportFallback' {
            if (($absX -ne 0 -or $absZ -ne 0) -and $absX -lt 65 -and $absZ -lt 65) {
                throw 'Selected QA report TeleportFallback L0 cache update is neither the unrepresentable-delta zero sentinel nor a shift crossing the 65-cell fallback boundary.'
            }
            $expectedCenter = 4225
            if ($candidate) {
                $expectedHalfX = 4290
                $expectedHalfZ = 4290
            }
        }
        'IncrementalStrip' {
            if ($absX -ge 65 -or $absZ -ge 65) {
                throw 'Selected QA report IncrementalStrip L0 cache shift exceeds the 64-cell overlap boundary.'
            }
            $expectedCenter = [uint64](4225 - ((65 - $absX) * (65 - $absZ)))
            $expectedReused = [uint64](4225 - $expectedCenter)
            if ($candidate) {
                $expectedHalfX = [uint64](4290 - ((66 - $absX) * (65 - $absZ)))
                $expectedHalfZ = [uint64](4290 - ((65 - $absX) * (66 - $absZ)))
                $expectedReused += [uint64](4290 - $expectedHalfX)
                $expectedReused += [uint64](4290 - $expectedHalfZ)
            }
        }
        default {
            throw "Selected QA report contains unsupported last_l0_cache_update '$CacheUpdate'."
        }
    }

    if ($CenterQueries -ne $expectedCenter -or
        $HalfXQueries -ne $expectedHalfX -or
        $HalfZQueries -ne $expectedHalfZ -or
        $ReusedHeightSamples -ne $expectedReused) {
        throw "Selected QA report L0 query/reuse counters do not match the exact $CacheUpdate cache-shift sampling identity."
    }
}

function Get-QaUniqueFiniteFieldValue {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ReportText,

        [Parameter(Mandatory = $true)]
        [string]$FieldName
    )

    $numberPattern = '[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?'
    $matches = [regex]::Matches(
        $ReportText,
        '(?m)^\s*' + [regex]::Escape($FieldName) + ':\s*(' + $numberPattern + '),?\s*$'
    )
    if ($matches.Count -ne 1) {
        throw "Selected QA report must contain exactly one finite $FieldName value."
    }
    $value = [double]0
    if (-not [double]::TryParse(
            $matches[0].Groups[1].Value,
            [Globalization.NumberStyles]::Float,
            [Globalization.CultureInfo]::InvariantCulture,
            [ref]$value) -or
        [double]::IsNaN($value) -or
        [double]::IsInfinity($value)) {
        throw "Selected QA report contains an invalid $FieldName value."
    }
    return $value
}

function Assert-QaDerivedWorldPathBudget {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,

        [Parameter(Mandatory = $true)]
        [string]$WorldName
    )

    if ($WorldName.Length -gt $MaxQaWorldNameChars -or $WorldName -notmatch '^[a-z0-9_-]+$') {
        throw "QA world identity must be lowercase filesystem-safe ASCII and at most $MaxQaWorldNameChars characters."
    }

    # This is deliberately more conservative than the currently reachable
    # PID/transaction counter. It proves the longest V3 previous-snapshot chunk
    # path remains below a legacy Windows MAX_PATH margin without touching saves.
    $longestTransactionName = '.grammar_v3.previous-4294967295-18446744073709551615'
    $longestChunkName = '-2147483648_-2147483648_-2147483648.ron'
    $relativeCandidates = @(
        "saves\$WorldName.v3",
        "saves\${WorldName}_edits\grammar_v3\manifest.ron",
        "saves\${WorldName}_edits\$longestTransactionName\chunks\$longestChunkName"
    )
    foreach ($relativePath in $relativeCandidates) {
        $derivedPath = [System.IO.Path]::GetFullPath((Join-Path $Root $relativePath))
        if ($derivedPath.Length -gt $MaxQaDerivedWorldPathChars) {
            throw "Derived QA world path length $($derivedPath.Length) exceeds the conservative $MaxQaDerivedWorldPathChars-character bound: $derivedPath"
        }
    }
}

function Get-QaSelectedReportContext {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ReportPath,

        [Parameter(Mandatory = $true)]
        [string]$QaRunsRoot
    )

    $rootItem = Get-Item -LiteralPath $QaRunsRoot -Force -ErrorAction Stop
    if (-not $rootItem.PSIsContainer -or
        ($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        -not $rootItem.Name.Equals('qa_runs', [StringComparison]::Ordinal)) {
        throw 'Selected QA evidence root must be the direct non-reparse qa_runs directory.'
    }
    $canonicalRoot = [System.IO.Path]::GetFullPath($rootItem.FullName).TrimEnd([char[]]'\/')

    $reportItem = Get-Item -LiteralPath $ReportPath -Force -ErrorAction Stop
    if ($reportItem.PSIsContainer -or
        ($reportItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        -not $reportItem.Name.Equals('report.ron', [StringComparison]::Ordinal)) {
        throw 'Selected QA report must be the direct non-reparse report.ron file.'
    }
    $runItem = Get-Item -LiteralPath $reportItem.DirectoryName -Force -ErrorAction Stop
    if (-not $runItem.PSIsContainer -or
        ($runItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $runItem.Name -notmatch '^run_[0-9]+$') {
        throw 'Selected QA run must be a direct non-reparse run_<epoch> directory.'
    }
    $runParent = [System.IO.Path]::GetFullPath(
        [System.IO.Directory]::GetParent($runItem.FullName).FullName
    ).TrimEnd([char[]]'\/')
    if (-not $runParent.Equals($canonicalRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Selected QA report is not directly beneath the selected qa_runs root.'
    }

    $canonicalRun = [System.IO.Path]::GetFullPath($runItem.FullName).TrimEnd([char[]]'\/')
    $canonicalReport = [System.IO.Path]::GetFullPath($reportItem.FullName)
    $expectedReport = [System.IO.Path]::GetFullPath((Join-Path $canonicalRun 'report.ron'))
    if (-not $canonicalReport.Equals($expectedReport, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Selected QA report did not resolve to the exact canonical run report path.'
    }

    return [pscustomobject]@{
        QaRunsRoot = $canonicalRoot
        EvidenceBase = [System.IO.Directory]::GetParent($canonicalRoot).FullName
        RunDirectory = $canonicalRun
        RunName = $runItem.Name
        ReportPath = $canonicalReport
    }
}

function ConvertFrom-QaRonScreenshotPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RawPath
    )

    # The engine emits an ASCII relative path. Accept only ordinary path bytes
    # and RON's escaped backslash; all other escapes are ambiguous for evidence
    # resolution and fail closed.
    if ($RawPath.Length -eq 0 -or
        $RawPath.Length -gt 1024 -or
        $RawPath -notmatch '^(?:[A-Za-z0-9_.\-/]|\\\\)+$') {
        throw 'Selected QA report screenshot path contains an unsupported RON escape or character.'
    }
    $decoded = $RawPath.Replace('\\', '\').Replace('/', '\')
    if ($decoded.Length -eq 0 -or
        $decoded.Length -gt 512 -or
        [System.IO.Path]::IsPathRooted($decoded) -or
        $decoded.StartsWith('\', [StringComparison]::Ordinal) -or
        $decoded.Contains(':')) {
        throw 'Selected QA report screenshot path is rooted, drive-qualified, or outside its character cap.'
    }
    $segments = @([regex]::Split($decoded, '[\\/]'))
    if ($segments.Count -ne 3 -or
        @($segments | Where-Object {
                [string]::IsNullOrWhiteSpace($_) -or $_ -eq '.' -or $_ -eq '..'
            }).Count -ne 0) {
        throw 'Selected QA report screenshot path is not an exact direct-child evidence path.'
    }
    return $decoded
}

function Read-QaExactBytes {
    param(
        [Parameter(Mandatory = $true)]
        [System.IO.Stream]$Stream,

        [Parameter(Mandatory = $true)]
        [ValidateRange(0, 65536)]
        [int]$Count,

        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    $buffer = [byte[]]::new($Count)
    $offset = 0
    while ($offset -lt $Count) {
        $read = $Stream.Read($buffer, $offset, $Count - $offset)
        if ($read -le 0) {
            throw "$Label ended before its declared PNG structure was complete."
        }
        $offset += $read
    }
    return ,$buffer
}

function Get-QaUInt32BigEndian {
    param(
        [Parameter(Mandatory = $true)]
        [byte[]]$Bytes,

        [ValidateRange(0, 65532)]
        [int]$Offset = 0
    )

    if ($Bytes.Length -lt $Offset + 4) {
        throw 'PNG big-endian integer read exceeded its fixed buffer.'
    }
    return [uint32]((([uint64]$Bytes[$Offset] -shl 24) -bor
            ([uint64]$Bytes[$Offset + 1] -shl 16) -bor
            ([uint64]$Bytes[$Offset + 2] -shl 8) -bor
            [uint64]$Bytes[$Offset + 3]) -band [uint64]4294967295)
}

function Get-QaPngCrc32Table {
    if ($null -eq $script:QaPngCrc32Table) {
        $table = [uint32[]]::new(256)
        [uint32]$polynomial = 3988292384
        for ($index = 0; $index -lt $table.Length; $index++) {
            [uint32]$value = $index
            for ($bit = 0; $bit -lt 8; $bit++) {
                if (($value -band 1) -ne 0) {
                    $value = [uint32](([uint64]$polynomial -bxor
                            ([uint64]$value -shr 1)) -band [uint64]4294967295)
                }
                else {
                    $value = [uint32]([uint64]$value -shr 1)
                }
            }
            $table[$index] = $value
        }
        $script:QaPngCrc32Table = $table
    }
    return ,$script:QaPngCrc32Table
}

function Update-QaPngCrc32 {
    param(
        [Parameter(Mandatory = $true)]
        [uint32]$Crc,

        [Parameter(Mandatory = $true)]
        [byte[]]$Bytes,

        [Parameter(Mandatory = $true)]
        [ValidateRange(0, 65536)]
        [int]$Count
    )

    if ($Count -gt $Bytes.Length) {
        throw 'PNG CRC input count exceeded its fixed buffer.'
    }
    $table = Get-QaPngCrc32Table
    [uint32]$current = $Crc
    for ($index = 0; $index -lt $Count; $index++) {
        $tableIndex = [int](([uint64]$current -bxor [uint64]$Bytes[$index]) -band 255)
        $current = [uint32](([uint64]$table[$tableIndex] -bxor
                ([uint64]$current -shr 8)) -band [uint64]4294967295)
    }
    return $current
}

function Assert-QaPngEvidenceFile {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $true)]
        [uint64]$ExpectedWidth,

        [Parameter(Mandatory = $true)]
        [uint64]$ExpectedHeight
    )

    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'Referenced QA screenshot is not a direct non-reparse regular file.'
    }
    $initialLength = [uint64]$item.Length
    $initialWriteTimeUtcTicks = [int64]$item.LastWriteTimeUtc.Ticks
    if ($initialLength -lt 57 -or $initialLength -gt $MaxQaScreenshotBytes) {
        throw "Referenced QA screenshot is outside the 57..$MaxQaScreenshotBytes byte contract."
    }

    # The validator streams chunk payloads through a fixed 64 KiB buffer. It
    # verifies signature, CRCs, IHDR semantics, critical-chunk ordering, IDAT
    # presence, terminal IEND, and absence of trailing bytes. It deliberately
    # does not inflate IDAT; pixel decoding remains the visual/analyzer stage.
    $stream = [System.IO.File]::Open(
        $item.FullName,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
    try {
        $signature = Read-QaExactBytes -Stream $stream -Count 8 -Label 'QA screenshot'
        [byte[]]$expectedSignature = @(137, 80, 78, 71, 13, 10, 26, 10)
        for ($index = 0; $index -lt $expectedSignature.Length; $index++) {
            if ($signature[$index] -ne $expectedSignature[$index]) {
                throw 'Referenced QA screenshot does not have the canonical PNG signature.'
            }
        }

        $chunkCount = 0
        $sawIhdr = $false
        $sawPlte = $false
        $sawIdat = $false
        $idatEnded = $false
        $sawIend = $false
        [uint64]$idatBytes = 0
        [byte]$colorType = 255
        $readBuffer = [byte[]]::new($QaPngReadBufferBytes)
        while (-not $sawIend) {
            $chunkCount++
            if ($chunkCount -gt $MaxQaPngChunkCount) {
                throw "Referenced QA screenshot exceeds the $MaxQaPngChunkCount PNG-chunk bound."
            }
            $header = Read-QaExactBytes -Stream $stream -Count 8 -Label 'QA screenshot'
            $chunkLength = [uint64](Get-QaUInt32BigEndian -Bytes $header)
            [byte[]]$chunkTypeBytes = @($header[4], $header[5], $header[6], $header[7])
            $chunkType = [System.Text.Encoding]::ASCII.GetString($chunkTypeBytes)
            if ($chunkType -notmatch '^[A-Za-z]{4}$') {
                throw 'Referenced QA screenshot contains an invalid PNG chunk type.'
            }
            $bytesRemaining = [uint64]($stream.Length - $stream.Position)
            if ($chunkLength -gt $bytesRemaining -or
                $bytesRemaining - $chunkLength -lt 4) {
                throw 'Referenced QA screenshot contains a truncated or overflowing PNG chunk.'
            }
            if ($chunkCount -eq 1 -and $chunkType -ne 'IHDR') {
                throw 'Referenced QA screenshot does not begin with PNG IHDR.'
            }

            switch ($chunkType) {
                'IHDR' {
                    if ($sawIhdr -or $chunkLength -ne 13) {
                        throw 'Referenced QA screenshot contains a duplicate or malformed PNG IHDR.'
                    }
                }
                'PLTE' {
                    if (-not $sawIhdr -or $sawPlte -or $sawIdat -or
                        $chunkLength -eq 0 -or $chunkLength -gt 768 -or
                        ($chunkLength % 3) -ne 0) {
                        throw 'Referenced QA screenshot contains a misplaced or malformed PNG PLTE.'
                    }
                    $sawPlte = $true
                }
                'IDAT' {
                    if (-not $sawIhdr -or $idatEnded) {
                        throw 'Referenced QA screenshot contains a misplaced or non-contiguous PNG IDAT.'
                    }
                    $sawIdat = $true
                    $idatBytes += $chunkLength
                }
                'IEND' {
                    if (-not $sawIhdr -or -not $sawIdat -or
                        $idatBytes -eq 0 -or $chunkLength -ne 0) {
                        throw 'Referenced QA screenshot contains a premature or malformed PNG IEND.'
                    }
                }
                default {
                    if ([char]::IsUpper($chunkType[0])) {
                        throw "Referenced QA screenshot contains unsupported critical PNG chunk $chunkType."
                    }
                    if (-not $sawIhdr) {
                        throw 'Referenced QA screenshot contains an ancillary chunk before IHDR.'
                    }
                }
            }
            if ($sawIdat -and $chunkType -ne 'IDAT' -and $chunkType -ne 'IEND') {
                $idatEnded = $true
            }

            [uint32]$crc = [uint32]::MaxValue
            $crc = Update-QaPngCrc32 -Crc $crc -Bytes $chunkTypeBytes -Count 4
            $ihdrData = $null
            if ($chunkType -eq 'IHDR') {
                $ihdrData = Read-QaExactBytes -Stream $stream -Count 13 -Label 'QA screenshot'
                $crc = Update-QaPngCrc32 -Crc $crc -Bytes $ihdrData -Count 13
            }
            else {
                [uint64]$remainingChunkBytes = $chunkLength
                while ($remainingChunkBytes -gt 0) {
                    $readCount = [int][Math]::Min([uint64]$readBuffer.Length, $remainingChunkBytes)
                    $actualRead = $stream.Read($readBuffer, 0, $readCount)
                    if ($actualRead -ne $readCount) {
                        throw 'Referenced QA screenshot ended inside a declared PNG chunk.'
                    }
                    $crc = Update-QaPngCrc32 -Crc $crc -Bytes $readBuffer -Count $actualRead
                    $remainingChunkBytes -= [uint64]$actualRead
                }
            }
            $storedCrcBytes = Read-QaExactBytes -Stream $stream -Count 4 -Label 'QA screenshot'
            $storedCrc = Get-QaUInt32BigEndian -Bytes $storedCrcBytes
            $computedCrc = [uint32](([uint64]$crc -bxor [uint64]4294967295) -band [uint64]4294967295)
            if ($storedCrc -ne $computedCrc) {
                throw "Referenced QA screenshot PNG chunk $chunkType has an invalid CRC."
            }

            if ($chunkType -eq 'IHDR') {
                $pngWidth = [uint64](Get-QaUInt32BigEndian -Bytes $ihdrData -Offset 0)
                $pngHeight = [uint64](Get-QaUInt32BigEndian -Bytes $ihdrData -Offset 4)
                [byte]$bitDepth = $ihdrData[8]
                $colorType = $ihdrData[9]
                [byte]$compressionMethod = $ihdrData[10]
                [byte]$filterMethod = $ihdrData[11]
                [byte]$interlaceMethod = $ihdrData[12]
                $validDepth = switch ($colorType) {
                    0 { $bitDepth -in @(1, 2, 4, 8, 16) }
                    2 { $bitDepth -in @(8, 16) }
                    3 { $bitDepth -in @(1, 2, 4, 8) }
                    4 { $bitDepth -in @(8, 16) }
                    6 { $bitDepth -in @(8, 16) }
                    default { $false }
                }
                if ($pngWidth -eq 0 -or $pngHeight -eq 0 -or
                    $pngWidth -ne $ExpectedWidth -or $pngHeight -ne $ExpectedHeight -or
                    $pngWidth -gt $MaxQaScreenshotDimension -or
                    $pngHeight -gt $MaxQaScreenshotDimension -or
                    $pngWidth * $pngHeight -gt $MaxQaScreenshotPixels -or
                    -not $validDepth -or $compressionMethod -ne 0 -or
                    $filterMethod -ne 0 -or $interlaceMethod -gt 1) {
                    throw 'Referenced QA screenshot PNG IHDR does not match the bounded reported physical viewport.'
                }
                $sawIhdr = $true
            }
            elseif ($chunkType -eq 'IEND') {
                if ($stream.Position -ne $stream.Length) {
                    throw 'Referenced QA screenshot contains trailing bytes after terminal PNG IEND.'
                }
                $sawIend = $true
            }
        }
        if (-not $sawIhdr -or -not $sawIdat -or -not $sawIend -or
            ($colorType -eq 3 -and -not $sawPlte) -or
            ($colorType -in @(0, 4) -and $sawPlte)) {
            throw 'Referenced QA screenshot does not satisfy the required PNG critical-chunk structure.'
        }
    }
    finally {
        $stream.Dispose()
    }

    $finalItem = Get-Item -LiteralPath $item.FullName -Force -ErrorAction Stop
    if ($finalItem.PSIsContainer -or
        ($finalItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        [uint64]$finalItem.Length -ne $initialLength -or
        [int64]$finalItem.LastWriteTimeUtc.Ticks -ne $initialWriteTimeUtcTicks) {
        throw 'Referenced QA screenshot changed during bounded PNG validation.'
    }
}

function Assert-QaScreenshotObservations {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ReportText,

        [Parameter(Mandatory = $true)]
        [string]$ReportPath,

        [Parameter(Mandatory = $true)]
        [string]$QaRunsRoot
    )

    $reportContext = Get-QaSelectedReportContext `
        -ReportPath $ReportPath `
        -QaRunsRoot $QaRunsRoot
    $viewportNumberPattern = '[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?'
    $viewportMatches = [regex]::Matches(
        $ReportText,
        '(?ms)^\s*viewport:\s*Some\(\(\s*\r?\n' +
            '\s*logical_width:\s*' + $viewportNumberPattern + ',?\s*\r?\n' +
            '\s*logical_height:\s*' + $viewportNumberPattern + ',?\s*\r?\n' +
            '\s*physical_width:\s*([0-9]+),?\s*\r?\n' +
            '\s*physical_height:\s*([0-9]+),?\s*\r?\n' +
            '\s*scale_factor:\s*' + $viewportNumberPattern + ',?\s*\r?\n' +
            '\s*dpi_percent:\s*' + $viewportNumberPattern + ',?\s*\r?\n' +
            '\s*\)\),?\s*$'
    )
    if ($viewportMatches.Count -ne 1) {
        throw 'Selected QA report must contain exactly one structurally complete observed viewport.'
    }
    $physicalWidth = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'physical_width' `
        -Maximum $MaxQaScreenshotDimension
    $physicalHeight = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'physical_height' `
        -Maximum $MaxQaScreenshotDimension
    if ($physicalWidth -eq 0 -or $physicalHeight -eq 0 -or
        $physicalWidth * $physicalHeight -gt $MaxQaScreenshotPixels -or
        [uint64]$viewportMatches[0].Groups[1].Value -ne $physicalWidth -or
        [uint64]$viewportMatches[0].Groups[2].Value -ne $physicalHeight) {
        throw 'Selected QA report physical viewport is empty or outside the screenshot pixel bound.'
    }

    $observationCount = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'screenshot_observation_count' `
        -Maximum 600
    if ($observationCount -eq 0) {
        throw 'Selected QA report contains no screenshot pose observations.'
    }
    $durationSeconds = Get-QaUniqueFiniteFieldValue `
        -ReportText $ReportText `
        -FieldName 'requested_duration_seconds'
    if ($durationSeconds -lt 8.0 -or $durationSeconds -gt 600.0) {
        throw 'Selected QA report requested duration is outside the public 8..600 second contract.'
    }

    $legacyBlockMatches = [regex]::Matches(
        $ReportText,
        '(?ms)^\s*screenshots:\s*\[(.*?)^\s*\],?\s*\r?\n\s*screenshot_observation_cap:'
    )
    if ($legacyBlockMatches.Count -ne 1) {
        throw 'Selected QA report must contain exactly one bounded legacy screenshots vector.'
    }
    $ronStringPattern = '"((?:\\.|[^"\\])*)"'
    $legacyPaths = [regex]::Matches($legacyBlockMatches[0].Groups[1].Value, $ronStringPattern)
    $observationPaths = [regex]::Matches(
        $ReportText,
        '(?m)^\s*screenshot_path:\s*' + $ronStringPattern + ',?\s*$'
    )
    $captureIndices = [regex]::Matches(
        $ReportText,
        '(?m)^\s*capture_index:\s*([0-9]+),?\s*$'
    )
    $numberPattern = '[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?'
    $scheduledCaptures = [regex]::Matches(
        $ReportText,
        '(?m)^\s*scheduled_capture_seconds:\s*(' + $numberPattern + '),?\s*$'
    )
    $translations = [regex]::Matches(
        $ReportText,
        '(?ms)^\s*player_camera_translation_metres:\s*\(\s*(' + $numberPattern +
            ')\s*,\s*(' + $numberPattern + ')\s*,\s*(' + $numberPattern +
            ')\s*,?\s*\),?\s*$'
    )
    $rotations = [regex]::Matches(
        $ReportText,
        '(?ms)^\s*player_camera_rotation_xyzw:\s*\(\s*(' + $numberPattern +
            ')\s*,\s*(' + $numberPattern + ')\s*,\s*(' + $numberPattern +
            ')\s*,\s*(' + $numberPattern + ')\s*,?\s*\),?\s*$'
    )
    foreach ($fieldCount in @(
            $legacyPaths.Count,
            $observationPaths.Count,
            $captureIndices.Count,
            $scheduledCaptures.Count,
            $translations.Count,
            $rotations.Count
        )) {
        if ([uint64]$fieldCount -ne $observationCount) {
            throw 'Selected QA report screenshot vectors and pose observations have inconsistent counts.'
        }
    }

    $seenPaths = [System.Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    $previousScheduledCapture = [double]::NegativeInfinity
    for ($index = 0; $index -lt [int]$observationCount; $index++) {
        $captureIndex = [uint64]0
        if (-not [uint64]::TryParse(
                $captureIndices[$index].Groups[1].Value,
                [Globalization.NumberStyles]::None,
                [Globalization.CultureInfo]::InvariantCulture,
                [ref]$captureIndex) -or $captureIndex -ne [uint64]$index) {
            throw 'Selected QA report screenshot capture indices are not unique, contiguous, and in range.'
        }

        $legacyPath = $legacyPaths[$index].Groups[1].Value
        $observationPath = $observationPaths[$index].Groups[1].Value
        if (-not $legacyPath.Equals($observationPath, [StringComparison]::Ordinal)) {
            throw 'Selected QA report legacy screenshot path does not match its pose observation.'
        }
        $decodedPath = ConvertFrom-QaRonScreenshotPath -RawPath $observationPath
        if (-not $seenPaths.Add($decodedPath)) {
            throw 'Selected QA report screenshot path is duplicated.'
        }
        $pathSegments = @([regex]::Split($decodedPath, '[\\/]'))
        $screenshotName = $pathSegments[2]
        $expectedRelativePath = "qa_runs\$($reportContext.RunName)\$screenshotName"
        if (-not $decodedPath.Equals($expectedRelativePath, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Selected QA report screenshot path does not name a direct child of its selected run.'
        }
        $screenshotNameMatch = [regex]::Match(
            $screenshotName,
            '^shot_([0-9]{4})_(establishing|approach|detail|context)\.png$'
        )
        [uint64]$filenameCaptureIndex = 0
        if (-not $screenshotNameMatch.Success -or
            -not [uint64]::TryParse(
                $screenshotNameMatch.Groups[1].Value,
                [Globalization.NumberStyles]::None,
                [Globalization.CultureInfo]::InvariantCulture,
                [ref]$filenameCaptureIndex) -or
            $filenameCaptureIndex -ne $captureIndex) {
            throw 'Selected QA screenshot filename is not bound to its capture index and route phase.'
        }

        $resolvedPath = [System.IO.Path]::GetFullPath(
            (Join-Path $reportContext.EvidenceBase $decodedPath)
        )
        $expectedResolvedPath = [System.IO.Path]::GetFullPath(
            (Join-Path $reportContext.RunDirectory $screenshotName)
        )
        if (-not $resolvedPath.Equals($expectedResolvedPath, [StringComparison]::OrdinalIgnoreCase) -or
            -not [System.IO.Path]::GetDirectoryName($resolvedPath).Equals(
                $reportContext.RunDirectory,
                [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Selected QA screenshot did not resolve to the exact canonical direct-child path.'
        }
        $screenshotItem = Get-Item -LiteralPath $resolvedPath -Force -ErrorAction Stop
        if ($screenshotItem.PSIsContainer -or
            ($screenshotItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
            -not $screenshotItem.FullName.Equals($expectedResolvedPath, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Selected QA screenshot is not the expected direct non-reparse regular child.'
        }

        $scheduledCapture = [double]0
        if (-not [double]::TryParse(
                $scheduledCaptures[$index].Groups[1].Value,
                [Globalization.NumberStyles]::Float,
                [Globalization.CultureInfo]::InvariantCulture,
                [ref]$scheduledCapture) -or
            [double]::IsNaN($scheduledCapture) -or
            [double]::IsInfinity($scheduledCapture) -or
            $scheduledCapture -lt 0.0 -or
            $scheduledCapture -gt $durationSeconds + 0.001 -or
            $scheduledCapture -le $previousScheduledCapture) {
            throw 'Selected QA report screenshot schedule is non-finite, non-monotonic, or out of range.'
        }
        $previousScheduledCapture = $scheduledCapture

        foreach ($componentGroup in 1..3) {
            $component = [double]0
            if (-not [double]::TryParse(
                    $translations[$index].Groups[$componentGroup].Value,
                    [Globalization.NumberStyles]::Float,
                    [Globalization.CultureInfo]::InvariantCulture,
                    [ref]$component) -or
                [double]::IsNaN($component) -or
                [double]::IsInfinity($component) -or
                [Math]::Abs($component) -gt 16777216.0) {
                throw 'Selected QA report Player camera translation is non-finite or outside the safe f32 integer range.'
            }
        }

        $rotationNormSquared = 0.0
        foreach ($componentGroup in 1..4) {
            $component = [double]0
            if (-not [double]::TryParse(
                    $rotations[$index].Groups[$componentGroup].Value,
                    [Globalization.NumberStyles]::Float,
                    [Globalization.CultureInfo]::InvariantCulture,
                    [ref]$component) -or
                [double]::IsNaN($component) -or
                [double]::IsInfinity($component)) {
                throw 'Selected QA report Player camera rotation contains a non-finite component.'
            }
            $rotationNormSquared += $component * $component
        }
        if ($rotationNormSquared -lt 0.999 -or $rotationNormSquared -gt 1.001) {
            throw 'Selected QA report Player camera rotation is not a unit quaternion.'
        }
        Assert-QaPngEvidenceFile `
            -Path $expectedResolvedPath `
            -ExpectedWidth $physicalWidth `
            -ExpectedHeight $physicalHeight
    }
}

function Assert-QaReportTextIdentity {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ReportText,

        [Parameter(Mandatory = $true)]
        [string]$ReportPath,

        [Parameter(Mandatory = $true)]
        [string]$QaRunsRoot,

        [Parameter(Mandatory = $true)]
        [string]$WorldName,

        [Parameter(Mandatory = $true)]
        [string]$InstanceLabel,

        [Parameter(Mandatory = $true)]
        [string]$RouteProfile,

        [Parameter(Mandatory = $true)]
        [string]$RouteFocus,

        [Parameter(Mandatory = $true)]
        [uint32]$WorldSeed,

        [Parameter(Mandatory = $true)]
        [string]$SceneryMode,

        [Parameter(Mandatory = $true)]
        [string]$SurfaceMode,

        [Parameter(Mandatory = $true)]
        [string]$HydroMode,

        [Parameter(Mandatory = $true)]
        [string]$CohortMode,

        [Parameter(Mandatory = $true)]
        [string]$TerrainGrammarMode,

        [Parameter(Mandatory = $true)]
        [string]$L0HeightModeValue,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedBuildProfile,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedGitSha,

        [Parameter(Mandatory = $true)]
        [bool]$ExpectedGitDirty,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedSourceFingerprint,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedExecutableHash,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedToolchain,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedHardware
    )

    $expectedWorldProfile = if ($RouteProfile -eq 'natural') {
        'Natural'
    }
    else {
        'AstralFrontier'
    }
    $expectedSceneryMode = switch ($SceneryMode) {
        'off' { 'Off' }
        'lean' { 'Lean' }
        'balanced' { 'Balanced' }
        'lush' { 'Lush' }
        default { throw "Unsupported expected scenery mode '$SceneryMode'." }
    }
    $expectedSurfaceMode = switch ($SurfaceMode) {
        'legacy' { 'LegacyPalette' }
        'bridge-v1' { 'BridgeV1' }
        'bridge-v2' { 'BridgeV2' }
        'lod-provenance-v1' { 'LodProvenanceV1' }
        default { throw "Unsupported expected surface mode '$SurfaceMode'." }
    }
    $expectedHydroMode = switch ($HydroMode) {
        'off' { 'Disabled' }
        'v1' { 'DescriptiveV1' }
        default { throw "Unsupported expected Hydro mode '$HydroMode'." }
    }
    $expectedCohortMode = switch ($CohortMode) {
        'off' { 'Disabled' }
        'v1' { 'SilhouettesV1' }
        default { throw "Unsupported expected cohort mode '$CohortMode'." }
    }
    $expectedTerrainGrammar = switch ($TerrainGrammarMode) {
        'v1' { 'V1' }
        'v2' { 'V2' }
        'v3' { 'V3' }
        default { throw "Unsupported expected terrain grammar '$TerrainGrammarMode'." }
    }
    $expectedL0HeightModeLabel = switch ($L0HeightModeValue) {
        'point-16-v1' { 'Point16V1' }
        'cardinal-trimmed-8-v1' { 'CardinalTrimmed8V1' }
        default { throw "Unsupported expected L0 height mode '$L0HeightModeValue'." }
    }
    $diagnosticL0HeightMode = $L0HeightModeValue -eq 'cardinal-trimmed-8-v1'
    $diagnosticLodProvenance = $SurfaceMode -eq 'lod-provenance-v1'
    $expectedReportSchemaVersion = if ($diagnosticL0HeightMode -and $diagnosticLodProvenance) {
        '2.5.0-diagnostic-l0-cardinal-trimmed-8-v1-lod-provenance-v1'
    }
    elseif ($diagnosticL0HeightMode) {
        '2.5.0-diagnostic-l0-cardinal-trimmed-8-v1'
    }
    elseif ($diagnosticLodProvenance) {
        '2.5.0-diagnostic-lod-provenance-v1'
    }
    else {
        '2.5.0'
    }
    $expectedEvidenceDisposition = if ($diagnosticL0HeightMode -and $diagnosticLodProvenance) {
        'diagnostic-l0-height-and-lod-provenance-only-non-publishable'
    }
    elseif ($diagnosticL0HeightMode) {
        'diagnostic-only-non-publishable'
    }
    elseif ($diagnosticLodProvenance) {
        'diagnostic-lod-provenance-only-non-publishable'
    }
    else {
        'canonical-candidate'
    }
    $expectedGitDirtyText = if ($ExpectedGitDirty) { 'true' } else { 'false' }
    $ronWorldName = [regex]::Escape((ConvertTo-RonStringContent -Value $WorldName))
    $ronInstanceLabel = [regex]::Escape((ConvertTo-RonStringContent -Value $InstanceLabel))
    $ronBuildProfile = [regex]::Escape((ConvertTo-RonStringContent -Value $ExpectedBuildProfile))
    $ronGitSha = [regex]::Escape((ConvertTo-RonStringContent -Value $ExpectedGitSha))
    $ronSourceFingerprint = [regex]::Escape((ConvertTo-RonStringContent -Value $ExpectedSourceFingerprint))
    $ronExecutableHash = [regex]::Escape((ConvertTo-RonStringContent -Value $ExpectedExecutableHash))
    $ronToolchain = [regex]::Escape((ConvertTo-RonStringContent -Value $ExpectedToolchain))
    $ronHardware = [regex]::Escape((ConvertTo-RonStringContent -Value $ExpectedHardware))

    $identityPatterns = [ordered]@{
        qa_report_schema_version = '(?m)^\s*qa_report_schema_version:\s*"' + [regex]::Escape($expectedReportSchemaVersion) + '",?\s*$'
        evidence_disposition = '(?m)^\s*evidence_disposition:\s*"' + [regex]::Escape($expectedEvidenceDisposition) + '",?\s*$'
        build_profile = '(?m)^\s*build_profile:\s*"' + $ronBuildProfile + '",?\s*$'
        world_name = '(?m)^\s*world_name:\s*Some\("' + $ronWorldName + '"\),?\s*$'
        instance_label = '(?m)^\s*instance_label:\s*Some\("' + $ronInstanceLabel + '"\),?\s*$'
        world_seed = '(?m)^\s*world_seed:\s*Some\(' + [regex]::Escape([string]$WorldSeed) + '\),?\s*$'
        world_profile = '(?m)^\s*world_profile:\s*Some\("' + [regex]::Escape($expectedWorldProfile) + '"\),?\s*$'
        scenery_quality = '(?m)^\s*scenery_quality:\s*Some\("' + [regex]::Escape($expectedSceneryMode) + '"\),?\s*$'
        terrain_grammar = '(?m)^\s*terrain_grammar:\s*Some\("' + [regex]::Escape($expectedTerrainGrammar) + '"\),?\s*$'
        git_sha = '(?m)^\s*git_sha:\s*Some\("' + $ronGitSha + '"\),?\s*$'
        git_dirty = '(?m)^\s*git_dirty:\s*Some\(' + $expectedGitDirtyText + '\),?\s*$'
        source_fingerprint = '(?m)^\s*source_fingerprint:\s*Some\("' + $ronSourceFingerprint + '"\),?\s*$'
        executable_hash = '(?m)^\s*executable_hash:\s*Some\("' + $ronExecutableHash + '"\),?\s*$'
        toolchain = '(?m)^\s*toolchain:\s*Some\("' + $ronToolchain + '"\),?\s*$'
        hardware = '(?m)^\s*hardware:\s*Some\("' + $ronHardware + '"\),?\s*$'
        world_edit_store_status = '(?m)^\s*world_edit_store_status:\s*"compatible",?\s*$'
        world_edit_store_compatible = '(?m)^\s*world_edit_store_compatible:\s*true,?\s*$'
        world_edit_store_seed = '(?m)^\s*world_edit_store_seed:\s*Some\(' + [regex]::Escape([string]$WorldSeed) + '\),?\s*$'
        world_edit_store_profile = '(?m)^\s*world_edit_store_profile:\s*Some\("' + [regex]::Escape($expectedWorldProfile) + '"\),?\s*$'
        world_edit_store_scenery_quality = '(?m)^\s*world_edit_store_scenery_quality:\s*Some\("' + [regex]::Escape($expectedSceneryMode) + '"\),?\s*$'
        world_edit_store_terrain_grammar = '(?m)^\s*world_edit_store_terrain_grammar:\s*Some\("' + [regex]::Escape($expectedTerrainGrammar) + '"\),?\s*$'
        world_edit_store_edited_chunks = '(?m)^\s*world_edit_store_edited_chunks:\s*Some\([0-9]+\),?\s*$'
        world_edit_store_block_reason_code = '(?m)^\s*world_edit_store_block_reason_code:\s*None,?\s*$'
        planetary_profile = '(?m)^\s*profile:\s*"' + [regex]::Escape($expectedWorldProfile) + '",?\s*$'
        desired_terrain_grammar = '(?m)^\s*desired_terrain_grammar:\s*Some\("' + [regex]::Escape($expectedTerrainGrammar) + '"\),?\s*$'
        active_terrain_grammar = '(?m)^\s*active_terrain_grammar:\s*Some\("' + [regex]::Escape($expectedTerrainGrammar) + '"\),?\s*$'
        desired_l0_height_mode = '(?m)^\s*desired_l0_height_mode:\s*"' + [regex]::Escape($expectedL0HeightModeLabel) + '",?\s*$'
        active_l0_height_mode = '(?m)^\s*active_l0_height_mode:\s*Some\("' + [regex]::Escape($expectedL0HeightModeLabel) + '"\),?\s*$'
        resident_l0_height_mode = '(?m)^\s*resident_l0_height_mode:\s*Some\("' + [regex]::Escape($expectedL0HeightModeLabel) + '"\),?\s*$'
        l0_probe_spacing_metres = '(?m)^\s*l0_probe_spacing_metres:\s*8,?\s*$'
        budget_l0_height_queries = '(?m)^\s*budget_l0_height_queries:\s*12805,?\s*$'
        surface_material_mode = '(?m)^\s*surface_material_mode:\s*"' + [regex]::Escape($expectedSurfaceMode) + '",?\s*$'
        hydro_mode = '(?m)^\s*hydro_mode:\s*"' + [regex]::Escape($expectedHydroMode) + '",?\s*$'
        semantic_cohort_mode = '(?m)^\s*semantic_cohort_mode:\s*"' + [regex]::Escape($expectedCohortMode) + '",?\s*$'
        requested_route_focus = '(?m)^\s*requested_route_focus:\s*"' + [regex]::Escape($RouteFocus) + '",?\s*$'
        resolved_route_focus = '(?m)^\s*resolved_route_focus:\s*"' + [regex]::Escape($RouteFocus) + '",?\s*$'
        route_focus_available = '(?m)^\s*route_focus_available:\s*true,?\s*$'
        route_focus_unavailable_reason = '(?m)^\s*route_focus_unavailable_reason:\s*None,?\s*$'
        route_focus_search_cap_exhausted = '(?m)^\s*route_focus_search_cap_exhausted:\s*false,?\s*$'
        camera_route_policy = '(?m)^\s*camera_route_policy:\s*"preflight-v1",?\s*$'
        resident_observation_valid = '(?m)^\s*resident_observation_valid:\s*true,?\s*$'
        resident_entity_count_overflow = '(?m)^\s*resident_entity_count_overflow:\s*false,?\s*$'
        resident_duplicate_levels = '(?m)^\s*resident_duplicate_levels:\s*0,?\s*$'
        resident_out_of_range_levels = '(?m)^\s*resident_out_of_range_levels:\s*0,?\s*$'
        resident_scheduler_mismatch = '(?m)^\s*resident_scheduler_mismatch:\s*false,?\s*$'
        resident_budget_exceeded = '(?m)^\s*resident_budget_exceeded:\s*false,?\s*$'
        resident_observation_rejections = '(?m)^\s*resident_observation_rejections:\s*0,?\s*$'
        resident_fluid_observation_valid = '(?m)^\s*resident_fluid_observation_valid:\s*true,?\s*$'
        resident_fluid_entity_count_overflow = '(?m)^\s*resident_fluid_entity_count_overflow:\s*false,?\s*$'
        resident_fluid_duplicate_slots = '(?m)^\s*resident_fluid_duplicate_slots:\s*0,?\s*$'
        resident_fluid_out_of_range_levels = '(?m)^\s*resident_fluid_out_of_range_levels:\s*0,?\s*$'
        resident_fluid_scheduler_mismatch = '(?m)^\s*resident_fluid_scheduler_mismatch:\s*false,?\s*$'
        resident_fluid_budget_exceeded = '(?m)^\s*resident_fluid_budget_exceeded:\s*false,?\s*$'
        resident_fluid_kind_integrity_valid = '(?m)^\s*resident_fluid_kind_integrity_valid:\s*true,?\s*$'
        resident_fluid_observation_rejections = '(?m)^\s*resident_fluid_observation_rejections:\s*0,?\s*$'
        resident_semantic_cohort_observation_valid = '(?m)^\s*resident_semantic_cohort_observation_valid:\s*true,?\s*$'
        resident_semantic_cohort_entity_count_overflow = '(?m)^\s*resident_semantic_cohort_entity_count_overflow:\s*false,?\s*$'
        resident_semantic_cohort_scheduler_mismatch = '(?m)^\s*resident_semantic_cohort_scheduler_mismatch:\s*false,?\s*$'
        resident_semantic_cohort_budget_exceeded = '(?m)^\s*resident_semantic_cohort_budget_exceeded:\s*false,?\s*$'
        resident_semantic_cohort_payload_integrity_valid = '(?m)^\s*resident_semantic_cohort_payload_integrity_valid:\s*true,?\s*$'
        resident_semantic_cohort_observation_rejections = '(?m)^\s*resident_semantic_cohort_observation_rejections:\s*0,?\s*$'
        pending_rebuilds = '(?m)^\s*pending_rebuilds:\s*0,?\s*$'
        dirty_mask = '(?m)^\s*dirty_mask:\s*0,?\s*$'
        build_in_flight = '(?m)^\s*build_in_flight:\s*false,?\s*$'
        near_coverage_transition_pending = '(?m)^\s*near_coverage_transition_pending:\s*false,?\s*$'
        frontier_complete = '(?m)^\s*frontier_complete:\s*true,?\s*$'
        dense_chunk_budget_exceeded = '(?m)^\s*dense_chunk_budget_exceeded:\s*false,?\s*$'
        budget_rejections = '(?m)^\s*budget_rejections:\s*0,?\s*$'
        screenshot_observation_cap = '(?m)^\s*screenshot_observation_cap:\s*600,?\s*$'
        screenshot_path_max_chars = '(?m)^\s*screenshot_path_max_chars:\s*512,?\s*$'
        screenshot_observation_valid = '(?m)^\s*screenshot_observation_valid:\s*true,?\s*$'
        screenshot_observation_cap_exhausted = '(?m)^\s*screenshot_observation_cap_exhausted:\s*false,?\s*$'
        screenshot_observation_rejections = '(?m)^\s*screenshot_observation_rejections:\s*0,?\s*$'
    }
    $identityFieldNames = [ordered]@{
        qa_report_schema_version = 'qa_report_schema_version'
        evidence_disposition = 'evidence_disposition'
        build_profile = 'build_profile'
        world_name = 'world_name'
        instance_label = 'instance_label'
        world_seed = 'world_seed'
        world_profile = 'world_profile'
        scenery_quality = 'scenery_quality'
        terrain_grammar = 'terrain_grammar'
        git_sha = 'git_sha'
        git_dirty = 'git_dirty'
        source_fingerprint = 'source_fingerprint'
        executable_hash = 'executable_hash'
        toolchain = 'toolchain'
        hardware = 'hardware'
        world_edit_store_status = 'world_edit_store_status'
        world_edit_store_compatible = 'world_edit_store_compatible'
        world_edit_store_seed = 'world_edit_store_seed'
        world_edit_store_profile = 'world_edit_store_profile'
        world_edit_store_scenery_quality = 'world_edit_store_scenery_quality'
        world_edit_store_terrain_grammar = 'world_edit_store_terrain_grammar'
        world_edit_store_edited_chunks = 'world_edit_store_edited_chunks'
        world_edit_store_block_reason_code = 'world_edit_store_block_reason_code'
        planetary_profile = 'profile'
        desired_terrain_grammar = 'desired_terrain_grammar'
        active_terrain_grammar = 'active_terrain_grammar'
        desired_l0_height_mode = 'desired_l0_height_mode'
        active_l0_height_mode = 'active_l0_height_mode'
        resident_l0_height_mode = 'resident_l0_height_mode'
        l0_probe_spacing_metres = 'l0_probe_spacing_metres'
        budget_l0_height_queries = 'budget_l0_height_queries'
        surface_material_mode = 'surface_material_mode'
        hydro_mode = 'hydro_mode'
        semantic_cohort_mode = 'semantic_cohort_mode'
        requested_route_focus = 'requested_route_focus'
        resolved_route_focus = 'resolved_route_focus'
        route_focus_available = 'route_focus_available'
        route_focus_unavailable_reason = 'route_focus_unavailable_reason'
        route_focus_search_cap_exhausted = 'route_focus_search_cap_exhausted'
        camera_route_policy = 'camera_route_policy'
        resident_observation_valid = 'resident_observation_valid'
        resident_entity_count_overflow = 'resident_entity_count_overflow'
        resident_duplicate_levels = 'resident_duplicate_levels'
        resident_out_of_range_levels = 'resident_out_of_range_levels'
        resident_scheduler_mismatch = 'resident_scheduler_mismatch'
        resident_budget_exceeded = 'resident_budget_exceeded'
        resident_observation_rejections = 'resident_observation_rejections'
        resident_fluid_observation_valid = 'resident_fluid_observation_valid'
        resident_fluid_entity_count_overflow = 'resident_fluid_entity_count_overflow'
        resident_fluid_duplicate_slots = 'resident_fluid_duplicate_slots'
        resident_fluid_out_of_range_levels = 'resident_fluid_out_of_range_levels'
        resident_fluid_scheduler_mismatch = 'resident_fluid_scheduler_mismatch'
        resident_fluid_budget_exceeded = 'resident_fluid_budget_exceeded'
        resident_fluid_kind_integrity_valid = 'resident_fluid_kind_integrity_valid'
        resident_fluid_observation_rejections = 'resident_fluid_observation_rejections'
        resident_semantic_cohort_observation_valid = 'resident_semantic_cohort_observation_valid'
        resident_semantic_cohort_entity_count_overflow = 'resident_semantic_cohort_entity_count_overflow'
        resident_semantic_cohort_scheduler_mismatch = 'resident_semantic_cohort_scheduler_mismatch'
        resident_semantic_cohort_budget_exceeded = 'resident_semantic_cohort_budget_exceeded'
        resident_semantic_cohort_payload_integrity_valid = 'resident_semantic_cohort_payload_integrity_valid'
        resident_semantic_cohort_observation_rejections = 'resident_semantic_cohort_observation_rejections'
        pending_rebuilds = 'pending_rebuilds'
        dirty_mask = 'dirty_mask'
        build_in_flight = 'build_in_flight'
        near_coverage_transition_pending = 'near_coverage_transition_pending'
        frontier_complete = 'frontier_complete'
        dense_chunk_budget_exceeded = 'dense_chunk_budget_exceeded'
        budget_rejections = 'budget_rejections'
        screenshot_observation_cap = 'screenshot_observation_cap'
        screenshot_path_max_chars = 'screenshot_path_max_chars'
        screenshot_observation_valid = 'screenshot_observation_valid'
        screenshot_observation_cap_exhausted = 'screenshot_observation_cap_exhausted'
        screenshot_observation_rejections = 'screenshot_observation_rejections'
    }
    foreach ($identityField in $identityPatterns.Keys) {
        $fieldPattern = '(?m)^\s*' + [regex]::Escape($identityFieldNames[$identityField]) + ':\s*'
        $fieldMatches = [regex]::Matches($ReportText, $fieldPattern)
        if ($fieldMatches.Count -ne 1) {
            throw "Selected QA report contains $($fieldMatches.Count) $($identityFieldNames[$identityField]) field declarations; exactly one is required."
        }
        $matches = [regex]::Matches($ReportText, $identityPatterns[$identityField])
        if ($matches.Count -ne 1) {
            throw "Selected QA report contains $($matches.Count) exact expected $identityField fields; exactly one is required."
        }
    }

    # QaReport serializes these top-level fields consecutively. The nested
    # near_coverage_transition_pending signal is validated independently in
    # identityPatterns above; it belongs to QaPlanetaryStreaming and must not
    # be inserted into this top-level block. Binding frontier_complete between
    # dirty_chunks and render_distance detects report-layout drift explicitly.
    $settledNearFieldPattern = '(?ms)^\s*loaded_chunks:\s*[0-9]+,?\s*\r?\n' +
        '\s*mesh_entities:\s*[0-9]+,?\s*\r?\n' +
        '\s*pending_terrain:\s*0,?\s*\r?\n' +
        '\s*pending_meshes:\s*0,?\s*\r?\n' +
        '\s*dirty_chunks:\s*0,?\s*\r?\n' +
        '\s*dense_chunks:\s*[0-9]+,?\s*\r?\n' +
        '\s*dense_chunk_budget:\s*2400,?\s*\r?\n' +
        '\s*dense_chunk_budget_exceeded:\s*false,?\s*\r?\n' +
        '\s*frontier_complete:\s*true,?\s*\r?\n' +
        '\s*render_distance:'
    if ([regex]::Matches($ReportText, $settledNearFieldPattern).Count -ne 1) {
        throw 'Selected QA report did not finish with settled near-field queues and a complete top-level frontier.'
    }
    $loadedChunks = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'loaded_chunks' `
        -Maximum 2400
    $pendingTerrain = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'pending_terrain' `
        -Maximum 2400
    $denseChunks = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'dense_chunks' `
        -Maximum 2400
    $denseChunkBudget = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'dense_chunk_budget' `
        -Maximum 2400
    $peakLoadedChunks = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'peak_loaded_chunks' `
        -Maximum 2400
    $peakPendingTerrain = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'peak_pending_terrain' `
        -Maximum 2400
    $peakDenseChunks = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'peak_dense_chunks' `
        -Maximum 2400
    if ($denseChunkBudget -ne 2400) {
        throw 'Selected QA report does not declare the exact 2400-chunk dense residency budget.'
    }
    if ($denseChunks -ne ($loadedChunks + $pendingTerrain)) {
        throw 'Selected QA report dense_chunks does not equal loaded_chunks plus pending_terrain.'
    }
    if ($peakDenseChunks -lt $denseChunks -or
        $peakDenseChunks -lt $peakLoadedChunks -or
        $peakDenseChunks -lt $peakPendingTerrain) {
        throw 'Selected QA report peak_dense_chunks is inconsistent with its component observations.'
    }
    if ($diagnosticLodProvenance -and ($HydroMode -ne 'off' -or $CohortMode -ne 'off')) {
        throw 'LOD-provenance report acceptance requires Hydro and semantic cohorts to be disabled.'
    }

    $expectedCacheBytes = if ($diagnosticL0HeightMode) { [uint64]263142 } else { [uint64]228822 }
    foreach ($cacheWindowField in @('live_sample_cache_windows', 'peak_live_sample_cache_windows')) {
        $cacheWindows = Get-QaUniqueUnsignedFieldValue `
            -ReportText $ReportText `
            -FieldName $cacheWindowField `
            -Maximum 6
        if ($cacheWindows -ne 6) {
            throw "Selected QA report $cacheWindowField is not the exact six-ring installed cache population."
        }
    }
    foreach ($cacheByteField in @('live_sample_cache_bytes', 'peak_live_sample_cache_bytes')) {
        $cacheBytes = Get-QaUniqueUnsignedFieldValue `
            -ReportText $ReportText `
            -FieldName $cacheByteField `
            -Maximum $expectedCacheBytes
        if ($cacheBytes -ne $expectedCacheBytes) {
            throw "Selected QA report $cacheByteField is not the exact $expectedCacheBytes-byte cache identity for $expectedL0HeightModeLabel."
        }
    }
    $sampleCacheBudget = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'budget_sample_cache_bytes' `
        -Maximum 524288
    if ($sampleCacheBudget -ne 524288) {
        throw 'Selected QA report sample-cache budget is not the canonical 524288-byte ceiling.'
    }

    Assert-QaScreenshotObservations `
        -ReportText $ReportText `
        -ReportPath $ReportPath `
        -QaRunsRoot $QaRunsRoot

    $l0QueryCaps = [ordered]@{
        last_l0_center_queries = [uint64]4225
        last_l0_half_x_queries = [uint64]4290
        last_l0_half_z_queries = [uint64]4290
    }
    $l0QueryCounters = [ordered]@{}
    foreach ($field in $l0QueryCaps.Keys) {
        $matches = [regex]::Matches(
            $ReportText,
            '(?m)^\s*' + [regex]::Escape($field) + ':\s*([0-9]+),?\s*$'
        )
        if ($matches.Count -ne 1) {
            throw "Selected QA report contains $($matches.Count) $field field declarations; exactly one bounded integer is required."
        }
        $value = [uint64]0
        if (-not [uint64]::TryParse(
                $matches[0].Groups[1].Value,
                [Globalization.NumberStyles]::None,
                [Globalization.CultureInfo]::InvariantCulture,
                [ref]$value) -or $value -gt $l0QueryCaps[$field]) {
            throw "Selected QA report $field exceeds its mode-independent $($l0QueryCaps[$field])-query channel cap."
        }
        $l0QueryCounters[$field] = $value
    }
    $l0HeightQueryTotal = $l0QueryCounters.last_l0_center_queries +
        $l0QueryCounters.last_l0_half_x_queries +
        $l0QueryCounters.last_l0_half_z_queries
    if ($l0HeightQueryTotal -gt 12805) {
        throw 'Selected QA report L0 height-query counters exceed their shared 12805-query budget.'
    }
    if (-not $diagnosticL0HeightMode -and
        ($l0QueryCounters.last_l0_half_x_queries -ne 0 -or
            $l0QueryCounters.last_l0_half_z_queries -ne 0)) {
        throw 'Point16V1 evidence must not contain half-step height queries.'
    }
    if ($diagnosticL0HeightMode) {
        $zeroCandidateQueryPlanes = @($l0QueryCounters.Values | Where-Object { $_ -eq 0 }).Count
        if ($zeroCandidateQueryPlanes -ne 0 -and $zeroCandidateQueryPlanes -ne 3) {
            throw 'CardinalTrimmed8V1 query channels must be either all zero for a fully reused L0 or all nonzero for an incremental/cold install.'
        }
    }
    $l0CacheUpdateMatches = [regex]::Matches(
        $ReportText,
        '(?m)^\s*last_l0_cache_update:\s*"(Cold|IncrementalStrip|TeleportFallback|IncompatibleFallback)",?\s*$'
    )
    if ($l0CacheUpdateMatches.Count -ne 1) {
        throw 'Selected QA report must contain exactly one supported last_l0_cache_update field.'
    }
    $lastL0CacheShiftX = Get-QaUniqueSignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'last_l0_cache_shift_x_cells' `
        -Minimum ([int32]::MinValue) `
        -Maximum ([int32]::MaxValue)
    $lastL0CacheShiftZ = Get-QaUniqueSignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'last_l0_cache_shift_z_cells' `
        -Minimum ([int32]::MinValue) `
        -Maximum ([int32]::MaxValue)
    $lastL0ReusedHeightSamples = Get-QaUniqueUnsignedFieldValue `
        -ReportText $ReportText `
        -FieldName 'last_l0_reused_height_samples' `
        -Maximum 12805
    Assert-QaL0SamplingIdentity `
        -L0HeightModeLabel $expectedL0HeightModeLabel `
        -CacheUpdate $l0CacheUpdateMatches[0].Groups[1].Value `
        -ShiftX $lastL0CacheShiftX `
        -ShiftZ $lastL0CacheShiftZ `
        -CenterQueries $l0QueryCounters.last_l0_center_queries `
        -HalfXQueries $l0QueryCounters.last_l0_half_x_queries `
        -HalfZQueries $l0QueryCounters.last_l0_half_z_queries `
        -ReusedHeightSamples $lastL0ReusedHeightSamples

    $l0EffectCounters = [ordered]@{}
    foreach ($field in @(
            'last_l0_trimmed_vertices',
            'last_l0_trimmed_up_vertices',
            'last_l0_trimmed_down_vertices'
        )) {
        $matches = [regex]::Matches(
            $ReportText,
            '(?m)^\s*' + [regex]::Escape($field) + ':\s*([0-9]+),?\s*$'
        )
        if ($matches.Count -ne 1) {
            throw "Selected QA report contains $($matches.Count) $field field declarations; exactly one bounded integer is required."
        }
        $value = [uint64]0
        if (-not [uint64]::TryParse(
                $matches[0].Groups[1].Value,
                [Globalization.NumberStyles]::None,
                [Globalization.CultureInfo]::InvariantCulture,
                [ref]$value) -or $value -gt 3721) {
            throw "Selected QA report $field exceeds the 3721-vertex L0 lattice bound."
        }
        $l0EffectCounters[$field] = $value
    }
    if (($l0EffectCounters.last_l0_trimmed_up_vertices +
            $l0EffectCounters.last_l0_trimmed_down_vertices) -ne
        $l0EffectCounters.last_l0_trimmed_vertices) {
        throw 'Selected QA report L0 trimmed-vertex accounting is inconsistent.'
    }

    $adjustmentMatches = [regex]::Matches(
        $ReportText,
        '(?m)^\s*last_l0_max_abs_adjustment_metres:\s*([0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?),?\s*$'
    )
    if ($adjustmentMatches.Count -ne 1) {
        throw 'Selected QA report must contain exactly one finite nonnegative last_l0_max_abs_adjustment_metres value.'
    }
    $maxL0AdjustmentMetres = [double]0
    if (-not [double]::TryParse(
            $adjustmentMatches[0].Groups[1].Value,
            [Globalization.NumberStyles]::Float,
            [Globalization.CultureInfo]::InvariantCulture,
            [ref]$maxL0AdjustmentMetres) -or
        [double]::IsNaN($maxL0AdjustmentMetres) -or
        [double]::IsInfinity($maxL0AdjustmentMetres) -or
        $maxL0AdjustmentMetres -lt 0.0) {
        throw 'Selected QA report contains an invalid last_l0_max_abs_adjustment_metres value.'
    }
    if (($l0EffectCounters.last_l0_trimmed_vertices -eq 0) -ne
        ($maxL0AdjustmentMetres -eq 0.0)) {
        throw 'Selected QA report L0 adjustment magnitude is inconsistent with its trimmed-vertex count.'
    }
    if (-not $diagnosticL0HeightMode -and
        ($l0EffectCounters.last_l0_trimmed_vertices -ne 0 -or
            $maxL0AdjustmentMetres -ne 0.0)) {
        throw 'Point16V1 evidence must have zero L0 height adjustments.'
    }

    $preflightApplicable = $RouteFocus -in @('river', 'lava', 'near-far')
    $cameraPatterns = if ($preflightApplicable) {
        [ordered]@{
            camera_route_preflight_applicable = '(?m)^\s*camera_route_preflight_applicable:\s*true,?\s*$'
            camera_route_plan_hash = '(?m)^\s*camera_route_plan_hash:\s*Some\("[0-9a-f]{16}"\),?\s*$'
            camera_route_available = '(?m)^\s*camera_route_available:\s*true,?\s*$'
            camera_route_unavailable_reason = '(?m)^\s*camera_route_unavailable_reason:\s*None,?\s*$'
            camera_route_variant_index = '(?m)^\s*camera_route_variant_index:\s*Some\([0-7]\),?\s*$'
            camera_route_variant_count = '(?m)^\s*camera_route_variant_count:\s*8,?\s*$'
            camera_route_validation_samples = '(?m)^\s*camera_route_validation_samples:\s*16,?\s*$'
            camera_route_voxel_query_cap = '(?m)^\s*camera_route_voxel_query_cap:\s*153600,?\s*$'
            camera_route_unloaded_chunk_checks = '(?m)^\s*camera_route_unloaded_chunk_checks:\s*0,?\s*$'
            camera_route_selected_clear_samples = '(?m)^\s*camera_route_selected_clear_samples:\s*16,?\s*$'
            camera_route_minimum_clearance_voxels = '(?m)^\s*camera_route_minimum_clearance_voxels:\s*Some\(([1-9][0-9]*)\),?\s*$'
            camera_route_work_cap_exhausted = '(?m)^\s*camera_route_work_cap_exhausted:\s*false,?\s*$'
        }
    }
    else {
        [ordered]@{
            camera_route_preflight_applicable = '(?m)^\s*camera_route_preflight_applicable:\s*false,?\s*$'
            camera_route_plan_hash = '(?m)^\s*camera_route_plan_hash:\s*None,?\s*$'
            camera_route_available = '(?m)^\s*camera_route_available:\s*false,?\s*$'
            camera_route_unavailable_reason = '(?m)^\s*camera_route_unavailable_reason:\s*None,?\s*$'
            camera_route_variant_index = '(?m)^\s*camera_route_variant_index:\s*None,?\s*$'
            camera_route_variant_count = '(?m)^\s*camera_route_variant_count:\s*0,?\s*$'
            camera_route_validation_samples = '(?m)^\s*camera_route_validation_samples:\s*0,?\s*$'
            camera_route_voxel_queries = '(?m)^\s*camera_route_voxel_queries:\s*0,?\s*$'
            camera_route_voxel_query_cap = '(?m)^\s*camera_route_voxel_query_cap:\s*0,?\s*$'
            camera_route_required_chunk_checks = '(?m)^\s*camera_route_required_chunk_checks:\s*0,?\s*$'
            camera_route_loaded_chunk_checks = '(?m)^\s*camera_route_loaded_chunk_checks:\s*0,?\s*$'
            camera_route_proven_air_chunk_checks = '(?m)^\s*camera_route_proven_air_chunk_checks:\s*0,?\s*$'
            camera_route_unloaded_chunk_checks = '(?m)^\s*camera_route_unloaded_chunk_checks:\s*0,?\s*$'
            camera_route_candidate_body_occlusions = '(?m)^\s*camera_route_candidate_body_occlusions:\s*0,?\s*$'
            camera_route_candidate_los_occlusions = '(?m)^\s*camera_route_candidate_los_occlusions:\s*0,?\s*$'
            camera_route_selected_clear_samples = '(?m)^\s*camera_route_selected_clear_samples:\s*0,?\s*$'
            camera_route_minimum_clearance_voxels = '(?m)^\s*camera_route_minimum_clearance_voxels:\s*None,?\s*$'
            camera_route_work_cap_exhausted = '(?m)^\s*camera_route_work_cap_exhausted:\s*false,?\s*$'
        }
    }
    foreach ($cameraField in $cameraPatterns.Keys) {
        $fieldMatches = [regex]::Matches(
            $ReportText,
            '(?m)^\s*' + [regex]::Escape($cameraField) + ':\s*'
        )
        if ($fieldMatches.Count -ne 1 -or
            [regex]::Matches($ReportText, $cameraPatterns[$cameraField]).Count -ne 1) {
            throw "Selected QA report does not contain exactly one expected $cameraField value."
        }
    }

    if ($preflightApplicable) {
        foreach ($diagnosticField in @(
            'camera_route_candidate_body_occlusions',
            'camera_route_candidate_los_occlusions'
        )) {
            $matches = [regex]::Matches(
                $ReportText,
                '(?m)^\s*' + [regex]::Escape($diagnosticField) + ':\s*([0-9]+),?\s*$'
            )
            if ($matches.Count -ne 1) {
                throw "Selected QA report contains $($matches.Count) $diagnosticField field declarations; exactly one bounded integer is required."
            }
            $diagnosticValue = [uint64]0
            if (-not [uint64]::TryParse(
                    $matches[0].Groups[1].Value,
                    [Globalization.NumberStyles]::None,
                    [Globalization.CultureInfo]::InvariantCulture,
                    [ref]$diagnosticValue) -or $diagnosticValue -gt 128) {
                throw "Selected QA report $diagnosticField exceeds the 128-pose preflight bound."
            }
        }

        $cameraCounters = [ordered]@{}
        foreach ($field in @(
                'camera_route_voxel_queries',
                'camera_route_required_chunk_checks',
                'camera_route_loaded_chunk_checks',
                'camera_route_proven_air_chunk_checks',
                'camera_route_unloaded_chunk_checks'
            )) {
            $matches = [regex]::Matches(
                $ReportText,
                '(?m)^\s*' + [regex]::Escape($field) + ':\s*([0-9]+),?\s*$'
            )
            if ($matches.Count -ne 1) {
                throw "Selected QA report contains $($matches.Count) $field field declarations; exactly one bounded integer is required."
            }
            $value = [uint64]0
            if (-not [uint64]::TryParse(
                    $matches[0].Groups[1].Value,
                    [Globalization.NumberStyles]::None,
                    [Globalization.CultureInfo]::InvariantCulture,
                    [ref]$value)) {
                throw "Selected QA report contains an invalid $field value."
            }
            $cameraCounters[$field] = $value
        }
        if ($cameraCounters.camera_route_voxel_queries -eq 0 -or
            $cameraCounters.camera_route_voxel_queries -ge 153600) {
            throw 'Selected QA report camera-route work must be positive and strictly below the preflight-v1 cap.'
        }
        if ($cameraCounters.camera_route_required_chunk_checks -ne $cameraCounters.camera_route_voxel_queries -or
            $cameraCounters.camera_route_loaded_chunk_checks -gt $cameraCounters.camera_route_required_chunk_checks) {
            throw 'Selected QA report camera-route query/required/loaded/proven-air/unloaded chunk accounting is inconsistent.'
        }
        $remainingCameraChecks = $cameraCounters.camera_route_required_chunk_checks -
            $cameraCounters.camera_route_loaded_chunk_checks
        if ($cameraCounters.camera_route_proven_air_chunk_checks -gt $remainingCameraChecks) {
            throw 'Selected QA report camera-route query/required/loaded/proven-air/unloaded chunk accounting is inconsistent.'
        }
        $remainingCameraChecks -= $cameraCounters.camera_route_proven_air_chunk_checks
        if ($cameraCounters.camera_route_unloaded_chunk_checks -ne $remainingCameraChecks) {
            throw 'Selected QA report camera-route query/required/loaded/proven-air/unloaded chunk accounting is inconsistent.'
        }
    }
}

function Assert-QaReportIdentity {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ReportPath,

        [Parameter(Mandatory = $true)]
        [string]$QaRunsRoot,

        [Parameter(Mandatory = $true)]
        [string]$WorldName,

        [Parameter(Mandatory = $true)]
        [string]$InstanceLabel,

        [Parameter(Mandatory = $true)]
        [string]$RouteProfile,

        [Parameter(Mandatory = $true)]
        [string]$RouteFocus,

        [Parameter(Mandatory = $true)]
        [uint32]$WorldSeed,

        [Parameter(Mandatory = $true)]
        [string]$SceneryMode,

        [Parameter(Mandatory = $true)]
        [string]$SurfaceMode,

        [Parameter(Mandatory = $true)]
        [string]$HydroMode,

        [Parameter(Mandatory = $true)]
        [string]$CohortMode,

        [Parameter(Mandatory = $true)]
        [string]$TerrainGrammarMode,

        [Parameter(Mandatory = $true)]
        [string]$L0HeightModeValue,

        [Parameter(Mandatory = $true)]
        $ExpectedProvenance,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedBuildProfile
    )

    $reportContext = Get-QaSelectedReportContext `
        -ReportPath $ReportPath `
        -QaRunsRoot $QaRunsRoot
    $reportItem = Get-Item -LiteralPath $reportContext.ReportPath -Force -ErrorAction Stop
    $initialReportLength = [uint64]$reportItem.Length
    $initialReportWriteTimeUtcTicks = [int64]$reportItem.LastWriteTimeUtc.Ticks
    if ($initialReportLength -eq 0 -or $initialReportLength -gt $MaxQaReportBytes) {
        throw "Selected QA report exceeds the $MaxQaReportBytes-byte bound."
    }
    $initialReportDigest = Get-BoundedFileSha256 `
        -Path $reportContext.ReportPath `
        -MaxBytes $MaxQaReportBytes `
        -Label 'Selected QA report'
    $reportText = [System.IO.File]::ReadAllText($reportItem.FullName)
    $finalReportItem = Get-Item -LiteralPath $reportContext.ReportPath -Force -ErrorAction Stop
    if ($finalReportItem.PSIsContainer -or
        ($finalReportItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        [uint64]$finalReportItem.Length -ne $initialReportLength -or
        [int64]$finalReportItem.LastWriteTimeUtc.Ticks -ne $initialReportWriteTimeUtcTicks) {
        throw 'Selected QA report changed during bounded identity validation.'
    }
    Assert-QaReportTextIdentity `
        -ReportText $reportText `
        -ReportPath $reportContext.ReportPath `
        -QaRunsRoot $reportContext.QaRunsRoot `
        -WorldName $WorldName `
        -InstanceLabel $InstanceLabel `
        -RouteProfile $RouteProfile `
        -RouteFocus $RouteFocus `
        -WorldSeed $WorldSeed `
        -SceneryMode $SceneryMode `
        -SurfaceMode $SurfaceMode `
        -HydroMode $HydroMode `
        -CohortMode $CohortMode `
        -TerrainGrammarMode $TerrainGrammarMode `
        -L0HeightModeValue $L0HeightModeValue `
        -ExpectedBuildProfile $ExpectedBuildProfile `
        -ExpectedGitSha $ExpectedProvenance.GitSha `
        -ExpectedGitDirty $ExpectedProvenance.GitDirty `
        -ExpectedSourceFingerprint $ExpectedProvenance.SourceFingerprint `
        -ExpectedExecutableHash $ExpectedProvenance.ExecutableHash `
        -ExpectedToolchain $ExpectedProvenance.Toolchain `
        -ExpectedHardware $ExpectedProvenance.Hardware
    $finalReportDigest = Get-BoundedFileSha256 `
        -Path $reportContext.ReportPath `
        -MaxBytes $MaxQaReportBytes `
        -Label 'Selected QA report'
    if ($finalReportDigest.Length -ne $initialReportDigest.Length -or
        $finalReportDigest.LastWriteTimeUtcTicks -ne $initialReportDigest.LastWriteTimeUtcTicks -or
        -not $finalReportDigest.Hex.Equals($initialReportDigest.Hex, [StringComparison]::Ordinal)) {
        throw 'Selected QA report changed while its referenced screenshot evidence was validated.'
    }
}

function Assert-QaRunnerStaticFixtures {
    $minimumProcessTimeout = Get-QaProcessTimeoutMilliseconds -RouteSeconds 8.0
    $fractionalProcessTimeout = Get-QaProcessTimeoutMilliseconds -RouteSeconds 8.0001
    $maximumProcessTimeout = Get-QaProcessTimeoutMilliseconds -RouteSeconds 600.0
    if ($minimumProcessTimeout -ne 161000 -or
        $fractionalProcessTimeout -ne 161001 -or
        $maximumProcessTimeout -ne 753000) {
        throw 'QA static fixture found process-deadline arithmetic drift.'
    }
    foreach ($invalidRouteSeconds in @(
            [double]::NaN,
            [double]::PositiveInfinity,
            [double]::NegativeInfinity,
            7.999,
            600.001
        )) {
        $rejected = $false
        try {
            [void](Get-QaProcessTimeoutMilliseconds -RouteSeconds $invalidRouteSeconds)
        }
        catch {
            $rejected = $true
        }
        if (-not $rejected) {
            throw 'QA static fixture accepted an invalid process-deadline duration.'
        }
    }
    if ($QaProcessGracefulShutdownMilliseconds -lt 1 -or
        $QaProcessGracefulShutdownMilliseconds -gt 60000 -or
        $QaProcessForcedShutdownMilliseconds -lt 1 -or
        $QaProcessForcedShutdownMilliseconds -gt 60000) {
        throw 'QA static fixture found an invalid bounded shutdown wait.'
    }
    $runnerTokens = $null
    $runnerParseErrors = $null
    $runnerAst = [System.Management.Automation.Language.Parser]::ParseFile(
        $PSCommandPath,
        [ref]$runnerTokens,
        [ref]$runnerParseErrors
    )
    if ($runnerParseErrors.Count -ne 0) {
        throw 'QA runner static fixture could not parse its own source.'
    }
    $unboundedProcessWaits = @($runnerAst.FindAll({
                param($node)
                return $node -is [System.Management.Automation.Language.InvokeMemberExpressionAst] -and
                    $node.Member.Value -eq 'WaitForExit' -and
                    $node.Arguments.Count -eq 0
            }, $true))
    if ($unboundedProcessWaits.Count -ne 0) {
        throw 'QA runner contains an unbounded zero-argument process wait.'
    }

    Assert-QaSurfaceEvidenceCompatibility `
        -SurfaceMode 'lod-provenance-v1' `
        -HydroMode 'off' `
        -CohortMode 'off' `
        -ViewportWidth 1920 `
        -ViewportHeight 1080
    Assert-QaSurfaceEvidenceCompatibility `
        -SurfaceMode 'bridge-v2' `
        -HydroMode 'v1' `
        -CohortMode 'v1' `
        -ViewportWidth 1280 `
        -ViewportHeight 720
    $surfaceRejectionFixtures = @(
        { Assert-QaSurfaceEvidenceCompatibility -SurfaceMode 'lod-provenance-v1' -HydroMode 'off' -CohortMode 'off' -ViewportWidth 1280 -ViewportHeight 1080 },
        { Assert-QaSurfaceEvidenceCompatibility -SurfaceMode 'lod-provenance-v1' -HydroMode 'off' -CohortMode 'off' -ViewportWidth 1920 -ViewportHeight 720 },
        { Assert-QaSurfaceEvidenceCompatibility -SurfaceMode 'lod-provenance-v1' -HydroMode 'v1' -CohortMode 'off' -ViewportWidth 1920 -ViewportHeight 1080 },
        { Assert-QaSurfaceEvidenceCompatibility -SurfaceMode 'lod-provenance-v1' -HydroMode 'off' -CohortMode 'v1' -ViewportWidth 1920 -ViewportHeight 1080 }
    )
    foreach ($fixture in $surfaceRejectionFixtures) {
        $rejected = $false
        try {
            & $fixture
        }
        catch {
            $rejected = $true
        }
        if (-not $rejected) {
            throw 'QA static fixture accepted an invalid LOD-provenance analyzer contract.'
        }
    }

    Assert-QaExecutableFreshness `
        -SourceNewestWriteTimeUtcTicks 100 `
        -ExecutableWriteTimeUtcTicks 101
    foreach ($staleExecutableTicks in @(99, 100)) {
        $rejected = $false
        try {
            Assert-QaExecutableFreshness `
                -SourceNewestWriteTimeUtcTicks 100 `
                -ExecutableWriteTimeUtcTicks $staleExecutableTicks
        }
        catch {
            $rejected = $true
        }
        if (-not $rejected) {
            throw 'QA static fixture accepted a stale or equal-timestamp executable.'
        }
    }

    $newArtifactFixture = {
        param([string]$SourceHash, [string]$ExecutableHash)
        return [pscustomobject]@{
            SourceFingerprint = $SourceHash
            SourceFileCount = 7
            SourceBytes = 70
            SourceNewestWriteTimeUtcTicks = 100
            ExecutableHash = $ExecutableHash
            ExecutableBytes = 700
            ExecutableWriteTimeUtcTicks = 101
        }
    }
    $expectedArtifacts = & $newArtifactFixture 'sha256:source-a' 'sha256:executable-a'
    Assert-QaArtifactSnapshotIdentity `
        -Expected $expectedArtifacts `
        -Actual (& $newArtifactFixture 'sha256:source-a' 'sha256:executable-a') `
        -Boundary 'static-fixture'
    foreach ($changedArtifacts in @(
            (& $newArtifactFixture 'sha256:source-b' 'sha256:executable-a'),
            (& $newArtifactFixture 'sha256:source-a' 'sha256:executable-b')
        )) {
        $rejected = $false
        try {
            Assert-QaArtifactSnapshotIdentity `
                -Expected $expectedArtifacts `
                -Actual $changedArtifacts `
                -Boundary 'static-fixture'
        }
        catch {
            $rejected = $true
        }
        if (-not $rejected) {
            throw 'QA static fixture accepted source or executable hash drift.'
        }
    }

    $settingsFixturePath = [System.IO.Path]::Combine(
        [System.IO.Path]::GetTempPath(),
        "voxel-native-settings-identity-$([Guid]::NewGuid().ToString('N')).ron"
    )
    try {
        $assertSettingsIdentityRejected = {
            param(
                [Parameter(Mandatory = $true)]
                [psobject]$Expected,

                [Parameter(Mandatory = $true)]
                [string]$Case
            )

            $rejected = $false
            try {
                Assert-QaOptionalFileUnchanged `
                    -Expected $Expected `
                    -Path $settingsFixturePath `
                    -MaxBytes 4 `
                    -Label 'Settings identity fixture'
            }
            catch {
                $expectedFragment = 'endpoint identity changed across QA boundaries'
                if ($_.Exception -isnot [System.Management.Automation.RuntimeException] -or
                    $_.Exception.Message.IndexOf(
                        $expectedFragment,
                        [StringComparison]::Ordinal
                    ) -lt 0) {
                    throw "QA settings identity fixture '$Case' observed the wrong rejection: $($_.Exception.GetType().FullName): $($_.Exception.Message)"
                }
                $rejected = $true
            }
            if (-not $rejected) {
                throw "QA settings identity fixture accepted $Case."
            }
        }

        $missingSettings = Get-QaOptionalFileIdentity `
            -Path $settingsFixturePath `
            -MaxBytes 4 `
            -Label 'Settings identity fixture'
        Assert-QaOptionalFileUnchanged `
            -Expected $missingSettings `
            -Path $settingsFixturePath `
            -MaxBytes 4 `
            -Label 'Settings identity fixture'

        [System.IO.File]::WriteAllBytes($settingsFixturePath, [byte[]](1, 2, 3, 4))
        & $assertSettingsIdentityRejected `
            -Expected $missingSettings `
            -Case 'creation of a persistent settings file'

        $existingSettings = Get-QaOptionalFileIdentity `
            -Path $settingsFixturePath `
            -MaxBytes 4 `
            -Label 'Settings identity fixture'
        Assert-QaOptionalFileUnchanged `
            -Expected $existingSettings `
            -Path $settingsFixturePath `
            -MaxBytes 4 `
            -Label 'Settings identity fixture'

        [System.IO.File]::Delete($settingsFixturePath)
        & $assertSettingsIdentityRejected `
            -Expected $existingSettings `
            -Case 'deletion of a persistent settings file'

        [System.IO.File]::WriteAllBytes($settingsFixturePath, [byte[]](1, 2, 3, 4))
        $metadataBaseline = Get-QaOptionalFileIdentity `
            -Path $settingsFixturePath `
            -MaxBytes 4 `
            -Label 'Settings identity fixture'
        [System.IO.File]::SetLastWriteTimeUtc(
            $settingsFixturePath,
            [DateTime]::UtcNow.AddMinutes(-5)
        )
        $metadataDrift = Get-QaOptionalFileIdentity `
            -Path $settingsFixturePath `
            -MaxBytes 4 `
            -Label 'Settings identity fixture'
        if ($metadataDrift.LastWriteTimeUtcTicks -eq $metadataBaseline.LastWriteTimeUtcTicks -or
            -not ([string]$metadataDrift.Hex).Equals(
                [string]$metadataBaseline.Hex,
                [StringComparison]::Ordinal
            )) {
            throw 'QA settings metadata-only fixture could not establish identical bytes with a distinct timestamp.'
        }
        & $assertSettingsIdentityRejected `
            -Expected $metadataBaseline `
            -Case 'metadata-only persistent settings drift'

        $byteBaseline = Get-QaOptionalFileIdentity `
            -Path $settingsFixturePath `
            -MaxBytes 4 `
            -Label 'Settings identity fixture'
        [System.IO.File]::WriteAllBytes($settingsFixturePath, [byte[]](4, 3, 2, 1))
        [System.IO.File]::SetLastWriteTimeUtc(
            $settingsFixturePath,
            [DateTime]::new(
                [int64]$byteBaseline.LastWriteTimeUtcTicks,
                [DateTimeKind]::Utc
            )
        )
        $byteDrift = Get-QaOptionalFileIdentity `
            -Path $settingsFixturePath `
            -MaxBytes 4 `
            -Label 'Settings identity fixture'
        if ($byteDrift.Length -ne $byteBaseline.Length -or
            $byteDrift.LastWriteTimeUtcTicks -ne $byteBaseline.LastWriteTimeUtcTicks -or
            ([string]$byteDrift.Hex).Equals(
                [string]$byteBaseline.Hex,
                [StringComparison]::Ordinal
            )) {
            throw 'QA settings byte-only fixture could not establish distinct bytes with identical length and timestamp.'
        }
        & $assertSettingsIdentityRejected `
            -Expected $byteBaseline `
            -Case 'persistent settings byte drift'
    }
    finally {
        if ([System.IO.File]::Exists($settingsFixturePath)) {
            [System.IO.File]::Delete($settingsFixturePath)
        }
    }

    $lockFixturePath = [System.IO.Path]::Combine(
        [System.IO.Path]::GetTempPath(),
        "voxel-native-executable-lock-$([Guid]::NewGuid().ToString('N')).bin"
    )
    [System.IO.File]::WriteAllBytes($lockFixturePath, [byte[]](1, 2, 3, 4))
    $lockFixture = $null
    try {
        $lockFixture = Open-QaExecutableReadLock -Path $lockFixturePath -MaxBytes 4
        $writeRejected = $false
        try {
            $unexpectedWriter = [System.IO.File]::Open(
                $lockFixturePath,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::Write,
                [System.IO.FileShare]::ReadWrite
            )
            $unexpectedWriter.Dispose()
        }
        catch [System.IO.IOException] {
            $writeRejected = $true
        }
        if (-not $writeRejected) {
            throw 'QA executable read lock allowed a concurrent write handle.'
        }
    }
    finally {
        if ($null -ne $lockFixture) {
            $lockFixture.Dispose()
        }
        if ([System.IO.File]::Exists($lockFixturePath)) {
            [System.IO.File]::Delete($lockFixturePath)
        }
    }
}

function Assert-QaReportParserFixtures {
    $fixtureRoot = [System.IO.Path]::Combine(
        [System.IO.Path]::GetTempPath(),
        "voxel-native-qa-parser-$([Guid]::NewGuid().ToString('N'))"
    )
    $fixtureQaRunsRoot = Join-Path $fixtureRoot 'qa_runs'
    $fixtureRunDirectory = Join-Path $fixtureQaRunsRoot 'run_123456'
    $fixtureReportPath = Join-Path $fixtureRunDirectory 'report.ron'
    $fixtureScreenshotPath = Join-Path $fixtureRunDirectory 'shot_0000_detail.png'
    [void][System.IO.Directory]::CreateDirectory($fixtureRunDirectory)
    [byte[]]$validPngBytes = [Convert]::FromBase64String(
        'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII='
    )
    [System.IO.File]::WriteAllBytes($fixtureScreenshotPath, $validPngBytes)
    try {
        # A resident L0 may install entirely from an unchanged sample cache. Its
        # truthful `last_*` query counters are then zero; resident mode identity,
        # not a cumulative interpretation of those counters, proves completion.
        $reusedPoint16Report = @'
qa_report_schema_version: "2.5.0",
evidence_disposition: "canonical-candidate",
build_profile: "release",
world_name: Some("qa_parser_fixture"),
instance_label: Some("QA PARSER FIXTURE"),
world_seed: Some(12345),
world_profile: Some("Natural"),
scenery_quality: Some("Lush"),
terrain_grammar: Some("V3"),
git_sha: Some("abcdef1234567890"),
git_dirty: Some(false),
source_fingerprint: Some("sha256:source-fixture"),
executable_hash: Some("sha256:executable-fixture"),
toolchain: Some("rustc fixture"),
hardware: Some("GPU \"fixture\" C:\\Device"),
world_edit_store_status: "compatible",
world_edit_store_compatible: true,
world_edit_store_seed: Some(12345),
world_edit_store_profile: Some("Natural"),
world_edit_store_scenery_quality: Some("Lush"),
world_edit_store_terrain_grammar: Some("V3"),
world_edit_store_edited_chunks: Some(0),
world_edit_store_block_reason_code: None,
viewport: Some((
    logical_width: 1.0,
    logical_height: 1.0,
    physical_width: 1,
    physical_height: 1,
    scale_factor: 1.0,
    dpi_percent: 100.0,
)),
profile: "Natural",
desired_terrain_grammar: Some("V3"),
active_terrain_grammar: Some("V3"),
desired_l0_height_mode: "Point16V1",
active_l0_height_mode: Some("Point16V1"),
resident_l0_height_mode: Some("Point16V1"),
l0_probe_spacing_metres: 8,
budget_l0_height_queries: 12805,
near_coverage_transition_pending: false,
resident_observation_valid: true,
resident_entity_count_overflow: false,
resident_duplicate_levels: 0,
resident_out_of_range_levels: 0,
resident_scheduler_mismatch: false,
resident_budget_exceeded: false,
resident_observation_rejections: 0,
resident_fluid_observation_valid: true,
resident_fluid_entity_count_overflow: false,
resident_fluid_duplicate_slots: 0,
resident_fluid_out_of_range_levels: 0,
resident_fluid_scheduler_mismatch: false,
resident_fluid_budget_exceeded: false,
resident_fluid_kind_integrity_valid: true,
resident_fluid_observation_rejections: 0,
resident_semantic_cohort_observation_valid: true,
resident_semantic_cohort_entity_count_overflow: false,
resident_semantic_cohort_scheduler_mismatch: false,
resident_semantic_cohort_budget_exceeded: false,
resident_semantic_cohort_payload_integrity_valid: true,
resident_semantic_cohort_observation_rejections: 0,
live_sample_cache_windows: 6,
live_sample_cache_bytes: 228822,
peak_live_sample_cache_windows: 6,
peak_live_sample_cache_bytes: 228822,
budget_sample_cache_bytes: 524288,
pending_rebuilds: 0,
dirty_mask: 0,
build_in_flight: false,
budget_rejections: 0,
surface_material_mode: "BridgeV2",
hydro_mode: "Disabled",
semantic_cohort_mode: "Disabled",
requested_route_focus: "streaming",
resolved_route_focus: "streaming",
route_focus_available: true,
route_focus_unavailable_reason: None,
route_focus_search_cap_exhausted: false,
camera_route_policy: "preflight-v1",
last_l0_center_queries: 0,
last_l0_half_x_queries: 0,
last_l0_half_z_queries: 0,
last_l0_cache_update: "IncrementalStrip",
last_l0_cache_shift_x_cells: 0,
last_l0_cache_shift_z_cells: 0,
last_l0_reused_height_samples: 4225,
last_l0_trimmed_vertices: 0,
last_l0_trimmed_up_vertices: 0,
last_l0_trimmed_down_vertices: 0,
last_l0_max_abs_adjustment_metres: 0.0,
camera_route_preflight_applicable: false,
camera_route_plan_hash: None,
camera_route_available: false,
camera_route_unavailable_reason: None,
camera_route_variant_index: None,
camera_route_variant_count: 0,
camera_route_validation_samples: 0,
camera_route_voxel_queries: 0,
camera_route_voxel_query_cap: 0,
camera_route_required_chunk_checks: 0,
camera_route_loaded_chunk_checks: 0,
camera_route_proven_air_chunk_checks: 0,
camera_route_unloaded_chunk_checks: 0,
camera_route_candidate_body_occlusions: 0,
camera_route_candidate_los_occlusions: 0,
camera_route_selected_clear_samples: 0,
camera_route_minimum_clearance_voxels: None,
camera_route_work_cap_exhausted: false,
requested_duration_seconds: 10.0,
loaded_chunks: 1,
mesh_entities: 1,
pending_terrain: 0,
pending_meshes: 0,
dirty_chunks: 0,
dense_chunks: 1,
dense_chunk_budget: 2400,
dense_chunk_budget_exceeded: false,
frontier_complete: true,
render_distance: 8,
peak_loaded_chunks: 1,
peak_dense_chunks: 1,
peak_pending_terrain: 0,
screenshots: [
    "qa_runs\\run_123456\\shot_0000_detail.png",
],
screenshot_observation_cap: 600,
screenshot_path_max_chars: 512,
screenshot_observation_count: 1,
screenshot_observation_valid: true,
screenshot_observation_cap_exhausted: false,
screenshot_observation_rejections: 0,
screenshot_observations: [
    (
        capture_index: 0,
        screenshot_path: "qa_runs\\run_123456\\shot_0000_detail.png",
        scheduled_capture_seconds: 2.5,
        player_camera_translation_metres: (1.0, 2.0, 3.0),
        player_camera_rotation_xyzw: (0.0, 0.0, 0.0, 1.0),
    ),
],
'@
    $assertPointFixture = {
        param([string]$FixtureText, [string]$SelectedReportPath)
        if ([string]::IsNullOrWhiteSpace($SelectedReportPath)) {
            $SelectedReportPath = $fixtureReportPath
        }
        [System.IO.File]::WriteAllText($SelectedReportPath, $FixtureText)
        Assert-QaReportTextIdentity `
            -ReportText $FixtureText `
            -ReportPath $SelectedReportPath `
            -QaRunsRoot $fixtureQaRunsRoot `
            -WorldName 'qa_parser_fixture' `
            -InstanceLabel 'QA PARSER FIXTURE' `
            -RouteProfile 'natural' `
            -RouteFocus 'streaming' `
            -WorldSeed 12345 `
            -SceneryMode 'lush' `
            -SurfaceMode 'bridge-v2' `
            -HydroMode 'off' `
            -CohortMode 'off' `
            -TerrainGrammarMode 'v3' `
            -L0HeightModeValue 'point-16-v1' `
            -ExpectedBuildProfile 'release' `
            -ExpectedGitSha 'abcdef1234567890' `
            -ExpectedGitDirty $false `
            -ExpectedSourceFingerprint 'sha256:source-fixture' `
            -ExpectedExecutableHash 'sha256:executable-fixture' `
            -ExpectedToolchain 'rustc fixture' `
            -ExpectedHardware 'GPU "fixture" C:\Device'
    }
    & $assertPointFixture $reusedPoint16Report
    $incrementalPoint16Report = $reusedPoint16Report.Replace(
        'last_l0_center_queries: 0',
        'last_l0_center_queries: 65'
    ).Replace(
        'last_l0_cache_shift_x_cells: 0',
        'last_l0_cache_shift_x_cells: -1'
    ).Replace(
        'last_l0_reused_height_samples: 4225',
        'last_l0_reused_height_samples: 4160'
    )
    & $assertPointFixture $incrementalPoint16Report
    $teleportSentinelPoint16Report = $reusedPoint16Report.Replace(
        'last_l0_center_queries: 0',
        'last_l0_center_queries: 4225'
    ).Replace(
        'last_l0_cache_update: "IncrementalStrip"',
        'last_l0_cache_update: "TeleportFallback"'
    ).Replace(
        'last_l0_reused_height_samples: 4225',
        'last_l0_reused_height_samples: 0'
    )
    & $assertPointFixture $teleportSentinelPoint16Report
    $noncanonicalReportPath = Join-Path $fixtureRunDirectory 'alternate.ron'
    $rejected = $false
    try {
        & $assertPointFixture $reusedPoint16Report $noncanonicalReportPath
    }
    catch {
        $rejected = $true
    }
    if (-not $rejected) {
        throw 'QA report parser accepted a selected report path other than canonical report.ron.'
    }

    $rejectionFixtures = [ordered]@{
        resident_observation_invalid = $reusedPoint16Report.Replace(
            'resident_observation_valid: true',
            'resident_observation_valid: false'
        )
        unsettled_far_field = $reusedPoint16Report.Replace(
            'pending_rebuilds: 0',
            'pending_rebuilds: 5'
        ).Replace(
            'dirty_mask: 0',
            'dirty_mask: 61'
        ).Replace(
            'build_in_flight: false',
            'build_in_flight: true'
        )
        unsettled_near_field = $reusedPoint16Report.Replace(
            'dirty_chunks: 0',
            'dirty_chunks: 118'
        )
        dense_chunk_budget_exceeded = $reusedPoint16Report.Replace(
            'dense_chunk_budget_exceeded: false',
            'dense_chunk_budget_exceeded: true'
        )
        dense_chunk_total_over_budget = $reusedPoint16Report.Replace(
            'dense_chunks: 1',
            'dense_chunks: 2401'
        )
        dense_chunk_total_mismatch = $reusedPoint16Report.Replace(
            'dense_chunks: 1',
            'dense_chunks: 2'
        )
        dense_chunk_budget_drift = $reusedPoint16Report.Replace(
            'dense_chunk_budget: 2400',
            'dense_chunk_budget: 2399'
        )
        peak_dense_chunk_over_budget = $reusedPoint16Report.Replace(
            'peak_dense_chunks: 1',
            'peak_dense_chunks: 2401'
        )
        peak_dense_chunk_below_current = $reusedPoint16Report.Replace(
            'peak_dense_chunks: 1',
            'peak_dense_chunks: 0'
        )
        near_coverage_transition_pending = $reusedPoint16Report.Replace(
            'near_coverage_transition_pending: false',
            'near_coverage_transition_pending: true'
        )
        near_coverage_transition_missing = $reusedPoint16Report.Replace(
            'near_coverage_transition_pending: false,',
            ''
        )
        near_coverage_transition_duplicate = $reusedPoint16Report.Replace(
            'near_coverage_transition_pending: false,',
            "near_coverage_transition_pending: false,`nnear_coverage_transition_pending: false,"
        )
        near_coverage_transition_wrong_top_level = $reusedPoint16Report.Replace(
            'near_coverage_transition_pending: false,',
            ''
        ).Replace(
            'frontier_complete: true,',
            "near_coverage_transition_pending: false,`nfrontier_complete: true,"
        )
        frontier_incomplete = $reusedPoint16Report.Replace(
            'frontier_complete: true',
            'frontier_complete: false'
        )
        frontier_complete_wrong_top_level_order = $reusedPoint16Report.Replace(
            'frontier_complete: true,',
            ''
        ).Replace(
            'loaded_chunks: 1,',
            "frontier_complete: true,`nloaded_chunks: 1,"
        )
        frontier_complete_missing = $reusedPoint16Report.Replace(
            'frontier_complete: true,',
            ''
        )
        frontier_complete_duplicate = $reusedPoint16Report.Replace(
            'frontier_complete: true,',
            "frontier_complete: true,`nfrontier_complete: true,"
        )
        point_cache_over_mode_ceiling = $reusedPoint16Report.Replace(
            'live_sample_cache_bytes: 228822',
            'live_sample_cache_bytes: 228823'
        )
        point_cache_underreported = $reusedPoint16Report.Replace(
            'peak_live_sample_cache_bytes: 228822',
            'peak_live_sample_cache_bytes: 228821'
        )
        cache_window_underreported = $reusedPoint16Report.Replace(
            'live_sample_cache_windows: 6',
            'live_sample_cache_windows: 5'
        )
        center_query_channel_overflow = $reusedPoint16Report.Replace(
            'last_l0_center_queries: 0',
            'last_l0_center_queries: 4226'
        )
        point_query_underreported = $incrementalPoint16Report.Replace(
            'last_l0_center_queries: 65',
            'last_l0_center_queries: 64'
        )
        teleport_small_shift = $teleportSentinelPoint16Report.Replace(
            'last_l0_cache_shift_x_cells: 0',
            'last_l0_cache_shift_x_cells: 1'
        )
        screenshot_index_out_of_range = $reusedPoint16Report.Replace(
            'capture_index: 0',
            'capture_index: 1'
        )
        screenshot_filename_index_mismatch = $reusedPoint16Report.Replace(
            'shot_0000_detail.png',
            'shot_0001_detail.png'
        )
        screenshot_absolute_drive_path = $reusedPoint16Report.Replace(
            'qa_runs\\run_123456\\shot_0000_detail.png',
            'C:\\outside\\shot_0000_detail.png'
        )
        screenshot_unc_path = $reusedPoint16Report.Replace(
            'qa_runs\\run_123456\\shot_0000_detail.png',
            '\\\\server\\share\\shot_0000_detail.png'
        )
        screenshot_traversal_path = $reusedPoint16Report.Replace(
            'qa_runs\\run_123456\\shot_0000_detail.png',
            'qa_runs\\run_123456\\..\\shot_0000_detail.png'
        )
        screenshot_arbitrary_prefix = $reusedPoint16Report.Replace(
            'qa_runs\\run_123456\\shot_0000_detail.png',
            'elsewhere\\run_123456\\shot_0000_detail.png'
        )
        screenshot_nested_child = $reusedPoint16Report.Replace(
            'qa_runs\\run_123456\\shot_0000_detail.png',
            'qa_runs\\run_123456\\nested\\shot_0000_detail.png'
        )
        screenshot_viewport_mismatch = $reusedPoint16Report.Replace(
            'physical_width: 1',
            'physical_width: 2'
        )
        provenance_mismatch = $reusedPoint16Report.Replace(
            'git_sha: Some("abcdef1234567890")',
            'git_sha: Some("ffffffffffffffff")'
        )
    }
    foreach ($fixtureName in $rejectionFixtures.Keys) {
        $rejected = $false
        try {
            & $assertPointFixture $rejectionFixtures[$fixtureName]
        }
        catch {
            $rejected = $true
        }
        if (-not $rejected) {
            throw "QA report parser rejection fixture '$fixtureName' was unexpectedly accepted."
        }
    }

    [byte[]]$badPngSignature = $validPngBytes.Clone()
    $badPngSignature[0] = 0
    [byte[]]$badPngCrc = $validPngBytes.Clone()
    $badPngCrc[29] = $badPngCrc[29] -bxor 1
    [byte[]]$truncatedPng = [byte[]]::new($validPngBytes.Length - 1)
    [Array]::Copy($validPngBytes, $truncatedPng, $truncatedPng.Length)
    [byte[]]$trailingPng = [byte[]]::new($validPngBytes.Length + 1)
    [Array]::Copy($validPngBytes, $trailingPng, $validPngBytes.Length)
    $trailingPng[$trailingPng.Length - 1] = 0
    $pngRejectionFixtures = [ordered]@{
        invalid_png_signature = $badPngSignature
        invalid_png_crc = $badPngCrc
        truncated_png_iend = $truncatedPng
        trailing_png_data = $trailingPng
    }
    foreach ($fixtureName in $pngRejectionFixtures.Keys) {
        $rejected = $false
        try {
            [System.IO.File]::WriteAllBytes(
                $fixtureScreenshotPath,
                [byte[]]$pngRejectionFixtures[$fixtureName]
            )
            & $assertPointFixture $reusedPoint16Report
        }
        catch {
            $rejected = $true
        }
        finally {
            [System.IO.File]::WriteAllBytes($fixtureScreenshotPath, $validPngBytes)
        }
        if (-not $rejected) {
            throw "QA PNG parser rejection fixture '$fixtureName' was unexpectedly accepted."
        }
    }

    $compositeDiagnosticReport = $reusedPoint16Report.Replace(
        'qa_report_schema_version: "2.5.0"',
        'qa_report_schema_version: "2.5.0-diagnostic-l0-cardinal-trimmed-8-v1-lod-provenance-v1"'
    )
    $compositeDiagnosticReport = $compositeDiagnosticReport.Replace(
        'evidence_disposition: "canonical-candidate"',
        'evidence_disposition: "diagnostic-l0-height-and-lod-provenance-only-non-publishable"'
    )
    $compositeDiagnosticReport = $compositeDiagnosticReport.Replace(
        'Point16V1',
        'CardinalTrimmed8V1'
    )
    $compositeDiagnosticReport = $compositeDiagnosticReport.Replace(
        'surface_material_mode: "BridgeV2"',
        'surface_material_mode: "LodProvenanceV1"'
    )
    $compositeDiagnosticReport = $compositeDiagnosticReport.Replace('228822', '263142')
    $compositeDiagnosticReport = $compositeDiagnosticReport.Replace(
        'last_l0_reused_height_samples: 4225',
        'last_l0_reused_height_samples: 12805'
    )
    $assertCompositeFixture = {
        param([string]$FixtureText)
        [System.IO.File]::WriteAllText($fixtureReportPath, $FixtureText)
        Assert-QaReportTextIdentity `
            -ReportText $FixtureText `
            -ReportPath $fixtureReportPath `
            -QaRunsRoot $fixtureQaRunsRoot `
            -WorldName 'qa_parser_fixture' `
            -InstanceLabel 'QA PARSER FIXTURE' `
            -RouteProfile 'natural' `
            -RouteFocus 'streaming' `
            -WorldSeed 12345 `
            -SceneryMode 'lush' `
            -SurfaceMode 'lod-provenance-v1' `
            -HydroMode 'off' `
            -CohortMode 'off' `
            -TerrainGrammarMode 'v3' `
            -L0HeightModeValue 'cardinal-trimmed-8-v1' `
            -ExpectedBuildProfile 'release' `
            -ExpectedGitSha 'abcdef1234567890' `
            -ExpectedGitDirty $false `
            -ExpectedSourceFingerprint 'sha256:source-fixture' `
            -ExpectedExecutableHash 'sha256:executable-fixture' `
            -ExpectedToolchain 'rustc fixture' `
            -ExpectedHardware 'GPU "fixture" C:\Device'
    }
    & $assertCompositeFixture $compositeDiagnosticReport
    $incrementalCompositeReport = $compositeDiagnosticReport.Replace(
        'last_l0_center_queries: 0',
        'last_l0_center_queries: 65'
    ).Replace(
        'last_l0_half_x_queries: 0',
        'last_l0_half_x_queries: 65'
    ).Replace(
        'last_l0_half_z_queries: 0',
        'last_l0_half_z_queries: 66'
    ).Replace(
        'last_l0_cache_shift_x_cells: 0',
        'last_l0_cache_shift_x_cells: -1'
    ).Replace(
        'last_l0_reused_height_samples: 12805',
        'last_l0_reused_height_samples: 12609'
    )
    & $assertCompositeFixture $incrementalCompositeReport

    $candidateQueryRejectionFixtures = [ordered]@{
        redistributed_half_x = $compositeDiagnosticReport.Replace(
            'last_l0_half_x_queries: 0',
            'last_l0_half_x_queries: 4291'
        )
        mixed_zero_nonzero_planes = $compositeDiagnosticReport.Replace(
            'last_l0_center_queries: 0',
            'last_l0_center_queries: 1'
        )
        underreported_positive_identity = $incrementalCompositeReport.Replace(
            'last_l0_center_queries: 65',
            'last_l0_center_queries: 64'
        )
        mismatched_shift_identity = $incrementalCompositeReport.Replace(
            'last_l0_cache_shift_x_cells: -1',
            'last_l0_cache_shift_x_cells: -2'
        )
    }
    foreach ($fixtureName in $candidateQueryRejectionFixtures.Keys) {
        $rejected = $false
        try {
            & $assertCompositeFixture $candidateQueryRejectionFixtures[$fixtureName]
        }
        catch {
            $rejected = $true
        }
        if (-not $rejected) {
            throw "QA report parser accepted invalid candidate query identity fixture '$fixtureName'."
        }
    }
    }
    finally {
        $canonicalFixtureRoot = [System.IO.Path]::GetFullPath($fixtureRoot).TrimEnd([char[]]'\/')
        $canonicalTempRoot = [System.IO.Path]::GetFullPath(
            [System.IO.Path]::GetTempPath()
        ).TrimEnd([char[]]'\/')
        $tempPrefix = $canonicalTempRoot + [System.IO.Path]::DirectorySeparatorChar
        if (-not $canonicalFixtureRoot.StartsWith(
                $tempPrefix,
                [StringComparison]::OrdinalIgnoreCase) -or
            -not [System.IO.Path]::GetFileName($canonicalFixtureRoot).StartsWith(
                'voxel-native-qa-parser-',
                [StringComparison]::Ordinal)) {
            throw 'QA parser fixture cleanup target escaped its dedicated temporary root.'
        }
        if ([System.IO.Directory]::Exists($canonicalFixtureRoot)) {
            [System.IO.Directory]::Delete($canonicalFixtureRoot, $true)
        }
    }
}

$script:QaFirstProvenance = $null

function Invoke-StreamingRoute {
    param([string]$RouteProfile)

    Write-Host "Collecting bounded provenance before the $RouteProfile route."
    $provenance = Get-QaProvenance -Root $projectRoot -ExecutablePath $executable
    Write-QaProvenance -Provenance $provenance
    if ($null -eq $script:QaFirstProvenance) {
        $script:QaFirstProvenance = $provenance
    }
    else {
        foreach ($field in @(
                'GitSha',
                'GitDirty',
                'SourceFingerprint',
                'SourceFileCount',
                'SourceBytes',
                'SourceNewestWriteTimeUtcTicks',
                'ExecutableHash',
                'ExecutableBytes',
                'ExecutableWriteTimeUtcTicks',
                'Toolchain',
                'Hardware'
            )) {
            if (-not ([string]$provenance.$field).Equals(
                    [string]$script:QaFirstProvenance.$field,
                    [StringComparison]::Ordinal)) {
                throw "QA provenance field $field changed between routes; same-binary evidence is required."
            }
        }
    }

    $epoch = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $runToken = [Guid]::NewGuid().ToString('N').Substring(0, 12)
    $profileTag = if ($RouteProfile -eq 'natural') { 'nat' } else { 'ast' }
    $focusTag = switch ($Focus) {
        'scenic' { 'sc' }
        'waypoint' { 'wp' }
        'streaming' { 'st' }
        'river' { 'rv' }
        'lava' { 'lv' }
        'near-far' { 'nf' }
    }
    $sceneryTag = switch ($Scenery) {
        'off' { 'q0' }
        'lean' { 'ql' }
        'balanced' { 'qb' }
        'lush' { 'qu' }
    }
    $surfaceTag = switch ($SurfaceMaterial) {
        'legacy' { 'mlg' }
        'bridge-v1' { 'mb1' }
        'bridge-v2' { 'mb2' }
        'lod-provenance-v1' { 'mlp' }
    }
    $hydroTag = if ($Hydro -eq 'off') { 'h0' } else { 'h1' }
    $cohortTag = if ($Cohorts -eq 'off') { 'c0' } else { 'c1' }
    $l0HeightTag = if ($L0HeightMode -eq 'cardinal-trimmed-8-v1') { 'ct8' } else { 'p16' }
    $worldName = "qa_${profileTag}${focusTag}_s${Seed}_${sceneryTag}${TerrainGrammar}_${surfaceTag}${hydroTag}${cohortTag}${l0HeightTag}_${epoch}_${runToken}"
    $instanceLabel = "QA $runToken|$($profileTag.ToUpperInvariant())|$($Focus.ToUpperInvariant())|S$Seed|$($Scenery.ToUpperInvariant())|G:$($TerrainGrammar.ToUpperInvariant())|M:$($surfaceTag.ToUpperInvariant())|H:$($hydroTag.Substring(1))|C:$($cohortTag.Substring(1))|L0:$($l0HeightTag.ToUpperInvariant())"
    if ($instanceLabel.Length -gt 96) {
        throw 'QA instance identity exceeds the engine report bound of 96 characters.'
    }
    Assert-QaDerivedWorldPathBudget -Root $projectRoot -WorldName $worldName
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $executable
    $startInfo.WorkingDirectory = $projectRoot
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $false
    $startInfo.Arguments = '--qa'

    # A QA launch must not silently inherit stale Voxel-Native controls from
    # the caller. This mutates only ProcessStartInfo's child environment; the
    # current PowerShell process environment remains untouched.
    $inheritedVoxelVariables = @($startInfo.EnvironmentVariables.Keys |
        Where-Object { ([string]$_).StartsWith('VOXEL_NATIVE_', [StringComparison]::OrdinalIgnoreCase) })
    foreach ($variableName in $inheritedVoxelVariables) {
        $startInfo.EnvironmentVariables.Remove([string]$variableName)
    }

    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA'] = '1'
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_FOCUS'] = $Focus
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_CAMERA_ROUTE_POLICY'] = 'preflight-v1'
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_PROFILE'] = $RouteProfile
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_SEED'] = [string]$Seed
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_SCENERY'] = $Scenery
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_TERRAIN_GRAMMAR'] = $TerrainGrammar
    # QA must exercise the far-field layer in both profiles. The normal runtime
    # default remains Astral-only until the Natural route has visual evidence.
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_PLANETARY_STREAMING'] = 'all'
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_DISTANCE_KM'] = [string]::Format(
        [Globalization.CultureInfo]::InvariantCulture,
        '{0:0.###}',
        $DistanceKm
    )
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_SECONDS'] = [string]::Format(
        [Globalization.CultureInfo]::InvariantCulture,
        '{0:0.###}',
        $Seconds
    )
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_SCREENSHOT_INTERVAL'] = [string]::Format(
        [Globalization.CultureInfo]::InvariantCulture,
        '{0:0.###}',
        $ScreenshotInterval
    )
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_WORLD'] = $worldName
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_FAR_SURFACE_MATERIAL'] = $SurfaceMaterial
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_FAR_HYDROGRAPHY'] = $Hydro
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_FAR_SEMANTIC_COHORTS'] = $Cohorts
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_FAR_L0_HEIGHT_MODE'] = $L0HeightMode
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_GIT_SHA'] = $provenance.GitSha
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_GIT_DIRTY'] = if ($provenance.GitDirty) {
        'true'
    }
    else {
        'false'
    }
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_SOURCE_FINGERPRINT'] = $provenance.SourceFingerprint
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_EXECUTABLE_HASH'] = $provenance.ExecutableHash
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_TOOLCHAIN'] = $provenance.Toolchain
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_HARDWARE'] = $provenance.Hardware
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_HOUR'] = [string]::Format(
        [Globalization.CultureInfo]::InvariantCulture,
        '{0:0.###}',
        $Hour
    )
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_WINDOW_WIDTH'] = [string]$Width
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_WINDOW_HEIGHT'] = [string]$Height
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_INSTANCE_LABEL'] = $instanceLabel
    $processTimeoutMilliseconds = Get-QaProcessTimeoutMilliseconds -RouteSeconds $Seconds

    Write-Host "Starting $RouteProfile $Focus route: seed $Seed, scenery $Scenery, grammar $TerrainGrammar, surface $SurfaceMaterial, hydro $Hydro, cohorts $Cohorts, L0 height $L0HeightMode, $DistanceKm km, $Seconds s, ${Width}x${Height}."
    Write-Host "Isolated world: $worldName"
    Write-Host "Strict process deadline: $processTimeoutMilliseconds ms after Process.Start, followed only by bounded shutdown waits."
    if ($StaticDryRun) {
        $expectedLaunchVariables = [ordered]@{
            VOXEL_NATIVE_QA = '1'
            VOXEL_NATIVE_QA_FOCUS = $Focus
            VOXEL_NATIVE_QA_CAMERA_ROUTE_POLICY = 'preflight-v1'
            VOXEL_NATIVE_QA_PROFILE = $RouteProfile
            VOXEL_NATIVE_QA_SEED = [string]$Seed
            VOXEL_NATIVE_QA_SCENERY = $Scenery
            VOXEL_NATIVE_QA_TERRAIN_GRAMMAR = $TerrainGrammar
            VOXEL_NATIVE_PLANETARY_STREAMING = 'all'
            VOXEL_NATIVE_QA_DISTANCE_KM = [string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0:0.###}', $DistanceKm)
            VOXEL_NATIVE_QA_SECONDS = [string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0:0.###}', $Seconds)
            VOXEL_NATIVE_QA_SCREENSHOT_INTERVAL = [string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0:0.###}', $ScreenshotInterval)
            VOXEL_NATIVE_QA_WORLD = $worldName
            VOXEL_NATIVE_FAR_SURFACE_MATERIAL = $SurfaceMaterial
            VOXEL_NATIVE_FAR_HYDROGRAPHY = $Hydro
            VOXEL_NATIVE_FAR_SEMANTIC_COHORTS = $Cohorts
            VOXEL_NATIVE_FAR_L0_HEIGHT_MODE = $L0HeightMode
            VOXEL_NATIVE_QA_GIT_SHA = $provenance.GitSha
            VOXEL_NATIVE_QA_GIT_DIRTY = if ($provenance.GitDirty) { 'true' } else { 'false' }
            VOXEL_NATIVE_QA_SOURCE_FINGERPRINT = $provenance.SourceFingerprint
            VOXEL_NATIVE_QA_EXECUTABLE_HASH = $provenance.ExecutableHash
            VOXEL_NATIVE_QA_TOOLCHAIN = $provenance.Toolchain
            VOXEL_NATIVE_QA_HARDWARE = $provenance.Hardware
            VOXEL_NATIVE_QA_HOUR = [string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0:0.###}', $Hour)
            VOXEL_NATIVE_WINDOW_WIDTH = [string]$Width
            VOXEL_NATIVE_WINDOW_HEIGHT = [string]$Height
            VOXEL_NATIVE_INSTANCE_LABEL = $instanceLabel
        }
        foreach ($variableName in $expectedLaunchVariables.Keys) {
            $actualValue = $startInfo.EnvironmentVariables[$variableName]
            if (-not ([string]$actualValue).Equals(
                    [string]$expectedLaunchVariables[$variableName],
                    [StringComparison]::Ordinal)) {
                throw "Static dry run found an incorrect launch value for $variableName."
            }
        }
        $actualVoxelVariables = @($startInfo.EnvironmentVariables.Keys |
            Where-Object { ([string]$_).StartsWith('VOXEL_NATIVE_', [StringComparison]::OrdinalIgnoreCase) })
        if ($actualVoxelVariables.Count -ne $expectedLaunchVariables.Count) {
            throw 'Static dry run found an unexpected inherited VOXEL_NATIVE_ launch variable.'
        }
        Assert-QaControlledArtifactsUnchanged `
            -Expected $provenance `
            -Root $projectRoot `
            -ExecutablePath $executable `
            -Boundary 'static-preflight'
        Write-Host "STATIC MATRIX: profile=$RouteProfile focus=$Focus grammar=$TerrainGrammar surface=$SurfaceMaterial hydro=$Hydro cohorts=$Cohorts l0_height=$L0HeightMode process_timeout_ms=$processTimeoutMilliseconds world=$worldName instance=$instanceLabel"
        Write-Host "STATIC DRY RUN: launch environment validated for $RouteProfile $Focus; Process.Start was not called."
        return
    }

    $qaRunsRoot = Join-Path $projectRoot 'qa_runs'
    $reportsBefore = [System.Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    foreach ($reportPath in @(Get-BoundedQaReportPaths -QaRunsRoot $qaRunsRoot)) {
        [void]$reportsBefore.Add($reportPath)
    }
    $persistentSettingsPath = Join-Path $projectRoot 'voxel-native-save.ron'
    $persistentSettingsBefore = Get-QaOptionalFileIdentity `
        -Path $persistentSettingsPath `
        -MaxBytes $MaxPersistentSettingsBytes `
        -Label 'Persistent settings file'
    $process = $null
    $processStarted = $false
    $processExitCode = $null
    $processExitObserved = $false
    $processTimedOut = $false
    $processTimeoutFailure = $null
    $lifecycleFailures = [System.Collections.Generic.List[string]]::new()
    $safetyFailures = [System.Collections.Generic.List[string]]::new()
    $executableReadLock = Open-QaExecutableReadLock `
        -Path $executable `
        -MaxBytes $MaxExecutableBytes
    try {
        Assert-QaControlledArtifactsUnchanged `
            -Expected $provenance `
            -Root $projectRoot `
            -ExecutablePath $executable `
            -Boundary 'locked-immediate-pre-launch'
        $process = [System.Diagnostics.Process]::Start($startInfo)
        if ($null -eq $process) {
            throw 'Process.Start returned no QA process handle.'
        }
        $processStarted = $true
        $processExitObserved = $process.WaitForExit($processTimeoutMilliseconds)
        if (-not $processExitObserved) {
            $processTimedOut = $true
            Write-Warning "$RouteProfile $Focus QA exceeded its strict $processTimeoutMilliseconds ms wall-clock deadline; requesting bounded shutdown."
            $shutdown = Stop-QaProcessBounded -Process $process
            $processExitObserved = [bool]$shutdown.ExitObserved
            $shutdownSummary = "graceful_requested=$($shutdown.GracefulRequested), forced_kill_requested=$($shutdown.ForcedKillRequested), exit_observed=$($shutdown.ExitObserved)"
            if ($shutdown.Errors.Count -gt 0) {
                $shutdownSummary += ", shutdown_errors=$($shutdown.Errors -join '; ')"
            }
            $processTimeoutFailure = "$RouteProfile $Focus QA exceeded the strict $processTimeoutMilliseconds ms wall-clock deadline ($shutdownSummary). Timed-out artifacts are never accepted as QA evidence."
        }
        if ($processExitObserved) {
            $processExitCode = $process.ExitCode
        }
        if ($processExitObserved) {
            $postProcessBoundary = if ($processTimedOut) {
                'locked-immediate-post-timeout-shutdown'
            }
            else {
                'locked-immediate-post-exit'
            }
            Assert-QaControlledArtifactsUnchanged `
                -Expected $provenance `
                -Root $projectRoot `
                -ExecutablePath $executable `
                -Boundary $postProcessBoundary
        }
    }
    catch {
        $lifecycleFailures.Add("process lifecycle exception: $($_.Exception.Message)")
        if ($null -ne $process -and -not $processExitObserved) {
            try {
                $emergencyShutdown = Stop-QaProcessBounded -Process $process
                $processExitObserved = [bool]$emergencyShutdown.ExitObserved
                if ($emergencyShutdown.Errors.Count -gt 0) {
                    $lifecycleFailures.Add(
                        "emergency shutdown diagnostics: $($emergencyShutdown.Errors -join '; ')"
                    )
                }
            }
            catch {
                $lifecycleFailures.Add("emergency shutdown exception: $($_.Exception.Message)")
            }
        }
    }
    finally {
        try {
            if ($processExitObserved -and $null -eq $processExitCode -and $null -ne $process) {
                try {
                    $processExitCode = $process.ExitCode
                }
                catch {
                    $lifecycleFailures.Add("process exit-code read failed: $($_.Exception.Message)")
                }
            }

            if (-not $processStarted -or $processExitObserved) {
                try {
                    Assert-QaOptionalFileUnchanged `
                        -Expected $persistentSettingsBefore `
                        -Path $persistentSettingsPath `
                        -MaxBytes $MaxPersistentSettingsBytes `
                        -Label 'Persistent settings file'
                }
                catch {
                    $safetyFailures.Add("persistent settings endpoint check: $($_.Exception.Message)")
                }
            }
            else {
                $safetyFailures.Add(
                    'persistent settings endpoint identity is unprovable because QA process exit was not observed; the engine may still be running'
                )
            }
        }
        finally {
            if ($null -ne $process) {
                try {
                    $process.Dispose()
                }
                catch {
                    $safetyFailures.Add("process-handle disposal failed: $($_.Exception.Message)")
                }
            }
            try {
                $executableReadLock.Dispose()
            }
            catch {
                $safetyFailures.Add("executable-lock disposal failed: $($_.Exception.Message)")
            }
        }
    }
    if ($processTimedOut) {
        $lifecycleFailures.Add($processTimeoutFailure)
    }
    if ($processStarted -and -not $processExitObserved) {
        $lifecycleFailures.Add(
            "$RouteProfile $Focus QA process exit was not observed within bounded waits."
        )
    }
    if ($processExitObserved -and $null -eq $processExitCode) {
        $lifecycleFailures.Add("$RouteProfile $Focus QA exit code is unavailable.")
    }
    elseif ($null -ne $processExitCode -and $processExitCode -ne 0) {
        $lifecycleFailures.Add("$RouteProfile $Focus QA exited with code $processExitCode.")
    }
    if ($lifecycleFailures.Count -gt 0 -or $safetyFailures.Count -gt 0) {
        $failureGroups = [System.Collections.Generic.List[string]]::new()
        if ($lifecycleFailures.Count -gt 0) {
            $failureGroups.Add("lifecycle=[$($lifecycleFailures -join ' | ')]")
        }
        if ($safetyFailures.Count -gt 0) {
            $failureGroups.Add("safety=[$($safetyFailures -join ' | ')]")
        }
        throw "QA route failed: $($failureGroups -join '; ')"
    }

    $newReportPaths = @(Get-BoundedQaReportPaths -QaRunsRoot $qaRunsRoot |
        Where-Object { -not $reportsBefore.Contains($_) })
    if ($newReportPaths.Count -ne 1) {
        throw "$RouteProfile $Focus QA exited successfully but produced $($newReportPaths.Count) new reports; exactly one identity-bound report is required."
    }
    $reportPath = $newReportPaths[0]
    Assert-QaReportIdentity `
        -ReportPath $reportPath `
        -QaRunsRoot $qaRunsRoot `
        -WorldName $worldName `
        -InstanceLabel $instanceLabel `
        -RouteProfile $RouteProfile `
        -RouteFocus $Focus `
        -WorldSeed $Seed `
        -SceneryMode $Scenery `
        -SurfaceMode $SurfaceMaterial `
        -HydroMode $Hydro `
        -CohortMode $Cohorts `
        -TerrainGrammarMode $TerrainGrammar `
        -L0HeightModeValue $L0HeightMode `
        -ExpectedProvenance $provenance `
        -ExpectedBuildProfile $targetFolder
    Write-Host "Completed $RouteProfile $Focus report: $reportPath"
}

Assert-QaRunnerStaticFixtures
Assert-QaReportParserFixtures

foreach ($routeProfile in $profiles) {
    Invoke-StreamingRoute -RouteProfile $routeProfile
}

if ($StaticDryRun) {
    Write-Host 'Planetary streaming QA static dry run complete. No build or engine process was started.'
}
else {
    Write-Host 'Planetary streaming QA complete. No worlds or prior QA evidence were removed.'
}
