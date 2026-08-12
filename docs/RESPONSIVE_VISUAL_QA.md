# Responsive visual QA contract

Visual work is not complete when it only looks correct in the developer's
current window. Every persistent HUD, editor, debug, spectator, and agent
surface must remain usable across the matrix below. This is a release guardrail
for future changes, not a claim that every older panel already passes it.

## Required viewport matrix

| Class | Minimum evidence | Main failure to reject |
| --- | --- | --- |
| 320 x 480 | layout/unit test and a capture when the panel changes | controls outside the viewport, negative width, unreadable overlap |
| 800 x 600 | visual capture | tool flyouts covering status/errors or the center reticle |
| 960 x 540 | visual capture | minimum-16:9 overlap or unreachable essential controls |
| 1280 x 720 | visual capture | gameplay blocked by combined editor and runtime overlays |
| 1920 x 1080 | primary visual acceptance capture | clipping, accidental dead space, weak hierarchy |
| 2560 x 1440 | layout test and visual capture for major UI changes | panels stretching instead of preserving readable widths |
| 3440 x 1440 | layout test and visual capture for edge-anchored UI changes | detached edge panels or excessive cursor travel |
| adverse portrait/narrow size | layout test; capture when support is claimed | unsafe overlap instead of reflow or an explicit supported-limit notice |

Repeat the affected sizes at 100%, 150%, and 200% OS/text scale when a change
touches typography, fixed pixel offsets, icon hit areas, or window chrome. Test
long translated labels at roughly 1.5 times the English/German width.

Desktop QA can drive the matrix without rebuilding the engine. Set
`VOXEL_NATIVE_WINDOW_WIDTH` and `VOXEL_NATIVE_WINDOW_HEIGHT` before launching
the same isolated route, for example:

```powershell
$env:VOXEL_NATIVE_WINDOW_WIDTH='800'
$env:VOXEL_NATIVE_WINDOW_HEIGHT='600'
$env:VOXEL_NATIVE_QA='1'
$env:VOXEL_NATIVE_QA_WORLD='qa_ui_800x600_unique_name'
target/release/voxel-native.exe --qa
```

Values are parsed as finite numbers and bounded to a safe 320..8192 width and
240..8192 height. Invalid values fall back to 1280 x 720. The bounds prevent a
typo from creating an unusable or pathologically large render target; they do
not replace the required captures above.

## Simultaneous-state checks

- Open the toolbox, contextual flyout, selection status, Agent Control, error
  message, and performance HUD in their realistic combinations. An overlay may
  reserve a known rail, compact, scroll, or relocate; it may not silently cover
  an actionable control.
- Verify mouse, keyboard, and controller focus. UI hover must not also rotate,
  fire, place, or delete in the world. Escape must have one visible result.
- Preserve errors until the subsystem that created them explicitly clears
  them. A successful per-frame poll must not erase an unrelated failure.
- Check normal, reduced-motion, and low-spec modes. Reduced motion may remove
  ornamental animation, but must preserve state and selection feedback.
- Check contrast over bright sky, pale terrain, dark interiors, foliage, and
  emissive scenes. Color alone must not be the only error/selection signal.
- Treat screenshots as evidence, not as the only oracle: inspect status RON,
  logs, shader errors, frame time, stalls, and gameplay position as well.

## Visual and simulation acceptance

For shader, terrain, vegetation, water, weather, or animation changes:

1. Capture the same stationary camera at two separated times to prove intended
   motion and expose flicker, detachment, chunk-wide sliding, or seam changes.
2. Fly a representative route and compare stabilized frame time with the prior
   build; terrain generation and shader warm-up are reported separately.
3. Confirm visual-only systems do not mutate player, shuttle, projectile,
   collider, voxel-authority, save, or bot-navigation state.
4. Inspect close, gameplay, and horizon distances. Reject detail that only
   works in one shot, excessive repetition, floating vegetation, hard biome
   borders, and decoration that destroys silhouettes.
5. Use a new isolated QA world and run directory. Never reuse, clean, delete,
   or overwrite a player's world to obtain test evidence.

## Causal cross-system inspection loop

Every visual flight is also a broad engine inspection. Do not stop at the
feature named in the current task, and do not cosmetically hide an anomaly
before identifying which state is authoritative. For each suspicious object,
motion, gap, UI element, or timing event, record:

1. **Observation:** what is visibly wrong, at which camera position, viewport,
   time, graphics tier, world seed, and distance band.
2. **Identity:** whether it is terrain, a missing mesh, deliberate cave, bot,
   ship, particle, celestial body, overlay, stale preview, or capture artifact.
   Inspect a second angle or telemetry before guessing from one screenshot.
3. **Authority:** which subsystem owns position, material, collision, save
   state, selection, and navigation. A render-only correction must not silently
   rewrite simulation state; a simulation correction must not be faked only in
   the shader.
4. **Lifecycle:** test fresh world, existing save, load/unload, undo/redo,
   tool/mode switch, pause/resume, and asynchronous startup ordering where
   relevant. Separate migration policy from the fresh-world fix.
5. **Scale:** repeat near/play/horizon distances, the affected viewport matrix,
   DPI/text scale, and Fast/Balanced/High. Ask whether LOD creates sub-pixel
   noise, whether large silhouettes clip, and whether a beautiful close shot
   collapses during ordinary flight.
6. **Coupling:** prove that visual wind, weather, vegetation, ambient life, and
   presentation do not alter player/shuttle/projectile physics, voxel authority,
   bot paths, saves, or editor hit records unless that coupling is intentional.
7. **Budget:** compare queue pressure, frame time, stalls, allocation/upload
   churn, and deterministic geometry caps. A feature is not accepted by hiding
   its cost behind warm-up or adaptive render distance.
8. **Disposition:** mark the issue as fixed with regression proof, intentional
   with rationale, deferred with a concrete risk, or unresolved. Never let a
   successful test silently erase a visual finding.

This loop is deliberately recursive: a fix is inspected for new failure modes,
including alternate seeds, negative/extreme coordinates, overlapping objects,
different input devices, translated labels, low-end budgets, and stale saves.
It turns "remember to look wider" into a persistent release contract.

## Multiscale environmental detail

Do not ask one representation to solve every viewing distance. Review natural
assets as a three-band contract:

- Near field: texture pores, branch attachment, material roughness, contact
  depth, and wind phase must survive a walking-speed inspection without
  turning foliage into sealed cubes or noisy transparency.
- Play field: crown volume, species stiffness, understorey colonies, meadow
  openings, and rock/material masses must read during ordinary flight. Repeated
  stamps, evenly spaced trees, and one-block decoration scatter are failures.
- Horizon field: forest cohorts, light wells, mountain silhouettes, atmospheric
  separation, and macro occlusion must remain stable under mipmapping and fog.
  Fine detail may collapse into an aggregate, but the aggregate may not shimmer
  or change the authoritative simulation.

Prefer transitions that emerge from filtering and bounded representation: for
example, a binary foliage pore mask can be visible in the base texture while
lower gamma-correct mip levels converge to a calm, filled crown. Record the
near/mid/far evidence separately; a beautiful hero shot is not proof that all
three bands work.

Large procedural objects must also be reviewed against the spatial partition
that owns them. A chunk-local safety margin is not visually neutral: when an
object radius approaches half the chunk width, its legal root positions
collapse toward the chunk centre and reveal the streaming grid. Prefer stable
world-space ownership with bounded neighbour replay and clipped writes. Test a
root near every horizontal seam, prove at least one face-connected pair across
the seam, and verify that the candidate distribution reaches the full legal
cell rather than one or two centre coordinates.

## Proof recorded with a change

Record the tested resolutions, world seed, camera/route, tool states exercised,
capture paths, average/max frame time, stall count, shader/log error state, and
known visual limits. If a matrix cell was not tested, state that directly.

### Current release evidence - 2026-08-09

| Viewport | Release evidence | Capture completion | Result |
| --- | --- | --- | --- |
| 320 x 480 | `qa_runs/run_1786291828/shot_0000.png` | 3 report paths = 3 terminal-IEND PNG files | compact dock clears the toolbox rail; core controls remain visible |
| 800 x 600 | `qa_runs/run_1786291220/shot_0000.png` | 4 report paths = 4 terminal-IEND PNG files | stacked status remains readable and clear of the toolbar |
| 960 x 540 | not captured in this pass | no evidence claimed | outstanding |
| 1280 x 720 | `qa_runs/run_1786291899/shot_0000.png` | 4 report paths = 4 terminal-IEND PNG files | wide mountain-river route and status presentation accepted |
| 1920 x 1080 | not captured in this pass | no evidence claimed | outstanding |
| 2560 x 1440 | not captured in this pass | no evidence claimed | outstanding |
| 3440 x 1440 | not captured in this pass | no evidence claimed | outstanding |
| adverse portrait/narrow size | not captured in this pass | no evidence claimed | outstanding |

The 100%, 150%, and 200% OS/text-scale cells also remain outstanding. The
layout metrics and safe window bounds are regression proof, but they are not a
substitute for those visual captures. The final 1280 x 720 route recorded 920
passing binary tests and a successful release build separately from descriptive
runtime telemetry; average FPS from this single route is not a causal benchmark.
