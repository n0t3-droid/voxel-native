<#
.SYNOPSIS
Starts one persistent, visible, isolated Voxel-Native observer session.

.DESCRIPTION
Creates a unique agent_runs/live_observer_<epoch> directory and a matching
unique world, then starts the release engine with file-based agent control.
The launcher never reuses or deletes a session or world and refuses to start
while another voxel-native process is running.

The engine stays open until its control file requests `exit: true` with a new
sequence (or the user closes the window). The launcher returns after startup by
default so the calling shell remains available. Use -Wait to stay attached to
the process lifetime.

.EXAMPLE
.\scripts\live-observer.ps1 -Profile natural -Focus river -Hour 15.65

.EXAMPLE
.\scripts\live-observer.ps1 -Profile astral -Focus spawn -Wait

.EXAMPLE
.\scripts\live-observer.ps1 -DryRun
#>
[CmdletBinding()]
param(
    [ValidateSet('natural', 'astral')]
    [string]$Profile = 'natural',

    [ValidateSet('river', 'spawn')]
    [string]$Focus = 'river',

    [ValidateSet('off', 'lean', 'balanced', 'lush')]
    [string]$Scenery = 'lush',

    [uint32]$Seed = 12345,

    [ValidateRange(0.0, 24.0)]
    [double]$Hour = 15.65,

    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9_-]{0,47}$')]
    [string]$WorldPrefix = 'live_observer',

    [ValidateRange(10, 300)]
    [int]$ReadinessTimeoutSeconds = 120,

    [ValidateRange(960, 3840)]
    [int]$Width = 1600,

    [ValidateRange(540, 2160)]
    [int]$Height = 900,

    [switch]$SemanticCohorts,
    [switch]$Wait,
    [switch]$DryRun,
    [switch]$PathSafetySelfTest
)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$targetRoot = Join-Path $projectRoot 'target'
$releaseRoot = Join-Path $targetRoot 'release'
$executable = Join-Path $releaseRoot 'voxel-native.exe'
$agentRunsRoot = Join-Path $projectRoot 'agent_runs'
$savesRoot = Join-Path $projectRoot 'saves'
$requiredObserverProtocol = 'live-observer-v1'

function Assert-SafeObserverExistingAncestorChain {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $cursor = [System.IO.Path]::GetFullPath($Path)
    $volumeRoot = [System.IO.Path]::GetPathRoot($cursor)
    if (-not $cursor.Equals($volumeRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        $cursor = $cursor.TrimEnd(
            [System.IO.Path]::DirectorySeparatorChar,
            [System.IO.Path]::AltDirectorySeparatorChar
        )
    }
    $ancestors = [System.Collections.Generic.List[string]]::new()
    while (-not [string]::IsNullOrEmpty($cursor)) {
        $ancestors.Add($cursor)
        $parent = [System.IO.Path]::GetDirectoryName($cursor)
        if ([string]::IsNullOrEmpty($parent) -or
            $parent.Equals($cursor, [System.StringComparison]::OrdinalIgnoreCase)) {
            break
        }
        $cursor = $parent
    }

    for ($index = $ancestors.Count - 1; $index -ge 0; $index--) {
        $ancestor = $ancestors[$index]
        $item = Get-Item -LiteralPath $ancestor -Force -ErrorAction SilentlyContinue
        if ($null -eq $item) {
            # A missing requested leaf is handled by the repository-bounded
            # creator below. Every existing ancestor still gets checked.
            continue
        }
        if (-not $item.PSIsContainer -or
            ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Observer directory chain contains a file, symlink, or junction: $ancestor"
        }
    }
}

function Assert-SafeObserverDirectoryChain {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [switch]$Create,
        [switch]$AllowMissing
    )

    $rootFull = [System.IO.Path]::GetFullPath($projectRoot).TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
    $targetFull = [System.IO.Path]::GetFullPath($Path).TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
    $rootPrefix = $rootFull + [System.IO.Path]::DirectorySeparatorChar
    if (-not $targetFull.Equals($rootFull, [System.StringComparison]::OrdinalIgnoreCase) -and
        -not $targetFull.StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Observer directory escapes the repository boundary: $targetFull"
    }

    # The repository itself may sit beneath a redirected parent. Validate the
    # complete native chain from the volume root before checking/creating any
    # repository-owned descendant.
    Assert-SafeObserverExistingAncestorChain -Path $rootFull

    $directories = [System.Collections.Generic.List[string]]::new()
    $directories.Add($rootFull)
    $relative = $targetFull.Substring($rootFull.Length).TrimStart(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
    $cursor = $rootFull
    if (-not [string]::IsNullOrEmpty($relative)) {
        foreach ($component in ($relative -split '[\\/]')) {
            if ([string]::IsNullOrEmpty($component)) {
                continue
            }
            $cursor = Join-Path $cursor $component
            $directories.Add($cursor)
        }
    }

    foreach ($directory in $directories) {
        $item = Get-Item -LiteralPath $directory -Force -ErrorAction SilentlyContinue
        if ($null -eq $item) {
            if ($Create) {
                [System.IO.Directory]::CreateDirectory($directory) | Out-Null
                $item = Get-Item -LiteralPath $directory -Force -ErrorAction Stop
            }
            elseif ($AllowMissing) {
                continue
            }
            else {
                throw "Required observer directory is missing: $directory"
            }
        }
        if (-not $item.PSIsContainer -or
            ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Observer directory chain contains a file, symlink, or junction: $directory"
        }
    }
}

function Assert-SafeObserverRegularFileOrMissing {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [switch]$RequireExisting
    )

    $fullPath = [System.IO.Path]::GetFullPath($Path)
    $parent = [System.IO.Path]::GetDirectoryName($fullPath)
    Assert-SafeObserverDirectoryChain -Path $parent
    $item = Get-Item -LiteralPath $fullPath -Force -ErrorAction SilentlyContinue
    if ($null -eq $item) {
        if ($RequireExisting) {
            throw "Required observer file is missing: $fullPath"
        }
        return
    }
    if ($item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Observer path is not a safe regular file: $fullPath"
    }
}

function Get-RunningVoxelNativeProcesses {
    return @(Get-Process -Name 'voxel-native' -ErrorAction SilentlyContinue)
}

function Assert-NoRunningVoxelNativeProcess {
    $running = @(Get-RunningVoxelNativeProcesses)
    if ($running.Count -eq 0) {
        return
    }

    $processes = ($running | Sort-Object Id | ForEach-Object {
        $path = '<path unavailable>'
        try {
            if (-not [string]::IsNullOrWhiteSpace($_.Path)) {
                $path = $_.Path
            }
        }
        catch {
            # A process path can be inaccessible without elevation. Its PID is
            # still sufficient to fail closed and prevent a second GPU window.
        }
        "PID $($_.Id) [$path]"
    }) -join ', '

    throw "Refusing to start a second voxel-native engine. Close the existing process first: $processes"
}

function Enter-LiveObserverLaunchMutex {
    $mutex = [System.Threading.Mutex]::new(
        $false,
        'VoxelNative.LiveObserver.Launch.v1'
    )
    $acquired = $false
    try {
        try {
            $acquired = $mutex.WaitOne(0)
        }
        catch [System.Threading.AbandonedMutexException] {
            # WaitOne grants ownership when it reports an abandoned mutex.
            $acquired = $true
        }
        if (-not $acquired) {
            throw 'Another live-observer launcher is already inside the guarded launch section.'
        }
        return $mutex
    }
    catch {
        if (-not $acquired) {
            $mutex.Dispose()
        }
        throw
    }
}

function Read-BoundedObserverFile {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $fullPath = [System.IO.Path]::GetFullPath($Path)
    Assert-SafeObserverDirectoryChain -Path ([System.IO.Path]::GetDirectoryName($fullPath))
    try {
        $item = Get-Item -LiteralPath $fullPath -Force -ErrorAction Stop
        if ($item.PSIsContainer -or
            ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
            $item.Length -le 0 -or
            $item.Length -gt 1MB) {
            return $null
        }
        return [System.IO.File]::ReadAllText($item.FullName)
    }
    catch {
        # Atomic replacement can make a particular polling instant unavailable.
        # Readiness remains false and the bounded loop tries again.
        return $null
    }
}

function Wait-LiveObserverReady {
    param(
        [Parameter(Mandatory)]
        [System.Diagnostics.Process]$Process,

        [Parameter(Mandatory)]
        $Plan,

        [Parameter(Mandatory)]
        [int]$TimeoutSeconds
    )

    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $Process.Refresh()
        if ($Process.HasExited) {
            throw "The live observer exited during startup with code $($Process.ExitCode)."
        }

        $controlText = Read-BoundedObserverFile -Path $Plan.ControlFile
        $bridgeText = Read-BoundedObserverFile -Path (Join-Path $Plan.SessionDirectory 'bridge.ron')
        $statusText = Read-BoundedObserverFile -Path $Plan.StatusFile
        $bridgeReady = $null -ne $bridgeText -and
            $bridgeText -match ('(?m)^\s*observer_protocol_version:\s*"' + [regex]::Escape($requiredObserverProtocol) + '",?\s*$') -and
            $bridgeText -match '(?m)^\s*isolated_observer:\s*true,?\s*$' -and
            $bridgeText -match '(?m)^\s*runtime_enabled:\s*true,?\s*$'
        $statusReady = $null -ne $statusText -and
            $statusText -match '(?m)^\s*game_state:\s*"InGame",?\s*$' -and
            $statusText -match '(?m)^\s*control_enabled:\s*true,?\s*$'
        if ($null -ne $controlText -and $bridgeReady -and $statusReady) {
            return
        }
        Start-Sleep -Milliseconds 100
    }

    throw "Live observer did not publish protocol '$requiredObserverProtocol', explicit isolation, a non-empty control file, and InGame status within $TimeoutSeconds seconds."
}

function Stop-IncompleteLiveObserver {
    param(
        [Parameter(Mandatory)]
        [System.Diagnostics.Process]$Process
    )

    try {
        $Process.Refresh()
        if (-not $Process.HasExited) {
            [void]$Process.CloseMainWindow()
            if (-not $Process.WaitForExit(5000)) {
                $Process.Kill()
                if (-not $Process.WaitForExit(5000)) {
                    throw "termination was not observed within five seconds after Kill()"
                }
            }
        }
        $Process.Refresh()
        if (-not $Process.HasExited) {
            throw 'the process still reports itself as running'
        }
    }
    catch {
        throw "Could not confirm termination of incomplete live observer PID $($Process.Id): $($_.Exception.Message)"
    }
}

function Invoke-ReleaseBuild {
    Assert-SafeObserverDirectoryChain -Path $projectRoot
    Assert-SafeObserverDirectoryChain -Path $targetRoot -AllowMissing
    Assert-SafeObserverDirectoryChain -Path $releaseRoot -AllowMissing
    Push-Location $projectRoot
    try {
        # The explicit target directory prevents an inherited Cargo setting
        # from redirecting mandatory build writes outside the validated tree.
        & cargo build --target-dir $targetRoot --release --bin voxel-native
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build --release failed with exit code $LASTEXITCODE"
        }
        Assert-SafeObserverDirectoryChain -Path $targetRoot
        Assert-SafeObserverDirectoryChain -Path $releaseRoot
        Assert-SafeObserverRegularFileOrMissing -Path $executable -RequireExisting
    }
    finally {
        Pop-Location
    }
}

function Test-WorldStorageClaim {
    param(
        [Parameter(Mandatory)]
        [string]$WorldName
    )

    Assert-SafeObserverDirectoryChain -Path $savesRoot -AllowMissing
    foreach ($candidate in @(
        (Join-Path $savesRoot "$WorldName.ron"),
        (Join-Path $savesRoot "$WorldName.world2"),
        (Join-Path $savesRoot "$WorldName.world3"),
        # Retain the two early-development spellings as conservative claims:
        # an unexpected legacy artifact must never be silently reused either.
        (Join-Path $savesRoot "$WorldName.v2.ron"),
        (Join-Path $savesRoot "$WorldName.v3.ron"),
        (Join-Path $savesRoot "${WorldName}_edits"),
        (Join-Path $savesRoot "${WorldName}_bots"),
        (Join-Path $savesRoot "${WorldName}_city")
    )) {
        if (Test-Path -LiteralPath $candidate) {
            return $true
        }
    }

    return $false
}

function New-LiveObserverSessionPlan {
    param(
        [Parameter(Mandatory)]
        [bool]$CreateDirectory
    )

    Assert-SafeObserverDirectoryChain `
        -Path $agentRunsRoot `
        -Create:$CreateDirectory `
        -AllowMissing:(-not $CreateDirectory)
    Assert-SafeObserverDirectoryChain `
        -Path $savesRoot `
        -Create:$CreateDirectory `
        -AllowMissing:(-not $CreateDirectory)

    $epoch = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    for ($attempt = 0; $attempt -lt 4096; $attempt++) {
        $candidateEpoch = $epoch + $attempt
        $sessionDirectory = Join-Path $agentRunsRoot "live_observer_$candidateEpoch"
        $worldName = "${WorldPrefix}_$candidateEpoch"

        if ((Test-Path -LiteralPath $sessionDirectory) -or
            (Test-WorldStorageClaim -WorldName $worldName)) {
            continue
        }

        if ($CreateDirectory) {
            try {
                # The path is constructed solely from the fixed session prefix
                # and an integer epoch, so wildcard interpretation is absent.
                New-Item -ItemType Directory -Path $sessionDirectory -ErrorAction Stop |
                    Out-Null
                Assert-SafeObserverDirectoryChain -Path $sessionDirectory
            }
            catch {
                if (Test-Path -LiteralPath $sessionDirectory) {
                    continue
                }
                throw
            }
        }

        return [pscustomobject]@{
            Epoch = $candidateEpoch
            WorldName = $worldName
            SessionDirectory = $sessionDirectory
            ControlFile = Join-Path $sessionDirectory 'agent_control.ron'
            StatusFile = Join-Path $sessionDirectory 'status.ron'
            SessionMetadataFile = Join-Path $sessionDirectory 'observer-session.json'
            ScreenshotPattern = Join-Path $sessionDirectory 'live_*.png'
        }
    }

    throw 'Could not reserve a unique live-observer session after 4096 attempts.'
}

function Start-LiveObserverProcess {
    param(
        [Parameter(Mandatory)]
        [hashtable]$Environment
    )

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $executable
    $startInfo.WorkingDirectory = $projectRoot
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $false
    $startInfo.Arguments = '--agent-control'

    # A persistent observer must never inherit an unrelated peer session. The
    # live-link parser intentionally rejects blank values, so remove inherited
    # variables entirely instead of attempting to neutralize them with ''.
    foreach ($liveLinkVariable in @(
        'VOXEL_NATIVE_LIVE_LINK_SIDE',
        'VOXEL_NATIVE_LIVE_LINK_BIND',
        'VOXEL_NATIVE_LIVE_LINK_PEER'
    )) {
        $startInfo.EnvironmentVariables.Remove($liveLinkVariable)
    }

    foreach ($entry in $Environment.GetEnumerator()) {
        $startInfo.EnvironmentVariables[$entry.Key] = [string]$entry.Value
    }

    $process = [System.Diagnostics.Process]::Start($startInfo)
    if ($null -eq $process) {
        throw 'The operating system did not return a process for the live observer.'
    }
    return $process
}

function Write-NewUtf8File {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$Content
    )

    Assert-SafeObserverRegularFileOrMissing -Path $Path
    $encoding = [System.Text.UTF8Encoding]::new($false)
    $stream = [System.IO.File]::Open(
        $Path,
        [System.IO.FileMode]::CreateNew,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::Read
    )
    try {
        $writer = [System.IO.StreamWriter]::new($stream, $encoding)
        try {
            $writer.Write($Content)
            $writer.Flush()
        }
        finally {
            $writer.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

function Invoke-LiveObserverPathSafetySelfTest {
    $tmpRoot = Join-Path $projectRoot 'tmp'
    Assert-SafeObserverDirectoryChain -Path $tmpRoot -Create
    $testName = "live-observer-path-test-$PID-$([Guid]::NewGuid().ToString('N'))"
    $testRoot = Join-Path $tmpRoot $testName
    $target = Join-Path $testRoot 'target'
    $junction = Join-Path $testRoot 'agent_runs'
    $buildJunction = Join-Path $testRoot 'build-target'
    $ancestorJunction = Join-Path $testRoot 'redirected-parent'
    [System.IO.Directory]::CreateDirectory($target) | Out-Null

    try {
        Assert-SafeObserverDirectoryChain -Path $target
        New-Item -ItemType Junction -Path $junction -Target $target -ErrorAction Stop |
            Out-Null
        $rejected = $false
        try {
            Assert-SafeObserverDirectoryChain `
                -Path (Join-Path $junction 'session') `
                -AllowMissing
        }
        catch {
            if ($_.Exception.Message -notmatch 'symlink, or junction') {
                throw
            }
            $rejected = $true
        }
        if (-not $rejected) {
            throw 'Path safety self-test accepted a junction ancestor.'
        }

        New-Item -ItemType Junction -Path $buildJunction -Target $target -ErrorAction Stop |
            Out-Null
        $rejected = $false
        try {
            Assert-SafeObserverDirectoryChain `
                -Path (Join-Path $buildJunction 'release') `
                -AllowMissing
        }
        catch {
            if ($_.Exception.Message -notmatch 'symlink, or junction') {
                throw
            }
            $rejected = $true
        }
        if (-not $rejected) {
            throw 'Path safety self-test accepted a target/build-chain junction.'
        }

        New-Item -ItemType Junction -Path $ancestorJunction -Target $target -ErrorAction Stop |
            Out-Null
        $rejected = $false
        try {
            Assert-SafeObserverExistingAncestorChain `
                -Path (Join-Path $ancestorJunction 'repository\session')
        }
        catch {
            if ($_.Exception.Message -notmatch 'symlink, or junction') {
                throw
            }
            $rejected = $true
        }
        if (-not $rejected) {
            throw 'Path safety self-test accepted a junction above an anchor path.'
        }
        Write-Host 'LIVE OBSERVER PATH SAFETY SELF-TEST PASSED.'
    }
    finally {
        foreach ($fixtureJunction in @($junction, $buildJunction, $ancestorJunction)) {
            if (Test-Path -LiteralPath $fixtureJunction) {
                $junctionItem = Get-Item -LiteralPath $fixtureJunction -Force
                if (($junctionItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0) {
                    throw "Refusing self-test cleanup because the expected junction changed: $fixtureJunction"
                }
                Remove-Item -LiteralPath $fixtureJunction -Force
            }
        }
        $fullTestRoot = [System.IO.Path]::GetFullPath($testRoot)
        $tmpPrefix = [System.IO.Path]::GetFullPath($tmpRoot).TrimEnd(
            [System.IO.Path]::DirectorySeparatorChar,
            [System.IO.Path]::AltDirectorySeparatorChar
        ) + [System.IO.Path]::DirectorySeparatorChar
        if (-not $fullTestRoot.StartsWith(
                $tmpPrefix,
                [System.StringComparison]::OrdinalIgnoreCase
            ) -or
            -not ([System.IO.Path]::GetFileName($fullTestRoot)).StartsWith(
                'live-observer-path-test-',
                [System.StringComparison]::Ordinal
            )) {
            throw "Refusing broad path-safety fixture cleanup: $fullTestRoot"
        }
        if ([System.IO.Directory]::Exists($fullTestRoot)) {
            [System.IO.Directory]::Delete($fullTestRoot, $true)
        }
    }
}

$selfTestRequested = $PathSafetySelfTest.IsPresent
if ($selfTestRequested) {
    Invoke-LiveObserverPathSafetySelfTest
    return
}

$launchMutex = Enter-LiveObserverLaunchMutex
try {
Assert-SafeObserverDirectoryChain -Path $projectRoot
Assert-SafeObserverDirectoryChain -Path $targetRoot -AllowMissing
Assert-SafeObserverDirectoryChain -Path $releaseRoot -AllowMissing
Assert-NoRunningVoxelNativeProcess

if (-not $DryRun) {
    Invoke-ReleaseBuild
}

if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    if ($DryRun) {
        Write-Warning "Release executable is absent; a real launch would build it: $executable"
    }
    else {
        throw "Release executable was not produced by the mandatory release build: $executable"
    }
}

Assert-NoRunningVoxelNativeProcess

$plan = New-LiveObserverSessionPlan -CreateDirectory (-not $DryRun)
$executableHash = $null
$executableBytes = $null
$executableModifiedUtc = $null
if (Test-Path -LiteralPath $executable -PathType Leaf) {
    Assert-SafeObserverRegularFileOrMissing -Path $executable -RequireExisting
    $executableItem = Get-Item -LiteralPath $executable
    $executableHash = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash
    $executableBytes = $executableItem.Length
    $executableModifiedUtc = $executableItem.LastWriteTimeUtc.ToString('o')
}

$environment = @{
    'VOXEL_NATIVE_AGENT_CONTROL' = '1'
    'VOXEL_NATIVE_AGENT_ISOLATED' = '1'
    'VOXEL_NATIVE_AGENT_NO_AUTO_ENTER' = '0'
    'VOXEL_NATIVE_AGENT_CONTROL_FILE' = $plan.ControlFile
    'VOXEL_NATIVE_AGENT_SESSION_DIR' = $plan.SessionDirectory
    'VOXEL_NATIVE_AGENT_WORLD' = $plan.WorldName
    'VOXEL_NATIVE_AGENT_PROFILE' = $Profile
    'VOXEL_NATIVE_AGENT_FOCUS' = $Focus
    'VOXEL_NATIVE_AGENT_SCENERY' = $Scenery
    'VOXEL_NATIVE_AGENT_SEED' = $Seed
    'VOXEL_NATIVE_AGENT_HOUR' = $Hour.ToString([System.Globalization.CultureInfo]::InvariantCulture)
    # Persistent observers use explicit one-shot screenshot commands only.
    # A periodic interval would have unbounded disk growth over an unbounded run.
    'VOXEL_NATIVE_AGENT_SCREENSHOT_INTERVAL' = '0'
    'VOXEL_NATIVE_PLANETARY_STREAMING' = 'all'
    'VOXEL_NATIVE_FAR_SURFACE_MATERIAL' = 'bridge-v2'
    'VOXEL_NATIVE_FAR_HYDROGRAPHY' = 'v1'
    'VOXEL_NATIVE_FAR_SEMANTIC_COHORTS' = if ($SemanticCohorts) { 'silhouettes-v1' } else { 'off' }
    'VOXEL_NATIVE_FAR_L0_HEIGHT_MODE' = 'cardinal-trimmed-8-v1'
    'VOXEL_NATIVE_AGENT_ID' = "live-observer-$($plan.Epoch)"
    'VOXEL_NATIVE_AGENT_NAME' = 'LIVE OBSERVER'
    'VOXEL_NATIVE_AGENT_ROLE' = 'VISUAL CHANGE OBSERVER'
    'VOXEL_NATIVE_AGENT_TASK' = 'Persistent visual implementation tour'
    'VOXEL_NATIVE_INSTANCE_LABEL' = "LIVE OBSERVER // $($Profile.ToUpperInvariant()) // $($Focus.ToUpperInvariant())"
    'VOXEL_NATIVE_WINDOW_WIDTH' = [string]$Width
    'VOXEL_NATIVE_WINDOW_HEIGHT' = [string]$Height
    'VOXEL_NATIVE_QA' = '0'
    'VOXEL_NATIVE_MISSION_CONTROL' = '0'
    'VOXEL_NATIVE_MISSION_FEED' = '0'
}

$sessionInfo = [ordered]@{
    Mode = if ($DryRun) { 'dry-run' } else { 'persistent-visible-observer' }
    ProcessId = $null
    LaunchedUtc = $null
    Executable = $executable
    ExecutableSha256 = $executableHash
    ExecutableBytes = $executableBytes
    ExecutableModifiedUtc = $executableModifiedUtc
    SessionDirectory = $plan.SessionDirectory
    ControlFile = $plan.ControlFile
    StatusFile = $plan.StatusFile
    SessionMetadataFile = $plan.SessionMetadataFile
    ScreenshotPattern = $plan.ScreenshotPattern
    WorldName = $plan.WorldName
    Profile = $Profile
    Focus = $Focus
    Scenery = $Scenery
    Seed = $Seed
    Hour = $Hour
    Width = $Width
    Height = $Height
    ScreenshotIntervalSeconds = 0.0
    PlanetaryStreaming = 'all'
    FarSurfaceMaterial = 'bridge-v2'
    FarHydrography = 'v1'
    FarSemanticCohorts = if ($SemanticCohorts) { 'silhouettes-v1' } else { 'off' }
    FarL0HeightMode = 'cardinal-trimmed-8-v1'
    IsolatedSettings = $true
    ObserverProtocol = $requiredObserverProtocol
    AutoExit = $false
}

if ($DryRun) {
    Write-Host 'LIVE OBSERVER DRY RUN — no directory created and no engine launched.'
    Write-Host "Executable: $executable"
    Write-Host "Session:    $($plan.SessionDirectory)"
    Write-Host "Control:    $($plan.ControlFile)"
    Write-Host "Status:     $($plan.StatusFile)"
    Write-Host "World:      $($plan.WorldName)"
    Write-Host "Viewport:   ${Width}x${Height}"
    [pscustomobject]$sessionInfo
    return
}

# Recheck after reserving the session. If another engine appeared during a
# release build or directory reservation, preserve the unused directory and
# fail closed rather than creating a second graphical/GPU session.
Assert-NoRunningVoxelNativeProcess
Assert-SafeObserverDirectoryChain -Path $agentRunsRoot
Assert-SafeObserverDirectoryChain -Path $savesRoot
Assert-SafeObserverDirectoryChain -Path $plan.SessionDirectory
Assert-SafeObserverRegularFileOrMissing -Path $executable -RequireExisting

$process = Start-LiveObserverProcess -Environment $environment
$processId = $process.Id
try {
    Wait-LiveObserverReady `
        -Process $process `
        -Plan $plan `
        -TimeoutSeconds $ReadinessTimeoutSeconds

    $launchedUtc = [DateTimeOffset]::UtcNow.ToString('o')
    $sessionInfo.ProcessId = $processId
    $sessionInfo.LaunchedUtc = $launchedUtc

$metadata = [ordered]@{
    schema_version = 1
    mode = $sessionInfo.Mode
    process_id = $processId
    launched_utc = $launchedUtc
    executable = $executable
    executable_sha256 = $executableHash
    executable_bytes = $executableBytes
    executable_modified_utc = $executableModifiedUtc
    session_directory = $plan.SessionDirectory
    control_file = $plan.ControlFile
    status_file = $plan.StatusFile
    screenshot_pattern = $plan.ScreenshotPattern
    world_name = $plan.WorldName
    profile = $Profile
    focus = $Focus
    scenery = $Scenery
    seed = $Seed
    hour = $Hour
    viewport_width = $Width
    viewport_height = $Height
    screenshot_interval_seconds = 0.0
    planetary_streaming = 'all'
    far_surface_material = 'bridge-v2'
    far_hydrography = 'v1'
    far_semantic_cohorts = if ($SemanticCohorts) { 'silhouettes-v1' } else { 'off' }
    far_l0_height_mode = 'cardinal-trimmed-8-v1'
    isolated_settings = $true
    observer_protocol = $requiredObserverProtocol
    auto_exit = $false
}
    Write-NewUtf8File -Path $plan.SessionMetadataFile -Content (
        $metadata | ConvertTo-Json -Depth 4
    )
}
catch {
    $startupError = $_.Exception.Message
    try {
        Stop-IncompleteLiveObserver -Process $process
    }
    catch {
        $rollbackError = $_.Exception.Message
        $process.Dispose()
        throw "Live observer startup failed and process rollback could not be confirmed. The session is preserved at $($plan.SessionDirectory). Startup error: $startupError Rollback error: $rollbackError"
    }
    $process.Dispose()
    throw "Live observer startup was rolled back; the unused session is preserved at $($plan.SessionDirectory). $startupError"
}

Write-Host 'LIVE OBSERVER STARTED — the visible engine remains open after this launcher returns.'
Write-Host "PID:        $processId"
Write-Host "Session:    $($plan.SessionDirectory)"
Write-Host "Control:    $($plan.ControlFile)"
Write-Host "Status:     $($plan.StatusFile)"
Write-Host "Metadata:   $($plan.SessionMetadataFile)"
Write-Host "Screenshots:$($plan.ScreenshotPattern)"
Write-Host "World:      $($plan.WorldName)"
Write-Host "Viewport:   ${Width}x${Height}"
Write-Host 'Steer the view by atomically updating the control file and incrementing its sequence.'
Write-Host 'Stop cleanly by requesting exit: true with a fresh sequence, or close the engine window.'
Write-Host 'Compiled Rust changes require one deliberate rebuild/restart; camera and control updates do not.'

[pscustomobject]$sessionInfo

if ($Wait) {
    Write-Host "Waiting for live observer PID $processId. The engine has no automatic timeout."
    $process.WaitForExit()
    $exitCode = $process.ExitCode
    $process.Dispose()
    if ($exitCode -ne 0) {
        throw "Live observer PID $processId exited with code $exitCode."
    }
    Write-Host "Live observer PID $processId exited cleanly."
}
else {
    $process.Dispose()
}
}
finally {
    if ($null -ne $launchMutex) {
        try {
            $launchMutex.ReleaseMutex()
        }
        finally {
            $launchMutex.Dispose()
        }
    }
}
