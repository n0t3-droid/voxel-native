# Voxel-Native

Voxel-Native is the native Rust successor to R93G: a Bevy/wgpu voxel engine
focused on fast streaming worlds, shuttle flight, shooter systems, friendly
robots, and autonomous city construction that can still run on modest PCs.

The project goal is not just a voxel sandbox. It is a playable engine where the
world, the UI, the spacecraft, the bots, and the terrain all work together as a
coherent sci-fi game.

## Engine Pillars

- **Native performance:** Rust + Bevy + wgpu for DX12/Vulkan/Metal instead of
  an Electron or browser shell.
- **Low-end friendly streaming:** chunk budgets, mesh budgets, and NeuroCore
  runtime throttling keep the game responsive while the visible horizon loads.
- **Bot-built cities:** friendly bot crews plan road-first city growth, keep a
  player-safe build buffer, and store project concepts with phases, owners,
  materials, structure, architecture, texture, and detail rows.
- **Cinematic voxel terrain:** procedural coastline, forests, terrain height,
  water, caves, and sci-fi landmarks are generated as real voxels.
- **Shuttle and shooter loop:** ships, weapons, drone combat, editor tools, and
  bot companions live in the same world instead of separate demos.
- **Liquid-glass engine UI:** HUD, toolbelt, bot panels, and system surfaces are
  styled as in-game engine controls rather than flat debug windows.

## Current Focus

The latest engine work improves the bot-city workflow and max-distance world
streaming:

- autonomous builds no longer use the player as the default construction anchor;
- a wider no-build buffer keeps projects away from the player and parked ships;
- starter city recovery is road-first, so decorative street work cannot replace
  access roads before the city has a real road grid;
- bot edit slices pause while the effective render horizon is far below the
  requested max chunk distance;
- the auto streaming governor recovers the horizon faster after startup stalls;
- regression tests cover player clearance, road-access planning, queued project
  pressure, and runtime budget behavior.

## Build And Run

```powershell
# Debug build
cargo run

# Release build
cargo run --release
```

The first Bevy build can take a while. Later builds are faster because the dev
profile optimizes dependencies while keeping the game code incremental.

## Controls

- `WASD` move
- mouse look
- `Space` / `Shift` fly up and down
- `Ctrl` sprint
- `Esc` release mouse / pause
- `F3` opens the in-game engine tools

## Autonomous QA

The engine can run deterministic visual/performance QA without manual play. QA
flies a route, captures screenshots locally, and writes a RON report with frame
timing, chunk counts, mesh queues, dirty chunks, render distance, and stalls.

```powershell
$env:VOXEL_NATIVE_QA='1'
$env:VOXEL_NATIVE_QA_SECONDS='45'
$env:VOXEL_NATIVE_QA_SCREENSHOT_INTERVAL='7'
.\target\release\voxel-native.exe --qa
```

Generated QA output is local-only:

- `qa_runs\run_<timestamp>\report.ron`
- `qa_runs\run_<timestamp>\shot_0000.png`
- saved bot project state under `saves\<world>_bots\`

These captures are intentionally ignored by Git. The repository should stay
focused on source, design, and reproducible engine behavior.

## Agent Control

`--agent-control` starts a visible session controlled by `agent_control.ron`.
This is used for external automation, visual checks, movement, screenshots,
weapon testing, and engine state inspection.

```powershell
.\target\release\voxel-native.exe --agent-control
```

Example:

```ron
(
    enabled: true,
    sequence: 1,
    forward: 1.0,
    right: 0.0,
    up: 0.0,
    sprint: true,
    fly: true,
    look_x: 0.35,
    look_y: -0.05,
    fire: false,
    scope: false,
    screenshot: true,
    exit: false,
)
```

Status and live screenshots are written under `agent_runs\live_<timestamp>\`.

## Architecture

| Area | Rust Modules |
| --- | --- |
| Blocks, chunks, world data | `src/blocks.rs`, `src/chunk.rs`, `src/world.rs` |
| Terrain and meshing | `src/terrain.rs`, `src/mesher.rs` |
| Runtime budgets | `src/neurocore.rs`, `src/settings.rs` |
| Player, weapons, ships | `src/player.rs`, `src/weapons.rs`, `src/ships.rs` |
| Bots and city autonomy | `src/bots.rs`, `src/city.rs` |
| UI and engine tools | `src/hud.rs`, `src/editor.rs`, `src/toolbelt.rs`, `src/theme.rs` |
| QA and automation | `src/qa.rs`, `src/agent_control.rs` |

## Development Standard

Before presenting a change as ready:

```powershell
cargo fmt --all
cargo test --workspace --quiet
cargo build --quiet
```

For visual or streaming changes, also run a QA world and inspect the generated
`report.ron` plus the latest screenshot.

## Roadmap

1. Make bot city planning increasingly architectural: road hierarchy, terrain
   following, skyline rules, residential variation, plazas, parks, service pads,
   and readable human-scale details.
2. Continue terrain beautification across the whole engine without replacing
   performance with noisy decoration.
3. Push ships toward solid, detailed cockpit and hull designs that read as real
   spacecraft from gameplay distance.
4. Keep low-end PCs as a first-class target by making every visual upgrade pass
   through chunk, mesh, and frame-budget verification.
