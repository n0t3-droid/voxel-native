# Voxel Native Build and Modeling Roadmap

Status: maintained product roadmap, August 2026. This document distinguishes
compile-registered behavior from live integration and from visually accepted
interaction. It does not claim SketchUp equivalence or release readiness.

## Product direction

Voxel Native is building a mouse-first modeling environment inside an editable
voxel world. The target is a coherent loop:

```text
point or select
  -> preview the exact affected geometry
  -> commit one bounded edit transaction
  -> preserve semantic identity and materials
  -> undo or redo the same transaction
  -> verify the result in the native engine
```

The editor must remain understandable while the world streams, the window
resizes, and control moves between play, build, menus, and observation. A tool
is not complete merely because its data type or command exists.

## Status vocabulary

| Label | Meaning |
| --- | --- |
| **Live** | Registered in the native application and reachable through the normal UI. |
| **Partial** | A real implementation exists, but one or more interaction, durability, or visual-acceptance gates remain open. |
| **Pure layer** | Compiled and tested as data or math, but not connected to the live authoring path. |
| **Planned** | A forward requirement, not implemented capability. |

## What exists today

| Area | Status | Source boundary | Honest limitation |
| --- | --- | --- | --- |
| Mode and input authority | **Live** | [`src/mode.rs`](src/mode.rs), [`src/menu.rs`](src/menu.rs), [`src/toolbelt.rs`](src/toolbelt.rs) | Mode, cursor, menu, editor, and weapon policies share one authority, but every transition still needs native edge-case inspection. |
| Modeling tool rail | **Live / Partial** | [`src/toolbelt.rs`](src/toolbelt.rs), [`src/builder.rs`](src/builder.rs), [`src/sculpt/`](src/sculpt/) | Core drawing and transform tools are exposed; their inference and selection behavior is not yet SketchUp-equivalent. |
| Semantic document and picking | **Partial** | [`src/sketch_model.rs`](src/sketch_model.rs), [`src/selection.rs`](src/selection.rs) | Raw hits, semantic links, and inference candidates exist, but universal occlusion-aware picking and complete nested selection do not. |
| Move, rotate, scale, and push/pull | **Partial** | [`src/sculpt/transform.rs`](src/sculpt/transform.rs), [`src/sculpt/pushpull.rs`](src/sculpt/pushpull.rs) | Bounded voxel commits and previews exist; grip-point parity, robust topology healing, arrays, and typed transforms remain incomplete. |
| Edit history and persistence | **Partial** | [`src/builder.rs`](src/builder.rs), [`src/world.rs`](src/world.rs), [`src/settings.rs`](src/settings.rs) | Batched edits and save paths exist, but every semantic, voxel, material, and link-index mutation is not yet proven as one durable command across all tools. |
| Material authoring | **Partial** | [`src/textures.rs`](src/textures.rs), [`src/blocks.rs`](src/blocks.rs) | Runtime material handling is bounded; complete cross-restart custom-source identity remains an explicit open contract. |
| Localization | **Planned** | UI strings remain distributed across the application. | No public claim of a complete language layer is made. |
| Responsive and visual evidence | **Contract active** | [`docs/RESPONSIVE_VISUAL_QA.md`](docs/RESPONSIVE_VISUAL_QA.md), [`src/qa.rs`](src/qa.rs) | A source-level or headless pass does not replace matched native screenshots and telemetry. |

The detailed interaction gap analysis lives in
[`docs/SKETCHUP_EQUIVALENCE_AUDIT.md`](docs/SKETCHUP_EQUIVALENCE_AUDIT.md).
The whole-engine acceptance boundary lives in
[`docs/ELITE_WORLD_SYSTEMS_STANDARD.md`](docs/ELITE_WORLD_SYSTEMS_STANDARD.md).

## Milestone 1: interaction correctness

Make pointing, inference, and mode transitions predictable before increasing
tool count.

- Keep raw picking separate from inference ranking.
- Prefer the visible mouse position for pointer tools and reject stale hidden
  cursor coordinates.
- Expose endpoint, midpoint, face, edge, axis, and reference-chain decisions
  in the preview instead of silently snapping.
- Make cancel, pause, menu, observer, and build transitions preserve one clear
  cursor policy and one visible status.
- Add adversarial tests for occlusion, large faces, chunk boundaries, narrow
  viewports, and focus loss.

Done means the same intended point produces the same preview and committed
cell, with no fallback to a hidden crosshair or stale pointer location.

## Milestone 2: selection and direct manipulation

Turn the existing semantic and transform foundations into a dependable
select-first workflow.

- Add crossing-window selection and explicit add/remove/toggle semantics.
- Complete nested component and edit-context priority.
- Give move, rotate, and scale stable grip points with inference-visible
  previews and typed deltas.
- Preserve block, material, semantic identity, and selection state through
  transforms.
- Add copy, array, make-unique, and duplicate workflows only after the base
  transform transaction is proven atomic.

Done means a selected object can be transformed across chunk boundaries,
undone, redone, saved, and reloaded without identity drift or partial edits.

## Milestone 3: topology, history, and durability

Unify authoring mutations under a single fail-closed transaction boundary.

- Route voxel cells, semantic links, material identity, and document changes
  through one bounded history record per user action.
- Make push/pull and opening tools reject ambiguous or non-manifold outcomes
  before mutation.
- Define exact rollback behavior for interrupted saves and missing custom
  material sources.
- Test failed writes, stale epochs, extreme signed coordinates, undo-cap
  pressure, and restart reconstruction.

Done means no supported tool can leave the voxel world and semantic document
desynchronized, even after a rejected operation or failed persistence step.

## Milestone 4: interface, accessibility, and localization

Polish the authoring surface without hiding capability behind undocumented
shortcuts.

- Keep tool names action-oriented and group related tools without duplicating
  their purpose.
- Provide visible keyboard alternatives, focus order, high-contrast states,
  reduced motion, and supported viewport limits.
- Move player-facing strings behind a versioned localization layer with a
  loud missing-key fallback.
- Validate 320 x 480 through ultrawide layouts at the documented DPI matrix.

Done means the core authoring loop remains legible and reachable across the
declared viewport, DPI, input, and language matrix.

## Milestone 5: evidence-backed acceptance

Promote interaction claims only from one identified build and reproducible
native routes.

- Record the source revision, binary SHA-256, world identity, route, viewport,
  DPI, tool state, and known limitations.
- Pair screenshots with telemetry and inspect both; average FPS alone is not a
  visual or responsiveness verdict.
- Exercise Natural and Astral profiles, existing and fresh worlds, focus loss,
  pause/resume, save/load, undo/redo, and pressure states.
- Keep rejected frames and unresolved findings out of the public gallery until
  the same-binary acceptance contract passes.

Done means each public capability statement points to source or to a bounded,
reproducible evidence artifact and states what remains unproven.

## Verification baseline

Use the repository gates before native visual inspection:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features
cargo test --workspace --quiet
cargo check --target wasm32-unknown-unknown --bin voxel-native
python -B tools/publication/validate_repository_presentation.py
```

Changes to interaction, layout, rendering, streaming, or persistence also need
the relevant focused tests and the native route matrix. Generated QA worlds,
screenshots, control files, and local settings are evidence inputs or local
state; they are not source changes.

## Guardrails

- Preserve signed integer world identity and Euclidean coordinate mapping.
- Keep work, memory, queue, entity, and history limits explicit.
- Reduce detail before dropping authority or silently shortening the horizon.
- Do not mutate user saves to manufacture test evidence.
- Keep experimental representations reversible and feature-gated.
- Treat a beautiful screenshot, a passing unit test, and a clean benchmark as
  different forms of evidence; no one form substitutes for the others.
