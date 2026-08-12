param(
    [ValidateSet('Dashboard', 'Agent')]
    [string]$Mode = 'Dashboard',

    [ValidateRange(1, 50)]
    [int]$Slot = 1,

    [string]$AgentId = 'agent-01',
    [string]$AgentName = 'WORLD EXPLORER 01',
    [string]$Role = 'ENGINE EXPLORER',
    [string]$Task = 'Autonomous world inspection',
    [string]$FleetId = 'voxel-native-main-fleet',
    [string]$World = 'mission_control_world',

    [ValidateSet('natural', 'astral')]
    [string]$Profile = 'astral',

    [uint32]$Seed = 12345,

    [ValidateRange(0.0, 24.0)]
    [double]$Hour = 15.65,

    [ValidateRange(0.25, 30.0)]
    [double]$ScreenshotInterval = 1.5,

    [switch]$Build,
    [switch]$OpenDashboard
)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$executable = Join-Path $projectRoot 'target\debug\voxel-native.exe'

if ($Build -or -not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    Push-Location $projectRoot
    try {
        & cargo build --bin voxel-native
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
}

function Start-VoxelNativeProcess {
    param(
        [string[]]$Arguments,
        [hashtable]$Environment
    )

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $executable
    $startInfo.WorkingDirectory = $projectRoot
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $false
    $startInfo.Arguments = ($Arguments | ForEach-Object {
        if ($_ -match '[\s"]') {
            '"' + ($_ -replace '"', '\"') + '"'
        }
        else {
            $_
        }
    }) -join ' '
    foreach ($entry in $Environment.GetEnumerator()) {
        $startInfo.EnvironmentVariables[$entry.Key] = [string]$entry.Value
    }
    return [System.Diagnostics.Process]::Start($startInfo)
}

function Start-MissionDashboard {
    $dashboardEnvironment = @{
        'VOXEL_NATIVE_MISSION_CONTROL' = '1'
        'VOXEL_NATIVE_MISSION_ROOT' = (Join-Path $projectRoot 'agent_runs')
        'VOXEL_NATIVE_INSTANCE_LABEL' = 'MISSION CONTROL // OMNISCOPE'
    }
    $process = Start-VoxelNativeProcess -Arguments @('--mission-control') -Environment $dashboardEnvironment
    Write-Host "Mission Control started (PID $($process.Id)). F9 hides or restores the wall."
}

if ($Mode -eq 'Dashboard') {
    Start-MissionDashboard
    return
}

$safeAgentId = -join ($AgentId.ToCharArray() | Where-Object {
    [char]::IsLetterOrDigit($_) -or $_ -eq '-' -or $_ -eq '_'
})
if ([string]::IsNullOrWhiteSpace($safeAgentId)) {
    throw 'AgentId must contain at least one letter, number, dash, or underscore.'
}
$safeAgentId = $safeAgentId.Substring(0, [Math]::Min(64, $safeAgentId.Length))

$basePort = 48100 + (($Slot - 1) * 2)
$agentPort = $basePort
$viewerPort = $basePort + 1
$activeUdpPorts = [System.Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveUdpListeners().Port
if ($activeUdpPorts -contains $agentPort -or $activeUdpPorts -contains $viewerPort) {
    throw "Mission slot $Slot is busy (UDP $agentPort/$viewerPort). Choose a different -Slot."
}

$epoch = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
$sessionDir = Join-Path $projectRoot "agent_runs\mission\${safeAgentId}_${epoch}"
[System.IO.Directory]::CreateDirectory($sessionDir) | Out-Null
$controlFile = Join-Path $sessionDir 'agent_control.ron'

$agentEnvironment = @{
    'VOXEL_NATIVE_AGENT_CONTROL' = '1'
    'VOXEL_NATIVE_AGENT_CONTROL_FILE' = $controlFile
    'VOXEL_NATIVE_AGENT_SESSION_DIR' = $sessionDir
    'VOXEL_NATIVE_AGENT_WORLD' = $World
    'VOXEL_NATIVE_AGENT_PROFILE' = $Profile
    'VOXEL_NATIVE_AGENT_SEED' = $Seed
    'VOXEL_NATIVE_AGENT_HOUR' = $Hour
    'VOXEL_NATIVE_AGENT_ID' = $safeAgentId
    'VOXEL_NATIVE_AGENT_FLEET_ID' = $FleetId
    'VOXEL_NATIVE_AGENT_NAME' = $AgentName
    'VOXEL_NATIVE_AGENT_ROLE' = $Role
    'VOXEL_NATIVE_AGENT_TASK' = $Task
    'VOXEL_NATIVE_MISSION_FEED' = '1'
    'VOXEL_NATIVE_AGENT_SCREENSHOT_INTERVAL' = $ScreenshotInterval
    'VOXEL_NATIVE_LIVE_LINK_SIDE' = 'CODEX'
    'VOXEL_NATIVE_LIVE_LINK_BIND' = "127.0.0.1:$agentPort"
    'VOXEL_NATIVE_LIVE_LINK_PEER' = "127.0.0.1:$viewerPort"
    'VOXEL_NATIVE_INSTANCE_LABEL' = "AGENT // $AgentName"
}

$agentProcess = Start-VoxelNativeProcess -Arguments @('--agent-control') -Environment $agentEnvironment
Write-Host "Agent feed '$safeAgentId' started (PID $($agentProcess.Id))."
Write-Host "Session: $sessionDir"
Write-Host "Live Link: CODEX 127.0.0.1:$agentPort <-> USER 127.0.0.1:$viewerPort"

if ($OpenDashboard) {
    Start-MissionDashboard
}
