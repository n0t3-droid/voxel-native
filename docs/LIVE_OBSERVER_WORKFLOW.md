# Live Observer workflow

Live Observer is the persistent visual work surface for Voxel Native. It keeps
one visible release engine open while an external agent labels and moves the
camera to the system under discussion. Camera control uses the engine's bounded
RON bridge; it never needs to move, click, hide, or confine the user's OS
pointer.

This is an implementation tour and live inspection path. It complements the
deterministic screenshot/report routes in
[Responsive Visual QA](RESPONSIVE_VISUAL_QA.md); it does not replace their
acceptance evidence.

Platform boundary: the launcher currently supports Windows with PowerShell 7
only. It validates Windows junction/reparse-point ancestry and launches the
native `voxel-native.exe`; Linux and macOS observer support is not claimed.

## Start one persistent session

Run the launcher from the repository root:

```powershell
# Preview the resolved paths and options without creating anything.
.\scripts\live-observer.ps1 -DryRun

# Exercise the Windows junction-ancestor guard without building or launching.
.\scripts\live-observer.ps1 -PathSafetySelfTest

# Verify/build the release binary, launch Natural at the river, and return the
# shell after the engine has published ready bridge and in-game status files.
.\scripts\live-observer.ps1 `
  -Profile natural `
  -Focus river `
  -Scenery lush `
  -Seed 12345 `
  -Hour 15.65 `
  -Width 1600 `
  -Height 900

# Launch an Astral spawn view and keep this shell attached to the process
# lifetime. The mandatory Cargo build is incremental when sources are current.
.\scripts\live-observer.ps1 -Profile astral -Focus spawn -Wait
```

The default invocation returns after readiness while the visible engine stays
open. `-Wait` keeps the launcher attached instead. The launcher refuses to
start if any `voxel-native` process is already running, so it cannot silently
create a second graphical/GPU session.

| Option | Values or effect |
| --- | --- |
| `-Profile` | `natural` or `astral` |
| `-Focus` | `river` or `spawn` |
| `-Scenery` | `off`, `lean`, `balanced`, or `lush` |
| `-Seed` | Unsigned 32-bit world seed; default `12345` |
| `-Hour` | `0.0` through `24.0`; default `15.65` |
| `-WorldPrefix` | Safe prefix for the unique world name |
| `-ReadinessTimeoutSeconds` | Bounded startup wait from 10 through 300 seconds |
| `-Width`, `-Height` | Observer viewport; defaults `1600x900`, with width `960..3840` and height `540..2160` |
| `-SemanticCohorts` | Enables the explicit far-field silhouette route |
| `-Wait` | Waits for the engine process instead of returning after readiness |
| `-DryRun` | Prints a plan without creating a session or launching the engine |
| `-PathSafetySelfTest` | Creates one bounded fixture under ignored `tmp/`, proves a junction ancestor is rejected, removes only that fixture, and never launches the engine |

Every real launch first runs Cargo's incremental release build, then reserves a
new `agent_runs/live_observer_<epoch>` directory
and a matching `<WorldPrefix>_<epoch>` world. It never reuses or deletes an old
session or world. Isolated mode also bypasses reads and writes of the normal
`voxel-native-save.ron` settings file. Its cursor override remains released and
visible for the entire process, including paused, disabled, or handoff states.
The child also removes inherited live-link peer variables, so an unrelated
collaboration session cannot leak into the isolated observer.

Every existing directory component from the native volume root through the
repository, Cargo `target/release`, `agent_runs`, `saves`, and the unique session
is inspected without accepting a symlink, junction, or other reparse point.
The target chain is checked before the mandatory build, Cargo receives the
validated target directory explicitly, and the complete executable/session
chain is checked again immediately before process start. Rust repeats the full
ancestor-chain and leaf check before control reads and every observer-owned
write. Screenshot pixels are encoded under fixed pixel/byte ceilings and only
then atomically published by the safe writer inside Bevy's asynchronous
callback; bridge, status, capability, mission-feed, and control files share the
same boundary. CI exercises the launcher guard with `-PathSafetySelfTest`; a
pre-positioned redirected root, build target, session ancestor, or output leaf
therefore fails before observer I/O can escape the repository.

The launcher prints the exact paths and records them in
`observer-session.json`:

```text
agent_runs/live_observer_<epoch>/
  agent_control.ron       external command authority
  bridge.ron              startup/heartbeat and bridge state
  status.ron              authoritative engine telemetry
  capabilities.ron        advertised transport capabilities
  observer-session.json   launch identity and release-binary provenance
  live_0000.png           first requested screenshot, when present
```

## Move the view without touching the mouse

Atomically replace the printed `agent_control.ron` with a RON command such as:

```ron
(
    enabled: true,
    sequence: 1,
    view_label: "Near/Far settlement and L0 seam",
    camera_pose: Some((
        position: (120.0, 90.0, -64.0),
        yaw: 0.5,
        pitch: -0.25,
    )),
    screenshot: true,
)
```

The position is a RON tuple, not an array. `view_label` appears in the live
engine overlay and in status telemetry, so the visible frame says what Codex is
inspecting; control characters and line breaks are removed, and the label is
bounded to 96 characters. `screenshot: true` on a fresh sequence produces one
`live_NNNN.png` after two complete settling frames; it does not start periodic
capture.

For each new view, increase `sequence` strictly and publish the entire file by
temporary-file rename. A stale sequence is rejected. Reusing a sequence with a
different payload or camera pose is also rejected instead of replayed. In
isolated mode, the engine treats this external file as authoritative and does
not rewrite it from its UI.

One PowerShell 7 publication pattern is:

```powershell
$controlFile = '<path printed by the launcher>'
$command = @'
(
    enabled: true,
    sequence: 2,
    view_label: "Streaming frontier under settled load",
    camera_pose: Some((
        position: (384.0, 150.0, -256.0),
        yaw: 1.2,
        pitch: -0.35,
    )),
    screenshot: true,
)
'@

$target = [System.IO.Path]::GetFullPath($controlFile)
$temporary = Join-Path `
  ([System.IO.Path]::GetDirectoryName($target)) `
  ('.agent-control-' + [guid]::NewGuid().ToString('N') + '.tmp')
[System.IO.File]::WriteAllText(
  $temporary,
  $command,
  [System.Text.UTF8Encoding]::new($false)
)
[System.IO.File]::Move($temporary, $target, $true)
```

Camera commands are finite and bounded. Horizontal coordinates must remain in
the exact `f32` integer range (`|x|, |z| <= 16,777,216`),
`|y| <= 1,048,576`, and `|pitch| <= 1.54` radians. A rejected pose leaves the
camera unchanged and reports the reason in `status.ron`. An accepted pose is
applied once for its sequence, clears velocity, and leaves the observer in a
stable flying state.

## Readiness and feedback

The launcher always completes Cargo's incremental release build first. It then
declares success only after all four conditions are visible on disk:

1. `agent_control.ron` exists and is non-empty;
2. `bridge.ron` reports `observer_protocol_version: "live-observer-v1"`;
3. that bridge reports both `isolated_observer: true` and
   `runtime_enabled: true`;
4. `status.ron` reports `game_state: "InGame"` and
   `control_enabled: true`.

An older release binary therefore cannot satisfy readiness merely by producing
legacy bridge files.

During the session, inspect `status.ron` instead of guessing from pixels. Its
fields include the accepted command sequence and label, the camera sequence
cursor and handled sequence, current pose, streaming work, frame telemetry,
`last_error`, the independent `last_screenshot_error`, successful screenshot
count, and `last_screenshot`. `bridge.ron` is the
low-rate heartbeat and identifies the active control and session paths.

`capabilities.ron` is the source of truth for bridge maturity. The current
shipping declaration is deliberately conservative: the bounded RON fallback
and visual capture are ready; the planned low-latency direct bridge is not.
Do not describe this observer as using the direct bridge until that capability
file advertises it as ready.

## One session, deliberate rebuilds

View labels, camera poses, movement commands, and screenshot requests update
inside the running process. Compiled Rust does **not** hot-reload into that
process. Use the observer this way:

1. launch one visible session and steer it among the systems being inspected;
2. batch related Rust changes and finish their non-visual checks;
3. request a clean observer exit;
4. rebuild once and launch one replacement session for the completed batch;
5. move directly to the changed system and capture evidence only when useful.

This keeps the engine visible during normal inspection without pretending an
old binary contains source edits that have not been compiled.

## Stop cleanly

Publish a fresh sequence with `exit: true`:

```ron
(
    enabled: true,
    sequence: 3,
    view_label: "Observer session complete",
    exit: true,
)
```

The user may also close the engine window. The session directory and any unique
world artifacts are preserved; the launcher never cleans them up implicitly.
