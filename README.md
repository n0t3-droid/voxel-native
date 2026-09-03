# Voxel-Native

The project goal is not just a voxel sandbox.

## Engine

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

- bot autonomy is command-gated by default: workers stay parked on load until
  the player places a city area or explicitly queues a bot task;
- the City Area tool now behaves like a road component workflow: two clicks
  mark the exact bot city footprint, then bots queue bounded avenue strips,
  a center junction/roundabout block, flatten passes, frontage parcels, plazas,
  residential blocks, parks, and towers inside that marked space instead of
  taking over the whole world;
- roads are editable components with smooth deck grades for bridges, ramps,
  corners, plazas, and future roundabout work;
- manual roads now start at boulevard scale by default, with larger editable
  roundabouts and a more forgiving straight-line intent lock while dragging;
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
- live Build mode now defaults to Sketch Draw instead of a low-level brush:
  LMB draws a snapped face/rectangle, RMB cuts openings, and G swaps into
  Push/Pull without opening a dense panel;
- the in-game Build Studio exposes one-click workflow icons for Sketch,
  Push/Pull, Roads, City Shells, and Towers;
- Sketch-style rectangle drawing keeps its locked floor or wall plane even
  when the cursor moves over empty space, so extensions, roofs, gardens, and
  side wings can be sketched without hunting for another voxel target;
- Smart Cut rectangles drill through continuous wall thickness for fast
  window, door, and facade openings instead of only shaving the front face;
- Build Studio now exposes categorized material swatches and architecture
  presets for modern houses, road/traffic layouts, gardens, towers, and
  spacecraft hulls directly in the live builder workflow;
- wide manual road components now stamp curb edges, sidewalk shoulders, and
  lightweight lamp markers so editable roads read as city infrastructure
  instead of flat strips;
- crystal, ice, water, lava, cockpit-glass, and neon-glass terrain materials
  avoid Bevy's sorted alpha-blend path, reducing close-range lag in dense
  glowing biomes on low-end PCs;
- crystal-spire terrain is bounded by a regression test that keeps hero
  skylines while preserving open flight corridors and safer close-up geometry;
- max-distance streaming pauses bot edit slices when the visible horizon is
  still catching up, keeping low-end PCs responsive.
- terrain installs are frame-capped so a wave of completed async chunks does
  not all fold back into the world during the same flight frame.
- concept-art inventions now ship in default terrain: bounded floating sky
  islands with preserved flight corridors, mesa/volcanic lava rivers edged by
  cyan/magenta neon channels, sparse monorail ribbons with bridge pylons and
  nearby moving carts, rare orbital docking spires on mountain peaks, and
  defense turret pads that fire red laser beams at hostile drones.

## GitHub Snapshot

This branch is source-first. No old Visual Studio screenshots or stale gallery
images are tracked here; generated QA screenshots and videos stay local so the
GitHub front page describes the engine instead of showing outdated captures.

The public project view should explain what is implemented, how to run it, and
which math keeps the engine fast. Current bot-planning details live in
[`docs/CITY_PLANNER_MATH.md`](docs/CITY_PLANNER_MATH.md).

Verified update for this snapshot:

- bot load defaults keep workers parked until the player commands them;
- placed bot city areas persist as the marked footprint and queue road-skeleton
  projects before clearing, civic, residential, park, and skyline parcels inside
  that boundary;
- road anchors are treated as planning intent, not completed road geometry;
- road candidates receive a bounded alignment score when their centerline or
  grid segment follows an authored district street;
- duplicate checks still protect against actual user roads and completed bot
  road projects;
- mode/cursor tests lock the rule that gameplay hides the OS cursor while
  menus and clickable picker overlays release it;
- workflow presets collapse common multi-step builder setups into one icon;
- Sketch-style builder tests cover locked-plane drawing into empty space and
  through-wall rectangle cuts for reliable windows and doors;
- the material catalog test keeps every buildable voxel block present exactly
  once across the SketchUp-style swatch categories;
- translucent terrain uses alpha-to-coverage instead of sorted alpha blending,
  and crystal spires are generated as wider hero forms instead of a solid
  translucent wall;
- local engine captures remain ignored rather than becoming GitHub gallery
  clutter.
- floating sky islands, lava/neon canyon channels, monorail tracks with carts,
  docking-spire landmarks, and drone-targeting defense turrets extend the
  showcase terrain toward the reference concept art.

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
