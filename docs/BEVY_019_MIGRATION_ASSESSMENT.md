# Bevy 0.19 Migration Assessment

Status: current-platform risk and staged upgrade plan, 2026-08-09. No dependency
or lockfile change has been made by this assessment.

## Finding

Voxel-Native currently pins Bevy 0.14 and `bevy_egui` 0.28. Bevy 0.19 was
released on 2026-06-19, so the engine is five breaking Bevy releases behind the
current official release. This does not make the existing renderer invalid,
but it is a material platform, maintenance and ecosystem risk.

An immediate version-number edit is rejected. Bevy remains pre-1.0 and its own
project warns that releases contain breaking changes. The official migration
guides show successive changes to rendering bundles, entity relationships,
fallible query access, events/messages, state transitions, camera targets,
materials, text, bloom, resource storage and the render graph. Migrating all of
those alongside active world-continuum changes would make regressions hard
to attribute and would put save/UI/visual evidence at risk.

## Measured local migration surface

This inventory was produced with read-only `rg` counts over `src/` and `tests/`
on 2026-08-09. It is a lower bound: a name count does not include transitive
type/signature changes.

| Current 0.14 pattern | Occurrences | Likely migration area |
| --- | ---: | --- |
| `PbrBundle` | 122 | 0.15 required mesh/material components |
| `MaterialMeshBundle` | 4 | 0.15 component-based mesh spawning |
| camera/light bundles | 11 | component spawning and later camera changes |
| `SpatialBundle` | 13 | transform/visibility required components |
| `NodeBundle` / `TextBundle` | 27 | UI and text migrations |
| `Style { ... }` | 43 | UI component/layout changes |
| `EventReader` / `EventWriter` | 19 | event/message API changes |
| `get_single()` / `get_single_mut()` | 122 | 0.16 fallible query migration |
| legacy `Color::rgb/rgba` | 10 | color API and color-space audit |
| `Handle::weak_from_u128` | 1 | 0.16 `weak_handle!` migration |
| `Mesh::new` | 6 | asset-usage/signature audit |
| custom `AsBindGroup` / `ShaderRef` | 6 | material and shader compatibility |
| `AlphaMode` | 45 | transparency/order visual regression |

The project also has native/wasm dependency splits and a third-party egui
integration. A matching `bevy_egui` release and its feature matrix must be
verified at each step; it may not be guessed from the Bevy version.

## Candidates

### A. Big-bang 0.14 to 0.19

This reaches the current API fastest on paper. It is rejected because hundreds
of mechanical errors, renderer changes and subtle behavior changes would land
in one unreviewable diff. Visual differences in bloom, text, picking, state
transitions or entity lifecycle could pass compilation while breaking the
simulator.

### B. Stay on 0.14 indefinitely

This preserves short-term velocity and known behavior. It is rejected as the
long-term plan because upstream fixes, documentation, plugins and renderer
capabilities increasingly target newer Bevy versions. Security or driver fixes
would become a private backport burden.

### C. Extract a separate pure world core first

The new virtual hierarchy, implicit voxel, morphogenesis and direct-bridge
modules already have no Bevy dependency. Moving more authority into a separate
crate could reduce future engine coupling. A full crate split now is deferred:
it would add package/build churn before the current modules are integrated.
The dependency-free boundary remains a design requirement during migration.

### D. Sequential release stepping with evidence at every step (chosen)

Migrate 0.14 -> 0.15 -> 0.16 -> 0.17 -> 0.18 -> 0.19 on an isolated branch.
Each step follows only its official guide, compiles and runs the same evidence
matrix, then becomes a reviewable checkpoint. Mechanical changes are separated
from deliberate renderer or UI adoption. The current 0.14 branch remains a
working rollback until 0.19 meets or beats the release gates.

## Staged execution

### Stage 0 — freeze evidence, not development

1. Record the exact native, wasm, full-test and release-build transcript.
2. Capture Natural/Astral hero, 8-km streaming and required viewport evidence.
3. Record p50/p95/p99 frame time, queue peaks, entity/mesh counts, executable
   size and warm/cold startup.
4. Record save/load/undo hashes using copied disposable QA worlds only.

### Stage 1 — 0.14 to 0.15

Mechanically replace deprecated render/UI bundles with required components,
audit render-world entity lifetime, custom material extraction and screenshot
APIs, and pin the compatible egui bridge. Do not redesign visuals in this step.

### Stage 2 — 0.15 to 0.16

Handle fallible `single` access explicitly, migrate hierarchy relationships and
the weak shader handle, and verify despawn/selection/undo semantics. A blanket
`unwrap` conversion is not accepted.

### Stage 3 — 0.16 to 0.17

Migrate the event/message model and audit any stored or ordered `Entity` values.
No save format may persist Bevy's unstable entity bit representation.

### Stage 4 — 0.17 to 0.18

Audit camera render targets, material settings, state self-transitions, mutable
mesh/AABB behavior and input features. `NextState` sites must choose deliberately
between an actual self-transition and `set_if_neq`.

### Stage 5 — 0.18 to 0.19

Resolve resources-as-components, the render-graph-as-systems change, Parley
text migration, feature collections, world-serialization names and linear
bloom. Types may not remain both resources and components. Linear-bloom and
text changes require image comparison, not only compilation.

### Stage 6 — adopt new capabilities separately

Only after parity is green may the project evaluate new 0.19 features. Feature
adoption gets its own baseline/candidate/benchmark diff so an API migration
cannot hide a performance or style change.

## Per-step acceptance gate

- `cargo fmt --all -- --check`, clippy, native check, full tests and wasm check
  pass with no newly ignored failure.
- Save metadata and semantic world hashes round-trip in disposable copies.
- Selection/edit/undo, flight, weapons, bots, Agent Control and Mission Control
  smoke routes behave the same unless a documented migration intentionally
  changes them.
- The viewport/DPI matrix has no new overlap, text reflow or input-focus defect.
- Natural and Astral comparison captures show no unexplained missing geometry,
  material, transparency, fog, bloom, shadow or screenshot failure.
- p95 frame time, cold startup and streaming queue peaks do not regress by more
  than five percent without a measured explanation and an explicit decision.
- Each version step is independently revertible. Cargo and source changes are
  scoped; saves, QA evidence and unrelated dirty files are excluded.

## Official references

- Bevy 0.19 release: https://bevy.org/news/bevy-0-19/
- 0.14 to 0.15: https://bevy.org/learn/migration-guides/0-14-to-0-15/
- 0.15 to 0.16: https://bevy.org/learn/migration-guides/0-15-to-0-16/
- 0.16 to 0.17: https://bevy.org/learn/migration-guides/0-16-to-0-17/
- 0.17 to 0.18: https://bevy.org/learn/migration-guides/0-17-to-0-18/
- 0.18 to 0.19: https://bevy.org/learn/migration-guides/0-18-to-0-19/
- Bevy release process: https://bevy.org/learn/contribute/project-information/release-process/

## Decision

Do not combine the Bevy migration with world-continuum integration. First
establish the pure-module tests and real-engine baseline, then perform the
sequential migration separately. Being current is part of the elite target;
keeping the engine runnable and persisted worlds intact is the first gate of
reaching it.
