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

The latest engine work turns bot construction into a road-first city planner:

- roads are editable components with smooth deck grades for bridges, ramps,
  corners, plazas, and future roundabout work;
- bot projects reserve footprints before building, so roads and buildings do
  not cut through each other while the city grows;
- duplicate road corridors are rejected before voxel edits are queued;
- authored district road anchors now guide final road-site selection instead
  of being mistaken for already-built duplicate roads;
- civic, service, tower, prep, and detail pads lift to the nearest raised road
  deck instead of sinking back to raw terrain;
- building lots bind to frontage streets and record the street face plus target
  deck height in the bot plan rows;
- districts prefer unused project kinds before repeating, giving the skyline
  residential, civic, utility, plaza, and landmark variety;
- cursor capture is centralized so live build, navigation, combat, menus, and
  the picker do not fight each other during mode switches;
- the in-game Build Studio exposes one-click workflow icons for Sketch,
  Push/Pull, Roads, City Shells, and Towers;
- max-distance streaming pauses bot edit slices when the visible horizon is
  still catching up, keeping low-end PCs responsive.

## GitHub Snapshot

This branch is source-first. No old Visual Studio screenshots or stale gallery
images are tracked here; generated QA screenshots and videos stay local so the
GitHub front page describes the engine instead of showing outdated captures.

The public project view should explain what is implemented, how to run it, and
which math keeps the engine fast. Current bot-planning details live in
[`docs/CITY_PLANNER_MATH.md`](docs/CITY_PLANNER_MATH.md).

Verified update for this snapshot:

- road anchors are treated as planning intent, not completed road geometry;
- road candidates receive a bounded alignment score when their centerline or
  grid segment follows an authored district street;
- duplicate checks still protect against actual user roads and completed bot
  road projects;
- mode/cursor tests lock the rule that gameplay hides the OS cursor while
  menus and clickable picker overlays release it;
- workflow presets collapse common multi-step builder setups into one icon;
- local engine captures remain ignored rather than becoming GitHub gallery
  clutter.

## City Planner Math

The bot planner uses bounded scoring instead of expensive world scans. A site is
chosen from a small candidate set and scored with weighted, clamped terms:

```text
site_score =
    2.50 * flatness
  + 2.40 * road_access
  + 1.80 * district_balance
  + 1.35 * route_fit
  + 0.55 * block_fit
  + 4.00 * road_anchor_alignment
  + 2.50 * semantic_anchor
  - 0.0005 * center_distance
```

Roads prefer routes that can become smooth decks instead of voxel staircases:

```text
route_fit =
    1
  - 0.55 * avg_step / 5
  - 0.30 * max_step / 9
  - 0.15 * max(height_range - 18, 0) / 34
```

Bridge and raised-road heights use smooth interpolation:

```text
smoothstep(t) = t * t * (3 - 2 * t)
deck_y(t) = lerp(start_y, end_y, smoothstep(t))
```

This keeps bot roads readable, lets nearby buildings align to raised decks, and
avoids turning the low-end target into a brute-force simulation.

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

For the city-planning invariants, formulas, and low-end performance boundaries,
see [`docs/CITY_PLANNER_MATH.md`](docs/CITY_PLANNER_MATH.md).

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
