# Repository Guidelines

## Project Structure & Module Organization

Voxel-Native is a Rust 2021 binary using Bevy 0.14 and wgpu. `src/main.rs`
registers the engine plugins. World authority and streaming live primarily in
`src/world.rs`, `src/chunk.rs`, `src/terrain.rs`, `src/mesher.rs`, and
`src/planetary_streaming.rs`; editing spans `src/sketch_model.rs` and
`src/sculpt/`; presentation spans `src/hud.rs`, `src/toolbelt.rs`,
`src/mission_control.rs`, and `src/theme.rs`. Unit tests are colocated with
modules; cross-module invariant tests belong in `tests/`. Research and
acceptance contracts are under `docs/`, especially
`ELITE_WORLD_SYSTEMS_STANDARD.md`, `VOXEL_DISCOVERY_ATLAS.md`, and
`RESPONSIVE_VISUAL_QA.md`.

## Build, Test, and Development Commands

```powershell
cargo run                         # incremental debug engine
cargo run --release               # optimized engine
cargo fmt --all -- --check        # CI formatting gate
cargo clippy --all-targets --all-features
cargo test --workspace --quiet    # complete registered suite
cargo test --bin voxel-native sculpt::transform::tests
cargo check --target wasm32-unknown-unknown --bin voxel-native
```

For deterministic visual evidence, build once and run
`scripts/planetary-streaming-qa.ps1` with a unique QA world. Inspect both the
screenshots and `report.ron`; never infer success from average FPS alone.
Use `scripts/elite-release-gates.ps1` for the combined non-visual gate. It also
rejects staged save/QA artifacts and verifies the documented viewport/DPI
matrix without launching the engine.

## Coding Style & Testing

Use rustfmt defaults. Preserve checked integer arithmetic, Euclidean division
for signed world coordinates, deterministic ordering, explicit epochs, and
compile-time work/memory caps in streaming or IPC code. A novel optimization
must document its baseline, alternatives, fixed budget, measured distribution,
failure mode, and rollback boundary. Add tests for negative/extreme coordinates,
stale async results, order independence, pressure saturation, and exact byte or
population ceilings where applicable. Visual changes also require the viewport
matrix and Natural/Astral route evidence defined in the QA contract.

## Agent Safety & Coordination

The worktree can contain user-owned modified, deleted, and untracked saves.
Never delete, restore, reset, move, regenerate, or broadly stage `saves/`,
`qa_runs/`, `agent_runs/`, personal media, or unrelated dirty files. Coordinate
file ownership before parallel edits. Only one agent may launch the graphical
engine/GPU QA at a time; compilation and pure tests do not grant permission to
alter saves. Experimental summaries are reconstructible caches and must not own
authoritative edits. Capability/readiness reports fail closed: do not label a
fallback transport as direct.

## Commit & Pull Request Guidelines

Recent history favors concise imperative subjects, commonly `feat:` or `fix:`.
Stage only reviewed paths; never use broad staging in this dirty workspace.
PR evidence should list exact tests, profiles, routes, viewports, measurements,
known limits, and excluded user data.
