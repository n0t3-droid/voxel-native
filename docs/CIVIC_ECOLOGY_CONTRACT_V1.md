# Civic Ecology Contract V1

Status: implementation contract for `src/villagers.rs` and the civic population
embedded in `BotWorldSave`. This document separates resident code that exists
today from release evidence and future work. A requirement in an acceptance
table is not, by itself, a claim that a fresh QA run has passed.

## Purpose and non-commerce boundary

Civic Ecology adds persistent settlement inhabitants whose visible life is
organized around home, stewardship, commons, shelter, rest, learning, play,
local exploration, and response to weather. It is deliberately separate from
the construction companions in `bots.rs`: construction companions may own
authorized voxel-edit commands; civic residents may observe the world but may
not mutate it.

The V1 schema contains no trade, price, currency, market, merchant, offer,
inventory, or exchange state. There is no commerce UI or commerce decision.
The serialized-schema regression test explicitly rejects the trade, price,
currency, market, merchant, and offer vocabulary; schema inspection also
confirms that no inventory or exchange state exists. Adding economic state
would therefore be a new contract version, not an incidental extension of V1.

The civic system also does not implement reproduction, population growth,
combat, global society simulation, or Minecraft parity. Its goal is a small,
bounded, original social ecology—not a replica of another game's villagers.

## Inputs, outputs, invariants, and success metrics

The system accepts one active world-generation identity, the first saved
settlement, resolved voxel samples, biome and environmental samples, time of
day, precipitation, and player position. It produces a deterministic saved
population plus disposable ECS visual projections.

The invariants are:

- `CivicPopulation` inside `BotWorldSave` is authoritative; a rendered entity
  is never authoritative for identity, activity, position, memory, or bonds.
- A resident never buys, sells, prices, offers, trades, or issues voxel edits.
- A route traverses only voxel cells whose support, feet, and head samples are
  resolved. Missing coverage is unknown, not air.
- A saved generation identity mismatch blocks simulation and presentation; it
  never silently regenerates or reinterprets the population.
- Ordering and tie-breaking are deterministic, including resident IDs,
  normalization, utility ties, A* open-set ordering, sparse social pairing, and
  LOD ranking ties.
- Population, cognition, navigation, social memory, visual work, and material
  allocation have explicit ceilings.

The functional success metric is a reproducible population of 12 residents for
the primary settlement with stable identities, non-commerce activities,
loaded-voxel movement, sparse social continuity, and profile/biome-coupled
visual identity. The safety metric is zero traversal through unresolved voxels
and zero civic voxel writes. The performance metric is bounded work per frame
and per request, followed by measured frame-time and allocation evidence in the
QA matrix below. V1 supplies code-level ceilings; this document does not invent
an unmeasured frame-time improvement.

## Saved authority and identity lifecycle

`BotWorldSave` version 3 contains a serde-defaulted `civic_population` with its
own schema version 1. Native saves remain in the existing bot journal; browser
builds use the existing browser-storage path. Older saves that do not contain
the field deserialize to an empty, uninitialized population and can be seeded
after the settlement is available.

On first initialization, V1 seeds the **first settlement only**. Each resident
receives:

- a stable high-bit-namespaced ID derived from world seed, world profile,
  scenery quality, terrain grammar version, settlement ID, ordinal, and a
  deterministic collision nonce;
- a deterministic original name;
- home, work, commons, and shelter anchors derived from fixed civic offsets and
  the world's sampled surface height;
- one of eight original callings and one of three life stages;
- an environment-derived culture from the local biome and environmental sample;
- bounded needs, memories, relationships, movement progress, and route-failure
  state, including the resident's last route failure, retry deadline, and
  deferred pre-wait activity.

The generation identity is saved with the population. If the active identity
differs, authority becomes `IdentityBlocked`, the failure becomes
`GenerationIdentityMismatch`, and transient simulation authority is suspended.
The accumulator, pending movement, logical/visual selections, queued requests,
and cached paths are cleared while the saved population is retained. If a later
active world exactly matches the saved generation identity, authority is
restored to `Active` and the mismatch failure is cleared without reseeding or
rewriting residents. V1 has no migration, reinterpretation, or automatic
unblock across *different* identities; exact-identity restoration is the only
recovery path.

Normalization removes zero IDs, duplicates, residents whose settlement no
longer exists, relationships to missing/self residents, over-limit records,
and over-limit blackboard notices. Residents are sorted by `(settlement_id,
resident_id)`. If no valid settlement remains, residents are cleared and the
population returns to `Uninitialized` with `InvalidSettlement` recorded.

Changes mark the existing bot save authority dirty. Identity transitions,
activity/target changes, route failures, endpoint corrections, blocked/arrival
events, and reconciliation corrections are marked immediately. Movement-only,
needs-only, and social-only changes set a pending flag and mark the authority
dirty at the five-second civic checkpoint. World unload uses the existing
ordered bot-save lifecycle before civic visuals are destroyed.

ECS roots and body parts are projections reconstructed from the saved records.
They may be culled, replaced by proxies, animated, or despawned without changing
resident identity.

## Deterministic fixed-point cognition

Time is wrapped to 24 hours and rounded to an integer minute. Precipitation is
the rounded maximum of rain and snow intensity on `[0, 1]`, represented as an
integer `P` on `[0, 1000]`. Every need is a deficit on `[0, 1000]`, where a
larger value is more urgent.

For candidate activity `a`, the implemented saturating `i64` utility is:

```text
U(a) = 1000 S(a, stage, minute)
     + N(a, energy, belonging, safety, purpose, curiosity, P)
     + 256000 [a = current_activity]
     - 8000 D1_xz(current_cell, target(a))
     - 1250 P [a is outdoor]
```

`D1_xz` is the Manhattan distance in the horizontal voxel plane. The need term
is exactly:

| Activity | `N(a, ...)` |
| --- | ---: |
| Rest at home or recover | `1900 energy + 500 safety` |
| Prepare | `500 purpose` |
| Work | `1750 purpose` |
| Socialize | `1850 belonging` |
| Share knowledge | `1100 belonging + 850 purpose` |
| Play | `1500 curiosity + 750 belonging` |
| Inspect settlement or wander locally | `1500 curiosity + 400 purpose` |
| Seek shelter | `2200 safety + 2400 P` |
| Wait for coverage | `0` (not part of the V1 decision set) |

The schedule compatibility `S` is project-authored and uses half-open minute
ranges:

| Activity condition | Minute range | `S` |
| --- | --- | ---: |
| Rest at home | before 06:00 or from 20:00 | 1200 |
| Prepare | 06:00–07:29 | 850 |
| Adult work | 07:30–11:59 or 14:00–17:59 | 1000 |
| Socialize | 12:00–13:59 or 18:00–19:59 | 900 |
| Elder shares knowledge | 12:00–13:59 or 18:00–19:59 | 1050 |
| Youth plays | 06:00–19:59 | 1000 |
| Inspect settlement | work windows | 600 |
| Wander locally | 06:00–19:59 | 420 |
| Recover | before 06:00 or from 20:00 | 500 |

All unmatched schedule combinations contribute zero. Equal utilities choose the
lower stable activity tag. A current activity receives hysteresis through the
`256000` term and a fixed commitment interval. Commitment lengths are 35 ticks
for rest, 15 prepare, 30 work, 24 social/teach, 20 play, 18 inspect/wander, 30
shelter, and 20 recover. At the 0.2-second logical step these correspond to 7.0,
3.0, 6.0, 4.8, 4.0, 3.6, 6.0, and 4.0 seconds. The helper retains a five-tick
value for `WaitForCoverage`, but route failures do not use that one-second
value: they set commitment to the longer retry deadline defined below.

For a resident admitted to activity selection (that is, one not still inside a
route-wait deadline), precipitation at `P >= 700` preempts every activity other
than shelter seeking. Independently, every logical tick evolves bounded
deficits: energy is `-12` while resting and `+3` otherwise; belonging is `-10` during social
activities and `+2` otherwise; purpose is `-9` during work/inspection and `+2`
otherwise; curiosity is `-8` during exploration/play/inspection and `+1`
otherwise. Safety is `+7` when `P >= 550` and the resident is unsheltered, and
`-5` otherwise. Saturation prevents underflow and overflow.

This is deterministic utility cognition, not a language model, unrestricted
planner, or claim of human intelligence.

## Loaded-voxel A* contract

The path service is a deterministic, bounded endpoint resolver followed by
four-neighbor A* over the currently resolved voxel world.

1. Both requested endpoints are first passed through `nearest_standable_cell`.
   It tests the requested column and then eight deterministic horizontal
   offsets at radius two. For each column, a standable root requires solid
   support one voxel below and air at feet and head; root-height offsets are
   probed in the exact order `0, +1, -1, +2, -2, +3, -3, +4, -4`.
2. Endpoint resolution itself does not move or rewrite the saved resident. If
   bounded A* succeeds, its corrected start is persisted as the resident's
   logical cell and updates any home, work, commons, shelter, or target anchor
   that exactly matched the old cell. Its corrected goal is then persisted as
   the target and as the anchor for the current activity. These authority
   corrections accompany a successful path result; they are not route steps
   through an obstacle. A failed path instead persists only its failure/wait
   state.
3. Reads use `voxel_at_if_resolved`. If any needed sample is unresolved and no
   standable candidate is proven, the result is `CoverageUnresolved`.
4. Horizontal A* neighbors are considered north, west, east, then south.
   Checked integer addition rejects coordinate overflow. A move may change
   height by at most one voxel.
5. Each horizontal step costs 10 plus `4 * abs(delta_y)`. The heuristic is
   `10 * Manhattan_xz`, saturated to `u32`. A* ties are ordered by total cost,
   heuristic, then deterministic `(z, x, y)` cell order.
6. Every candidate must stay within 48 voxels of the resolved route start on
   both the X and Z axes. A returned route contains at most 96 cells and a
   request expands at most 768 nodes.
7. Before at most one request is serviced per update, the queue is pruned in
   FIFO-preserving order. Missing residents, residents outside the current
   logical-active set, future-tick requests, and requests whose saved start or
   goal no longer matches are discarded. Enqueue replaces an older request for
   the same resident before enforcing the 32-request ceiling.
8. Before each logical cell transition, standability is checked again. An edit
   that blocks the next cell invalidates the path, enters bounded route-wait,
   increments a capped resident failure count, and records a bounded route
   memory/notice. The resident never traverses the newly invalid cell.

Freshly seeded or loaded cells are also reconciled independently of path
requests in a round-robin pass limited to two residents per update. This pass
uses the same nine-column and nine-height-probe standability proof. A resolved
correction resets movement progress, updates matching anchors, and invalidates
stale path state. Unresolved reconciliation leaves the saved cell untouched.
LOD admission independently requires a resolved standable logical cell, so an
unproven resident is not rendered as if valid.

Failure behavior is explicit:

| Condition | V1 behavior |
| --- | --- |
| Unresolved endpoint or route coverage | persist `CoverageUnresolved`, enter `WaitForCoverage`, and cache no route |
| No standable endpoint, no route, or goal outside the 48-voxel box | persist `NoRoute`, enter `WaitForCoverage`, and move no further |
| 768 expansions reached | internal `BudgetExhausted`; persist `PathBudgetExhausted` and enter route-wait |
| Reconstructed route would exceed 96 cells | internal `PathTooLong`; persist `PathBudgetExhausted` and enter route-wait |
| Queue already contains 32 other resident requests | enqueue fails, records path-budget pressure, and enters route-wait |
| Installing a non-empty path for a new resident when 64 paths are cached | deterministically evict the lowest resident ID before insertion |
| Successful path result contains no cells | remove only that resident's cached path; do not evict another resident |
| Next cell changed after planning | remove route, persist `NoRoute`, record `RouteBlocked`, and retry after backoff |
| Missing, inactive, future, or stale request | prune without applying stale work |

`WaitForCoverage` is the serialized name for the generic V1 route-wait state.
It is assigned for `CoverageUnresolved`, `NoRoute`, and
`PathBudgetExhausted`, even when coverage itself is present. The previous
activity is stored as `deferred_activity`, but expiry performs a fresh bounded
utility decision rather than unconditionally restoring that activity.

Every failure increments `route_failures` to the resident ceiling of 15. For
coverage failures the first retry delay is 10 logical ticks and for no-route or
path-budget failures it is 40 ticks. The delay doubles with consecutive
failures, with the exponent capped at four: exact maxima are 160 ticks for
coverage and 640 ticks for no-route/path-budget failures. At 0.2 seconds per
logical tick these eligibility-deadline ranges are 2–32 seconds and 8–128
seconds; inactive or round-robin-delayed residents may be reconsidered later.
Retry deadlines use saturating `u64` addition. A successful path result clears
the resident failure, deadline, and deferred activity.

Movement accrues an integer 1,400 millimetres per second at the 0.2-second
logical step and crosses a voxel boundary after accumulating 1,000 millimetres.
The rendered root interpolates toward the saved logical cell; render smoothing
does not authorize a logical move.

## Sparse social continuity

Social updates occur only when the logical tick is divisible by 10. Eligible
residents must be socializing, sharing knowledge, or playing and be within one
horizontal Manhattan cell of their own commons anchor. Eligible records are
sorted by resident ID, partitioned into disjoint pairs, and limited to four
pairs per invocation. This avoids an all-pairs scan; the fixed per-resident
limits keep relationship state sparse for larger populations without claiming
that a two- or three-resident population cannot form a complete small graph.

Each resident retains at most 12 relationships and 12 memories. A new bond
starts at familiarity 120 and trust 100; a repeated meeting adds 18 familiarity
and 8 trust, saturated at 1,000. When full, the least familiar/oldest/lowest-ID
bond is replaced deterministically. Knowledge-sharing memories are newest-first
and truncated to the same fixed bound. The shared blackboard retains at most 16
notices; aggregate counters saturate rather than wrap.

V1 eligibility proves proximity to each resident's own commons, not mutual
face-to-face distance between the pair. Spatial co-presence validation and
settlement-local grouping are required before social simulation is expanded
beyond the primary seeded settlement.

## Visual and material identity

The visual language is original and procedural. A detailed Civic Weaver uses
three shared primitive meshes (cube, sphere, and cylinder) to build a rounded
head, articulated separate arms and legs, torso, collar, diagonal sash, and two
emissive eyes. Arms are not folded; there is no long-nose or robe silhouette.
Activity animation supplies gait, work gestures, social nods, breathing, and a
small rest posture. Youth, adult, and elder scales are 0.76, 1.00, and 0.94.

Six authored cultures—Riverglass, Canopy, Sunstone, Highland, Frostweave, and
Astral—select fabric, accent, skin, eye, roughness, and metallic response. The
selection is caused by the world profile, biome, temperature, mineral
resonance, and flowering resonance sampled near the resident's home. Astral
Frontier and astral biomes select Astral directly. Natural cultures distinguish
forest/jungle, arid, cold, highland/mineral, and temperate conditions.

Materials are Bevy `StandardMaterial` values, not copied bitmap textures or a
new custom shader. Colors enter through sRGB base-color constructors; emissive
eye values are supplied in linear RGBA. The cache can contain four materials
for each of six cultures plus one shared sole material: exactly 25 civic
materials at full culture coverage. It also contains exactly three shared civic
meshes.

LOD selection refreshes every 0.25 seconds. Up to 64 resolved residents within
320 metres are logically active. Up to 24 residents within 220 metres receive
visual roots; the nearest eight are detailed and the remainder are proxies.
Distance ties resolve by resident ID. At the current seed population, the close
case is at most eight detailed residents and four proxies.

The live refresh delegates to the same pure `plan_civic_lod` function exercised
by deterministic tests. Visual churn is also planned as a pure ordered delta:
existing and desired selections are compared by `(resident_id, mode)`, stale
roots are removed in resident/mode/entity order, and new roots are built in
resident/mode order. One update removes at most four stale roots and builds at
most two missing roots. A detailed/proxy mode transition removes the old mode
before the replacement becomes eligible for its bounded build pass. The build
planner counts current live roots and detailed modes before every admission, so
neither a rank swap nor delayed stale removal can exceed 24 simultaneous roots
or eight simultaneous detailed rigs.

No Minecraft model, texture, sound, animation, screenshot, UI, font, source
data, structure template, profession costume, job block, bell/golem motif,
schedule table, trade table, or exact gameplay value is included. Generic ideas
such as a daily rhythm, community commons, shelter-seeking, and biome-aware
identity are treated only as abstract genre references. Wiki content and game
assets retain their own licenses; linking to a wiki page does not license
proprietary Minecraft assets for this repository.

## Exact V1 budgets

### Persistent and logical state

| Budget | Exact ceiling or cadence |
| --- | ---: |
| Fresh primary-settlement seed | 12 residents |
| Residents in one settlement after normalization | 32 |
| Residents in one world after normalization | 128 |
| Logically active residents | 64 within 320 m |
| Decisions per logical tick | 8, round-robin |
| Fixed logical step | 0.2 s |
| Catch-up steps and exact pending step ticks per update | 2 |
| Maximum frame delta admitted to the logical accumulator | 0.4 s |
| Maximum decisions per update during catch-up | 16 |
| Cell reconciliation | 2 residents per update |
| Resident name | 48 Unicode scalar values |
| Need value | 0–1000 |
| Memory confidence | 0–1000 after normalization |
| Relationship familiarity and trust | 0–1000 after normalization |
| Memories per resident | 12 |
| Relationships per resident | 12 |
| Blackboard notices | 16 |
| Resident route-failure counter | 15 |
| Coverage retry delay | 10 ticks initially; 160 ticks maximum |
| No-route/path-budget retry delay | 40 ticks initially; 640 ticks maximum |
| Retry exponent shift | 4 maximum |
| Movement parameter | 1,400 mm/s |
| Pending needs/movement/social save checkpoint | 5.0 s, accumulated with a 0.1 s per-update delta cap |

### Navigation and presentation

| Budget | Exact ceiling or cadence |
| --- | ---: |
| Path requests queued | 32 |
| Valid path requests serviced per update | At most 1, after FIFO-preserving pruning |
| A* expansions per request | 768 |
| Cells in a returned path | 96 |
| Cached paths | 64 |
| Route box from start | +/-48 voxels on X and Z |
| Standability height probes | 9 |
| Endpoint/reconciliation horizontal candidates | 9 per endpoint or resident pass |
| Visual roots selected | 24 within 220 m |
| Detailed visual roots | 8 |
| Visual roots built per update | 2 |
| Stale visual roots removed per update | 4 |
| Visual roots synchronized per update | 16 |
| Detailed residents animated per update | 8 |
| LOD refresh | 0.25 s, accumulated with a 0.1 s per-update delta cap |
| Shared civic meshes | 3 |
| Cached civic materials | 25 |

A social invocation processes at most four disjoint pairs. Every catch-up step
carries its own logical tick into movement, relationships, and memory; two
consecutive steps therefore cannot both satisfy the ten-tick social cadence.
This keeps the per-render-update maximum at four pair updates and makes one
0.4-second admission persist the same social state as two 0.2-second
admissions. The population graph remains capped at 12 outgoing bonds per
resident.

## Baseline and four candidate architectures

### Baseline

Before this slice, the engine already had persistent construction companions,
settlement records, world identity, save ordering, and visual LOD machinery,
but it had no separate saved civilian population. The measurable civic baseline
was therefore zero residents, zero civic activities, and zero civic navigation
work. That baseline was computationally cheap but failed the feature metric.
It remains valuable as the runtime rollback state.

### Candidate A — extend construction companions

Reuse `BotAgent`, its project planner, and its construction visual rig for every
resident. This minimizes new types, but it conflates social life with voxel-edit
authority, makes “no trading/no building” harder to prove, and couples civilian
cost to a very large construction subsystem. Rejected for authority clarity.

### Candidate B — global GOAP/behavior trees plus a world navmesh

Model residents as open-ended goal planners and maintain a global navigation
mesh across streamed chunks. This could express richer plans, but invalidation
after voxel edits, global memory growth, planner branching, and far-world work
would violate V1's fixed budgets without a much larger research program.
Rejected for this version; it may inform isolated future experiments.

### Candidate C — ECS-only reactive crowd and steering

Keep all resident state on transient ECS entities and use local steering around
obstacles. This is visually direct and can scale to crowds, but reload identity,
deterministic replay, authoritative memories, exact save migration, and
fail-closed unknown coverage become weak or absent. Rejected for persistence and
truthfulness.

### Candidate D — chosen saved-data hybrid

Store bounded resident records inside the existing ordered save authority; use
integer utility cognition, a small fixed-step scheduler, resolved-voxel A*,
sparse pair updates, and disposable detailed/proxy ECS projections. This
combination wins the V1 functional metric while preserving explicit ceilings,
deterministic replay, identity blocking, and a one-variable runtime rollback.

The proof currently consists of code-level budget constants and targeted
deterministic tests. It is **not** yet a claim that Candidate D improves frame
time over a measured alternative. A release claim requires the Natural/Astral
frame-time,
queue-pressure, allocation/upload, and visual evidence below. If p95/p99 frame
time, repeated route failures, or visual churn exceed the accepted baseline,
disable the feature and revisit the design rather than hiding cost with a
shorter world horizon.

## Implemented V1 versus roadmap

| Capability | V1 state | Boundary |
| --- | --- | --- |
| Persistent resident identity and bounded memories/bonds | Implemented | Embedded in `BotWorldSave`; ECS is projection only |
| Exact-identity blocking and restoration | Implemented | Mismatch suspends transient work; the same saved identity can restore authority without reseeding |
| Primary-settlement seed | Implemented | Exactly the first settlement; not multi-settlement support |
| Time/need/weather utility decisions | Implemented | Deterministic fixed-point policy, not unrestricted planning |
| Loaded-voxel local navigation | Implemented | Four-neighbor, local 48-voxel box, no global navmesh |
| Reconciliation after streaming/edits | Implemented | Both route endpoints plus two round-robin records per update; nine local columns per proof |
| Sparse social continuity | Implemented | Four deterministic pairs per social pass; no social all-pairs scan |
| Natural/Astral resident culture and palette | Implemented | Resident materials only |
| Civic HUD summary | Implemented | Resident/work/commons/shelter/blocked counts; no dialogue UI |
| Runtime LOD and activity animation | Implemented | Pure deterministic LOD/delta plans, primitive PBR meshes, no custom skeletal rig |
| Trading/economy | Intentionally absent | Contract violation if added silently |
| Reproduction or dynamic population growth | Not implemented | Fixed seed population in V1 |
| Multi-settlement seeding/simulation | Not implemented | Schema fields/caps are future-compatible, not a support claim |
| Indoor rooms, beds, doors, or door opening | Not implemented | Shelter is a standable hub anchor, not an interior proof |
| Global far-resident simulation | Not implemented | Residents outside the 320 m active set do not advance |
| Terrain grammar, settlement buildings, roads, or vegetation generation | Not changed by this slice | Existing world systems remain authoritative |
| Water realism, weather rendering, texture pipeline, or new shaders | Not changed by this slice | Weather is read only as a cognition input |
| Async/global path planning, flow fields, or navmesh | Not implemented | One bounded synchronous A* request per update |
| Mutual co-location proof for social pairs | Roadmap | V1 proves only proximity to each resident's own commons |
| Route failure telemetry and bounded retry wait | Implemented | Persists `NoRoute`, `CoverageUnresolved`, or path-budget failure and uses generic `WaitForCoverage` with capped backoff |

## Deterministic unit acceptance

The focused command is:

```powershell
cargo test --bin voxel-native villagers::tests
```

The module currently registers 22 unit obligations. Their presence does not
replace running the command on the reviewed source revision.

| Invariant | Registered deterministic test | Acceptance |
| --- | --- | --- |
| ID namespace, replay, settlement/seed separation, order independence | `resident_ids_are_stable_namespaced_and_order_independent` | Same identity/ordinal repeats exactly; changed seed or settlement changes ID |
| World, settlement, notice, memory, relationship, scalar, failure, duplicate, and ordering ceilings | `population_normalization_enforces_global_per_settlement_and_sparse_caps` | Exact world cap 128 plus the asserted subordinate caps hold after adversarial reversed input |
| Invalid-settlement fail-closed normalization | `normalization_caps_notices_even_without_a_valid_settlement` | Residents clear, notices remain capped, and authority becomes uninitialized |
| Integer choice, night/work behavior, weather preemption, commitment | `cognition_is_fixed_point_weather_preemptive_and_commitment_stable` | Expected activity selected with deterministic tags |
| Saturation under long pressure | `needs_remain_bounded_under_long_adversarial_progression` | All five deficits remain on `[0,1000]` after 100,000 updates |
| Sparse graph | `sparse_social_pairing_never_builds_a_complete_graph` | Per-resident caps hold and total edges remain below a complete graph |
| Frame-partition invariant social persistence | `catchup_batching_preserves_exact_social_ticks_and_persistent_state` | One 0.4 s catch-up records ticks 9 and 10 exactly and serializes the same state as two 0.2 s updates |
| Loaded route and edit detour replay | `loaded_voxel_path_is_deterministic_and_detours_around_edits` | Repeated A* result is identical and excludes the edited obstruction |
| Unknown coverage and extreme coordinates | `pathfinding_fails_closed_for_unresolved_and_extreme_coordinates` | Empty coverage is unresolved; extreme inputs return an error without overflow |
| Exact expansion ceiling | `exact_astar_expansion_ceiling_returns_budget_failure` | Adversarial search returns `BudgetExhausted` at the declared limit |
| Causal profile/biome culture | `culture_is_causally_driven_by_profile_biome_and_environment` | Natural forest/desert and Astral profile select their authored cultures |
| No commerce schema and save replay | `serialized_schema_has_no_commerce_state_and_round_trips` | Forbidden commerce vocabulary absent; ID and generation identity round-trip |
| Persisted block and exact-identity restoration | `identity_block_restores_when_matching_world_returns` | Mismatch blocks; serialized residents survive; the matching identity restores active authority |
| Runtime authority suspension | `authority_suspension_clears_transient_work_and_preserves_rollback_setting` | Logical/visual/path/movement work clears while the feature-enable setting survives |
| Obstructed-start endpoint correction | `route_resolves_obstructed_start_before_astar_without_teleporting` | First safe offset is deterministic; A* begins with a cardinal step from the resolved start; matching saved anchors reconcile |
| Retry timing and overflow safety | `route_retry_backoff_is_monotonic_capped_and_overflow_safe` | Coverage and route delays double to exact maxima; `u64::MAX` saturates; wait/deferred/failure state persists |
| Queue pruning, FIFO, duplicate replacement, exact capacity | `path_queue_prunes_invalid_work_preserves_fifo_and_replaces_at_capacity` | Invalid and stale work is removed; valid order remains; replacement at 32 preserves the ceiling |
| Non-adjacent relationship canonicalization | `normalization_dedups_nonadjacent_relationship_ids_and_keeps_best` | Duplicate peer IDs collapse deterministically to the strongest/newest record |
| LOD permutation, exact caps, and inclusive boundaries | `lod_plan_is_permutation_invariant_exactly_capped_and_boundary_safe` | Forward/reverse input agrees; 64 logical, 24 visual, 8 detailed; 220/320 m boundaries are pinned |
| Visual-delta permutation, transition, and per-update work | `visual_delta_is_permutation_invariant_and_mode_transition_budgeted` | Reversed ECS order agrees; at most four removals and two builds; old mode is removed before replacement build |
| Deterministic cache eviction and empty-route handling | `path_cache_eviction_is_deterministic_and_empty_routes_do_not_evict` | A full cache evicts its lowest resident ID only for a new non-empty path; an empty result evicts nobody else; replacement preserves the ceiling |
| Internal budget relationships and material ceiling | `visual_and_simulation_budget_constants_are_internally_bounded` | Seed/build/remove/catch-up/path constants and the 25-material formula remain pinned |

The bot-save module separately registers
`legacy_v1_bot_world_loads_with_v3_defaults` and
`legacy_v2_bot_world_defaults_civic_population_and_migrates_to_v3` for the
serde-default and V3 migration claims.

Remaining direct-test gaps are the complete Bevy round-robin
reconciliation/dirty-propagation system and a full civic save/unload/reload
through the bot journal. Natural/Astral runtime and visual acceptance also
remain evidence obligations rather than unit-test claims.

The complete non-visual release gate remains:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features
cargo test --workspace --quiet
cargo check --target wasm32-unknown-unknown --bin voxel-native
scripts/elite-release-gates.ps1
```

## Natural/Astral runtime acceptance

Use a fresh isolated QA world and run directory for every route. Record the
source fingerprint, executable hash, seed, scenery, terrain grammar, profile,
time, weather, viewport, DPI, camera, and route. Never reuse or clean a player's
world for evidence.

Run the same numeric seed with Lush scenery and terrain grammar V3 in both
Natural and Astral Frontier. The minimum civic route is:

1. At 06:30, verify a stable 12-resident seed, unique names/IDs, resolved feet,
   no duplicate roots, and prepare/youth behavior.
2. At 10:00, verify adult stewardship selection, bounded path work, a repeatable
   loaded-voxel route, and work gestures.
3. At 12:30 and 18:30, verify commons movement, bounded pair formation, social
   nods, and no relationship/memory cap violation.
4. With rain or snow at exactly 0.70 and above, verify shelter preemption without
   teleportation. Capture clear and precipitation cases from the same camera.
5. Place a reversible obstruction on a loaded route and verify detour or a
   fail-closed blocked event; remove it and verify recovery only after the
   declared capped retry deadline.
6. At 20:15, verify return/rest selection and the rest pose.
7. Observe from close range, across the 220 m visual boundary, across the 320 m
   logical boundary, and back again. Reject duplicate roots, oscillating LOD,
   invalid placements, or off-range state advancement.
8. Save, unload, and reload. Compare IDs, names, generation identity, logical
   cells, activities, memories, relationships, and counts. Reject duplication,
   reseeding, or loss of authority.
9. In a disposable integration fixture, load the saved civic record under a
   different generation identity. Require `IdentityBlocked`, no logical/visual
   selection or queued/cached route work, and no population overwrite. Then
   restore the exact saved identity and require `Active` authority with the
   same resident records and no reseed.
10. Capture the same stationary camera twice at separated times to prove gait,
    breathing, work, and social animation without flicker. Inspect logs, shader
    errors, frame-time distribution, queue pressure, stalls, and save errors;
    screenshots alone are insufficient.

Profile-specific rejection rules:

- **Natural:** the resident culture must follow the sampled biome/environment;
  residents must read as part of the terrain without copying Minecraft's
  silhouette or disappearing into foliage and water contrast.
- **Astral Frontier:** residents must select Astral culture and remain legible
  against emissive terrain without blown-out eyes, metallic noise, or palette-
  only claims that the world itself changed.

## Viewport and DPI evidence matrix

Each Natural and Astral cell below requires 100%, 150%, and 200% OS/text-scale
coverage with the same reviewed executable. The civic debug line, weather line,
warnings, and long generated names must remain legible without covering core
controls. For resident meshes, inspect close/play-field composition and LOD
stability; for HUD, inspect clipping, overlap, input reachability, and contrast.

| Logical viewport | Natural 100/150/200% | Astral 100/150/200% | Reject when |
| --- | --- | --- | --- |
| 320 x 480 | Required | Required | unsafe reflow, negative width, unreachable controls, civic text covering the reticle |
| 800 x 600 | Required | Required | debug/tool/status collision or resident framing blocked by overlays |
| 960 x 540 | Required | Required | minimum-16:9 overlap, clipped civic telemetry, or illegible proxy silhouette |
| 1280 x 720 | Required | Required | primary gameplay view obstructed or LOD transition hidden by UI |
| 1920 x 1080 | Primary acceptance | Primary acceptance | weak hierarchy, invalid resident placement, duplicate roots, material/animation defect |
| 2560 x 1440 | Required | Required | stretched panels, tiny civic readout, unstable distant proxies |
| 3440 x 1440 | Required | Required | detached edge UI, excessive cursor travel, composition or LOD imbalance |

Also run one adverse portrait/narrow diagnostic. A safe compact layout or an
explicit supported-limit notice is acceptable; silent overlap is not. Every
matrix record must include screenshot paths, matching report/source/executable
identity, average and p95/p99 frame time where available, stall count, maximum
path expansions, path-budget failures, shader/log error state, and any untested
cell. Fresh evidence must compare with a civic-disabled route from the same
binary and world identity.

## Rollback and rejection boundary

On native builds, set the following before launch to disable Civic Ecology
without deleting its saved records:

```powershell
$env:VOXEL_NATIVE_CIVIC_ECOLOGY='0'
```

The trimmed, case-insensitive values `0`, `false`, `off`, and `disabled` all
disable it; an unset variable enables it. Disabled runtime clears its identity
authority, accumulator, pending movement, logical/visual selections, queued
requests, and cached paths, stops cognition/navigation, and drains stale visual
roots through the bounded removal system. The saved population remains
available for a later compatible launch. The environment switch is read at
process start.

WASM currently forces Civic Ecology enabled, so the native environment variable
is not a browser rollback. A browser rollback requires a code/build decision.

Reject or disable V1 if any of the following occurs:

- an identity mismatch mutates or reseeds the population;
- a resident traverses unresolved or newly solid voxels;
- resident, memory, relationship, notice, queue, path, visual, mesh, or material
  ceilings are exceeded;
- social work grows quadratically with population;
- civic code issues a voxel edit or commerce state enters the schema;
- Natural/Astral evidence shows unstable placement, copied visual identity,
  shader/log errors, persistent LOD churn, or an unacceptable p95/p99 frame-time
  regression against the disabled baseline;
- a fresh save/reload loses IDs, duplicates residents, or changes world identity.

## Sources and research boundary

The following German Minecraft Wiki pages were consulted only for abstract
reference categories such as daily rhythm, community anchors, biome coupling,
plant distribution, water context, lighting, and weather response:

- [Dorfbewohner](https://de.minecraft.wiki/w/Dorfbewohner)
- [Dorf](https://de.minecraft.wiki/w/Dorf)
- [Biom](https://de.minecraft.wiki/w/Biom)
- [Biom/Gemäßigte Biome](https://de.minecraft.wiki/w/Biom/Gem%C3%A4%C3%9Figte_Biome)
- [Biom vor Beta 1.8](https://de.minecraft.wiki/w/Biom/Vor_Beta_1.8)
- [Gewächs](https://de.minecraft.wiki/w/Gew%C3%A4chs)
- [Baum](https://de.minecraft.wiki/w/Baum)
- [Texturdaten](https://de.minecraft.wiki/w/Texturdaten)
- [Wasser](https://de.minecraft.wiki/w/Wasser)
- [Fluss](https://de.minecraft.wiki/w/Fluss)
- [Licht](https://de.minecraft.wiki/w/Licht)
- [Nebel](https://de.minecraft.wiki/w/Nebel)
- [Regen](https://de.minecraft.wiki/w/Regen)
- [Gewitter](https://de.minecraft.wiki/w/Gewitter)
- [Tag-Nacht-Rhythmus](https://de.minecraft.wiki/w/Tag-Nacht-Rhythmus)
- [Vollversion 1.18](https://de.minecraft.wiki/w/Versionen/Vollversion_1.18)

The search-index snapshots available during this review were approximately 1.2
years old; the live pages, game behavior, and licensing notices may have changed.
The wiki is an unofficial reference, not a normative technical specification.
Its text and images remain under the licenses stated by that site, while
Minecraft code and game assets remain proprietary to their owners. Voxel-Native
uses original names, formulas, code, palettes, geometry, and values and does not
copy wiki prose, screenshots, structure files, textures, sounds, or game data.
