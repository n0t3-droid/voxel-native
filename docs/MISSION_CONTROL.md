# Mission Control // Omniscope

Mission Control is the local multi-agent observation wall for Voxel-Native.
Every engine instance controlled by a coding or QA agent can publish a live
thumbnail and a bounded telemetry feed. The wall discovers current and future
agents automatically; it does not contain a fixed list of four slots.

## Start the wall

From the repository root:

```powershell
.\scripts\mission-control.ps1 -Mode Dashboard -Build
```

The dashboard opens as a visible Voxel-Native window. `F9` hides or restores
the wall. The registry root defaults to `agent_runs`; existing files are never
deleted by the launcher.

## Start an agent feed

```powershell
.\scripts\mission-control.ps1 `
  -Mode Agent `
  -Slot 1 `
  -AgentId terrain-01 `
  -AgentName 'TERRAIN ARCHITECT' `
  -Role 'WORLD STREAMING' `
  -Task 'Inspect remote provinces and LOD transitions' `
  -FleetId 'voxel-native-main-fleet' `
  -World mission_astral `
  -Profile astral `
  -Seed 12345 `
  -OpenDashboard
```

Each slot reserves a distinct pair of loopback UDP ports. The launcher refuses
an occupied pair instead of silently attaching to the wrong agent. Slots 1-50
are supported by the launcher; the dashboard itself accepts up to 48 recent
feeds so corrupted or abandoned directories cannot allocate an unbounded UI.

An external agent continues to drive the instance through its generated
`agent_control.ron`. Mission Control is an observation and handoff layer, not a
second source of simultaneous movement commands.

## Feed card

Every mini-screen shows:

- current screenshot or a safe waiting state;
- live/recent state;
- agent name, role and current task;
- world, profile, seed and time;
- position, FPS, frame time and stall count;
- resident chunks and pending streaming work;
- warning/error signal and current command status.
- fleet identity and whether the agent advertises the current shared power
  profile.

Selecting a card opens a larger focus panel without forcing the user to follow
the agent's camera.

## Spectate and Join

An active agent started by the launcher publishes a local Live Link pair.

- **Spectate // Open Live Engine** starts a separate deterministic user engine
  in the same world and follows the selected agent at full render frame rate.
- **Join // You Lead** starts that user engine with user authority. The agent's
  synthetic gameplay input is suppressed while your lease is alive, and the
  agent follows your pose and actions.
- `F10` inside the user engine switches between spectator and Join authority.
- If the user engine closes or the link times out, authority returns to the
  coding agent automatically.

The mini-screen wall uses periodic PNG frames to keep many feeds affordable.
The selected Spectate/Join engine uses the existing 30 Hz pose/input link; it
is not controlled from a low-frame-rate thumbnail.

## Future-agent publishing contract

An Agent-Control instance becomes a publisher when:

```text
VOXEL_NATIVE_MISSION_FEED=1
```

Optional identity variables:

```text
VOXEL_NATIVE_AGENT_ID
VOXEL_NATIVE_AGENT_FLEET_ID
VOXEL_NATIVE_AGENT_NAME
VOXEL_NATIVE_AGENT_ROLE
VOXEL_NATIVE_AGENT_TASK
VOXEL_NATIVE_AGENT_SCREENSHOT_INTERVAL
```

It writes `mission_feed.ron` beside its `status.ron` and screenshots. Schema v1
is deliberately smaller than the complete Agent-Control telemetry, so a future
agent can implement the contract without copying internal engine state.

The file contains identity, task, process ID, heartbeat, world identity,
camera/player position, performance, streaming pressure, current screenshot
and optional Live Link endpoints. It also publishes the capability schema,
shared power-profile ID and the current direct/RON/visual bridge readiness.
New fields must be backward-compatible or the schema version must change.

Each startup also writes `capabilities.ron` beside the feed. Identity, task and
authority may differ per agent; the command, observation and safety profile may
not. Mission Control marks an old, missing or different capability profile as
`POWER MISMATCH` instead of silently assuming parity. The current profile is
honest about transport maturity: the bounded RON control path and visual
capture are available, while the planned low-latency direct bridge is not yet
advertised as ready.

## Safety and resource limits

Mission Control is deliberately local:

- Live Link accepts loopback addresses only.
- Feed scanning has a depth and count limit and skips directory links.
- RON files above 256 KiB are ignored.
- A screenshot must resolve under the configured mission root.
- Only PNG files are decoded.
- Compressed files above 20 MiB, edges above 4096 pixels, or images above
  16,777,216 pixels are rejected before texture upload.
- Texture handles are released when a feed leaves the recent registry.
- A feed older than four seconds is not allowed to launch Join.
- Recent/offline feeds are hidden by default and expire from discovery after
  fifteen minutes.

These constraints protect the dashboard from malformed future-agent output
and keep observation overhead bounded.

## Responsive wall

The feed wall uses explicit layout breakpoints:

| Available width | Columns |
| ---: | ---: |
| below 620 px | 1 |
| 620-1049 px | 2 |
| 1050-1479 px | 3 |
| 1480 px and above | 4 |

The selected feed occupies a separate focus panel and the remaining cards stay
inside a vertical scroll region. This rule is part of the repository's general
responsive-UI contract: no feature is considered complete after testing only
on the developer's current window size.

The focus panel has its own independent collapse rules. Below 820 px it stacks
the live preview above telemetry instead of squeezing both into unreadable
half-width columns, and the Spectate/Join controls become full-width rows.
Below 760 px the title, registry state and recent/offline control also stack.
These thresholds are regression-tested separately from the feed-card grid so a
future change cannot pass the column test while still overlapping the focused
agent controls.

## Required visual QA

Before Mission Control changes are accepted:

1. start two publishers with different slots and identities;
2. confirm both mini-screens update without mixing screenshots;
3. close one publisher and confirm it becomes recent/offline within four
   seconds;
4. resize the wall through 320x480, 800x600, 1080p and ultrawide layouts;
5. open Spectate and verify the agent remains authoritative;
6. open Join and verify the user becomes authoritative;
7. close Join and verify the authority lease returns safely;
8. attempt an out-of-root screenshot and non-loopback endpoint and confirm both
   are rejected;
9. compare frame time with the wall hidden and visible.

The platform is infrastructure for continuous engine inspection. It does not
replace the final integrated QA pass: an agent can approve its own feed, but
the primary integration run still verifies the whole world and all merged
changes together.
