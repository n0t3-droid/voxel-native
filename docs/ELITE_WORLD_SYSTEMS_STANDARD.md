# Voxel-Native Elite World Systems Standard

Status: hard acceptance contract, August 2026. This is a target specification,
not a claim that every gate already passes.

## The raised objective

Voxel-Native is not aiming for one impressive map, one cinematic screenshot,
or one unusually fast benchmark. The product target is a coherent editable
world continuum: a user can fly kilometres, land anywhere, edit or construct
something, leave, return, change tools, resize the window, switch profiles and
continue without the world losing identity or the engine losing control of its
budgets.

The highest useful expectation is not "unlimited dense voxels at zero cost".
That is physically impossible on finite hardware. The stronger engineering
goal is bounded cost with graceful representation changes: exact voxels where
interaction requires them, conservative multiresolution summaries in the
midfield, constant-count macro geometry at the horizon, and analytic bodies at
planetary scale. User edits and semantic object identity remain authoritative
through every representation.

No feature is elite because it is novel. It becomes eligible only after it is
measured, bounded, deterministic, reversible, visually inspected and connected
to the whole-world contract.

## Acceptance ladder

Every system must state which levels it passes. A higher level includes every
lower level; a beautiful Level 4 feature that fails Level 1 is not shippable.

### Level 0 — data safety and reversibility

- Automated tests and QA runs use explicit disposable worlds or read-only
  sources. They never reuse, delete, reset, move or overwrite user saves.
- Existing dirty files are treated as user-owned unless authorship is proven.
- Every persisted format has a version, validation path and fail-closed error.
- Destructive operations require an exact target and a recovery story.
- Experimental representations are caches; the authoritative edit log is not
  evicted with them.

### Level 1 — numerical and semantic correctness

- World indexing uses signed integer coordinates and Euclidean division across
  negative positions. Floating-point values are local render coordinates, not
  planetary identity.
- Public APIs fail closed at integer extremes, subnormal inputs, invalid
  dimensions, stale epochs and corrupt payload lengths. Debug overflow or NaN
  propagation is a test failure.
- Selection, move, rotate, scale, delete, undo and redo update voxels, semantic
  links and document state atomically.
- A semantic object has one stable identity. Touching objects do not merge by
  accident, and array copies do not inherit the original identity.

### Level 2 — hard boundedness

- The system publishes compile-time or configuration-time caps for resident
  bytes, entities, tasks, queue entries, work per frame and generated output.
- Resident work cannot grow with lifetime distance travelled. A 20,000-km
  coordinate route must settle back to the same bounded population as origin.
- Pressure policies reduce detail, cadence or promotion radius in a documented
  order. They do not silently shorten the far horizon or discard authority.
- Benchmarks report toolchain, target, build profile, hardware, route and a
  distribution such as median/p95/p99. A single best sample is not evidence.

### Level 3 — global causal coherence

- Geology constrains elevation and strata; elevation constrains drainage;
  drainage and climate constrain moisture and soil; those constrain vegetation,
  routes and settlement suitability.
- Adjacent macro tiles agree at their shared border independently of generation
  order, worker completion order and camera direction.
- Hydrology has explicit downhill, sink, conservation and cross-tile contracts.
- Natural and Astral profiles are distinct causal worlds, not palette swaps.
- A seed plus version reproduces macro fields, species guilds and landmark
  placement. Changes to world grammar require a migration or new version.

### Level 4 — representation continuity

- Interaction, Near, Mid, Far and Celestial tiers describe the same logical
  world feature with stable identity and compatible quantisation.
- A coarse parent is ready before a finer child may disappear. Teleports and
  stale async results cannot reveal holes.
- Promotion and demotion are temporally stable. Morph bands, cross-fades or
  conservative parent coverage prevent popping and silhouette discontinuity.
- Sparse edits invalidate a bounded ancestor chain and remain visible in
  distant summaries without loading all source chunks.
- Rendering proxies never become collision or edit authority unless they prove
  the same conservative coverage contract as the interaction tier.

### Level 5 — perceptual and temporal fidelity

- A feature is reviewed from gameplay cameras while still, walking, flying,
  rotating, accelerating, teleporting and crossing tier boundaries.
- Lighting preserves material readability in bright, dark, foggy and emissive
  scenes. Bloom and saturation may enrich a scene but may not erase shape.
- Vegetation motion is species-, branch- and height-aware, has coherent gusts
  and local phase variation, and remains a visual force unless an explicit
  physics coupling is enabled. Wind animation must not perturb shuttle flight.
- Water, atmosphere, fog, clouds, shadows, emissives and exposure are checked
  for temporal shimmer, clipping, banding, z-fighting and abrupt transitions.
- Hero compositions are world-system evidence only when the same rules remain
  convincing away from the hero location.

### Level 6 — responsive interaction quality

- No controls, labels or status regions overlap, clip or become unreachable in
  the required viewport and DPI matrix.
- Selection behaves semantically by default; Edit Object exposes controlled
  part-level operations without losing the object scope.
- Tool changes, Escape, undo/redo, focus loss and history rejection leave no
  stale selection or edit mode.
- Every interaction gives immediate preview, valid/invalid feedback and a
  deterministic commit. Long work is budgeted or asynchronous without freezing
  input.
- Flight, camera and building controls are tested together with vegetation,
  weather, streaming pressure and UI, not only in isolated unit tests.

### Level 7 — agent observability and parity

- Every agent instance publishes the same versioned capability manifest. Old,
  incomplete or foreign-fleet manifests fail closed as a power mismatch.
- Mission Control distinguishes command transport, observation transport and
  authority. It never labels a fallback path as direct shared memory.
- Command and event channels have bounded payloads, queues and per-frame drain.
  Epoch, nonce, sequence, expiry and corruption checks are mandatory.
- A user can observe many agents without each spectator view multiplying full
  world simulation cost. Views share snapshots or bounded render products.
- Join mode makes user input and engine evidence visible to an agent through an
  explicit authority boundary; it does not grant hidden destructive access.

### Level 8 — resilience under adversarial state

- Soak tests cover rapid teleports, window resizing, focus churn, tool churn,
  queue saturation, task cancellation, stale completion, device pressure and
  malformed persisted or IPC data.
- A rejected history record, full queue, unavailable GPU feature or exhausted
  cache produces a safe no-op, fallback representation or visible diagnostic.
- Recovery does not depend on hash-map iteration order, wall-clock timing or a
  particular worker completion sequence.
- Telemetry counts both current and peak populations, deferred work, dropped
  stale work, corruption and recovery events. Unknown state is reported as
  unknown, never as healthy.

### Level 9 — release evidence

- Native check, focused tests, full suite and the supported WebAssembly check
  pass from a clean command transcript.
- Real-engine QA covers Natural and Astral profiles, short interaction routes,
  8-km flights and a 30-km stress route after warm-up.
- Screenshots and telemetry are phase-named, timestamped and tied to a build.
- Known limits and deferred candidates are documented beside the measured wins.
- Git staging is path-curated. Saves, QA worlds, logs, personal media, secrets
  and unrelated dirty files are excluded. Publishing happens only after the
  scoped diff is reviewed.

### Level 10 — recursive discovery and fleet intelligence

Level 10 is the raised research target, not a current release claim. It means
the engine, QA system, and agent fleet improve their ability to discover the
next valuable problem instead of merely accumulating features.

- Every long visual route produces a bounded anomaly ledger spanning visuals,
  mechanics, UI, streaming, authority, lifecycle, and performance. A pass in
  the named feature cannot erase an unrelated observation.
- Candidate improvements are compared against a measured baseline and at least
  two credible alternatives. The chosen slice must state its global-world
  effect, fixed cost, failure mode, rollback, and the evidence that would make
  the team reject it.
- Agents share one versioned capability and evidence schema. New agents inherit
  current budgets, safety boundaries, QA routes, known failures, and release
  gates rather than relearning them privately in disconnected tasks.
- Automated inspection may create reconstructible caches, disposable QA worlds,
  reports, and proposed patches; it may never gain implicit authority to delete,
  rewrite, publish, or reinterpret user data.
- A representation change is evaluated from Interaction through Celestial
  scale. A local hero asset is not accepted as a world-system advance until its
  grammar, identity, streaming, and degradation rules work away from the hero
  location.
- Novelty remains subordinate to truth: a simpler bounded path that is faster,
  clearer, and easier to verify outranks a sophisticated path whose cost or
  correctness is hidden.
- The evidence pipeline itself is adversarially tested. Reports distinguish
  desired from resident state, warm-up from active routes, observation from
  causation, and unavailable evidence from a zero or a pass.

The Level 10 loop is complete only when it leaves the product easier for the
next human or agent to understand, test, challenge, and safely improve.

The current Bevy 0.14 to 0.19 platform gap is handled by the isolated,
sequential evidence lane in
[`BEVY_019_MIGRATION_ASSESSMENT.md`](BEVY_019_MIGRATION_ASSESSMENT.md). A
dependency upgrade may not be mixed into world-system integration simply to
make the version number look current.

## Required viewport and display matrix

Responsive visual QA is a recurring release gate, not a one-time UI cleanup.
At minimum, the main HUD, toolbelt, Agent Control and Mission Control must be
checked at:

| Logical viewport | Shape | Required checks |
| --- | --- | --- |
| 320x480 | narrow/portrait floor | safe compact reflow; core controls reachable |
| 800x600 | minimum legacy desktop | no status/tool flyout collision |
| 960x540 | minimum 16:9 | no overlap; essential controls reachable |
| 1280x720 | common low | legible primary status and tool previews |
| 1920x1080 | baseline | intended hierarchy and composition |
| 2560x1440 | high | panels do not drift or waste interaction distance |
| 3440x1440 | ultrawide | world view expands without edge UI detaching |
| other portrait/narrow diagnostic | adverse | safe reflow or explicit supported-limit notice |

Run the matrix at 100%, 150% and 200% OS scale where the platform exposes it.
Text expansion, long agent names, large counts, warnings and missing-data states
are part of the matrix. Pixel-perfect screenshots alone are insufficient: input
hit regions and keyboard/controller reachability must also be exercised.
The detailed capture and causal-inspection procedure is maintained in
[`RESPONSIVE_VISUAL_QA.md`](RESPONSIVE_VISUAL_QA.md).

## Performance objectives and honest interpretation

These are target gates for the August 2026 reference machine, not universal
promises. Each run must publish the exact profile and whether the target was
met.

- Balanced 1080p flight target: median frame <= 16.7 ms and p95 <= 33.3 ms
  after warm-up, with no sustained unbounded backlog.
- Minimum supported profile target: p95 <= 33.3 ms at its documented viewport
  while preserving interaction authority and the far-world silhouette.
- Streaming install work targets a small fixed fraction of a frame; exceptional
  cold teleport work may span frames but must preserve parent coverage and
  responsive input.
- One cold spike, average FPS or a synthetic microbenchmark cannot establish
  perceptual stability. Frame-time traces, queue peaks and visual evidence are
  evaluated together.

If a target fails, the result is still useful evidence. It triggers candidate
comparison and a measured correction; it does not justify hiding telemetry or
lowering the target after the fact.

## Novel-solution decision record

Every non-obvious optimisation or data structure records:

1. inputs, outputs, invariants and the real hot path;
2. the measured current baseline;
3. two to four candidates with portability, complexity and safety costs;
4. explicit rejection of pseudo-novel or unsafe choices;
5. the chosen bounded implementation and regression tests;
6. before/after metrics and assumptions that would invalidate the result;
7. a reversible integration boundary until whole-engine evidence is green.

Unsafe Rust, undefined behaviour, GPU-only authority and undocumented binary
layout tricks are not accepted as intelligence. A simpler approach that wins
the real metric is the more advanced solution.

## Definition of "top elite"

The project reaches this standard when a user can start from a normal desktop,
observe or join agents, fly through a globally coherent world, build and edit
semantic voxel objects, return across large distances, change display size and
quality profile, and keep both their work and the engine's budgets intact. The
experience should remain readable, responsive and visually alive while every
approximation is explicit about what it can and cannot represent.

That definition deliberately combines appearance, mechanics, scale, tooling,
agents, persistence and recovery. Excellence in only one of those dimensions
is an intermediate milestone.
