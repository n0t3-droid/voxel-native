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

    [ValidateSet('bridge-v2', 'bridge-v1', 'legacy')]
    [string]$SurfaceMaterial = 'bridge-v2',

    [ValidateSet('v1', 'off')]
    [string]$Hydro = 'v1',

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

    return [pscustomobject]@{
        Length = $length
        Hex = ConvertTo-LowerHex -Bytes $digest
    }
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

function Get-QaProvenance {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,

        [Parameter(Mandatory = $true)]
        [string]$ExecutablePath
    )

    $git = Get-GitProvenance -Root $Root
    $source = Get-SourceFingerprint -Root $Root
    $executableDigest = Get-BoundedFileSha256 `
        -Path $ExecutablePath `
        -MaxBytes $MaxExecutableBytes `
        -Label 'QA executable'
    if ($executableDigest.Hex -notmatch '^[0-9a-f]{64}$') {
        throw 'Executable provenance did not produce a canonical SHA-256 digest.'
    }
    $toolchain = Get-RustcToolchainProvenance
    $hardware = Get-HardwareProvenance

    return [pscustomobject]@{
        GitSha = $git.Sha
        GitDirty = [bool]$git.Dirty
        SourceFingerprint = $source.Token
        SourceFileCount = $source.FileCount
        SourceBytes = $source.TotalBytes
        ExecutableHash = "sha256:$($executableDigest.Hex)"
        ExecutableBytes = $executableDigest.Length
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

function Invoke-StreamingRoute {
    param([string]$RouteProfile)

    Write-Host "Collecting bounded provenance before the $RouteProfile route."
    $provenance = Get-QaProvenance -Root $projectRoot -ExecutablePath $executable
    Write-QaProvenance -Provenance $provenance

    $epoch = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $worldName = "qa_streaming_${RouteProfile}_seed${Seed}_${Scenery}_${SurfaceMaterial}_hydro-${Hydro}_${epoch}"
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $executable
    $startInfo.WorkingDirectory = $projectRoot
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $false
    $startInfo.Arguments = '--qa'
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA'] = '1'
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_FOCUS'] = 'streaming'
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_PROFILE'] = $RouteProfile
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_SEED'] = [string]$Seed
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_QA_SCENERY'] = $Scenery
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
    $startInfo.EnvironmentVariables['VOXEL_NATIVE_INSTANCE_LABEL'] = "STREAMING QA // $($RouteProfile.ToUpperInvariant()) // SEED $Seed // $($Scenery.ToUpperInvariant()) // SURFACE $($SurfaceMaterial.ToUpperInvariant()) // HYDRO $($Hydro.ToUpperInvariant()) // ${DistanceKm}KM"

    Write-Host "Starting $RouteProfile streaming route: seed $Seed, scenery $Scenery, surface $SurfaceMaterial, hydro $Hydro, $DistanceKm km, $Seconds s, ${Width}x${Height}."
    Write-Host "Isolated world: $worldName"
    if ($StaticDryRun) {
        $requiredProvenanceVariables = @(
            'VOXEL_NATIVE_QA_GIT_SHA',
            'VOXEL_NATIVE_QA_GIT_DIRTY',
            'VOXEL_NATIVE_QA_SOURCE_FINGERPRINT',
            'VOXEL_NATIVE_QA_EXECUTABLE_HASH',
            'VOXEL_NATIVE_QA_TOOLCHAIN',
            'VOXEL_NATIVE_QA_HARDWARE',
            'VOXEL_NATIVE_FAR_SURFACE_MATERIAL',
            'VOXEL_NATIVE_FAR_HYDROGRAPHY'
        )
        foreach ($variableName in $requiredProvenanceVariables) {
            if ([string]::IsNullOrWhiteSpace($startInfo.EnvironmentVariables[$variableName])) {
                throw "Static dry run found missing launch variable $variableName."
            }
        }
        Write-Host "STATIC DRY RUN: launch environment validated for $RouteProfile; Process.Start was not called."
        return
    }

    $startedAt = [DateTime]::UtcNow
    $process = [System.Diagnostics.Process]::Start($startInfo)
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) {
        throw "$RouteProfile streaming QA exited with code $($process.ExitCode)"
    }

    $report = Get-ChildItem -LiteralPath (Join-Path $projectRoot 'qa_runs') -Filter 'report.ron' -File -Recurse |
        Where-Object { $_.LastWriteTimeUtc -ge $startedAt } |
        Sort-Object LastWriteTimeUtc -Descending |
        Select-Object -First 1
    if ($null -eq $report) {
        throw "$RouteProfile streaming QA exited successfully but produced no report after launch."
    }
    Write-Host "Completed $RouteProfile report: $($report.FullName)"
}

$profiles = if ($Profile -eq 'both') { @('natural', 'astral') } else { @($Profile) }
foreach ($routeProfile in $profiles) {
    Invoke-StreamingRoute -RouteProfile $routeProfile
}

if ($StaticDryRun) {
    Write-Host 'Planetary streaming QA static dry run complete. No build or engine process was started.'
}
else {
    Write-Host 'Planetary streaming QA complete. No worlds or prior QA evidence were removed.'
}
