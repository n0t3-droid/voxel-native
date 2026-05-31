# City Planner Math

Voxel-Native's autonomous city builder is designed around one rule: roads are
the contract, buildings are clients of that contract. Bots should create a city
that reads as intentional from the air, stays editable in-game, and still runs
on modest PCs.

## Road-First Invariant

A project may only build when it has a reachable road story:

- a frontage road or planned route near the lot;
- a reserved footprint that does not overlap road corridors;
- a target deck height when the road is raised;
- enough player and ship clearance to avoid building around the active player.

The planner treats roads as city structure, not decoration. This prevents the
old failure mode where bots placed buildings first and later carved roads
through them.

## Site Score

Bots select from bounded candidate sites instead of scanning the whole world.
Each candidate uses normalized terms in the `0..1` range where possible:

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

The terms are intentionally simple:

- `flatness` rewards ground that needs fewer voxel edits.
- `road_access` rewards lots that can connect to the existing road graph.
- `district_balance` prevents one repeated project type from taking over.
- `route_fit` rewards routes with manageable slopes.
- `block_fit` favors lots that sit cleanly inside the road grid.
- `road_anchor_alignment` rewards road projects whose generated centerline or
  road-grid segment follows an authored district street.
- `semantic_anchor` rewards meaningful frontage, plaza, tower, civic, and
  service relationships.
- `center_distance` gently prevents every project from collapsing into one
  crowded point.

Road anchors are planning intent, not completed geometry. They are part of the
road graph for frontage, collision avoidance, and lot reasoning, but the
duplicate-road test only compares against actual user roads and completed bot
road projects. This lets bots build the authored street once instead of
rejecting it as already present.

## Road Grade Fit

Route candidates are scored by grade before bots commit edit work:

```text
route_fit =
    1
  - 0.55 * avg_step / 5
  - 0.30 * max_step / 9
  - 0.15 * max(height_range - 18, 0) / 34
```

This rejects routes that would become harsh staircases and keeps bridges from
jumping too aggressively. The constants are cheap to evaluate and easy to tune
without changing the planner architecture.

## Smooth Decks

Raised road components interpolate deck height with smoothstep:

```text
smoothstep(t) = t * t * (3 - 2 * t)
deck_y(t) = lerp(start_y, end_y, smoothstep(t))
```

This gives roads, bridge decks, and building pads a soft grade transition while
still producing integer voxel edits at the final placement step.

## Collision Rules

The planner protects the city layout with bounded geometric tests:

- project footprints are axis-aligned boxes in world block coordinates;
- road corridors reject overlap with reserved project footprints;
- duplicate roads reject nearly parallel corridors that run through the same
  space;
- road-frontage probes bind each lot edge to the nearest valid road segment;
- raised-road bindings use `max(terrain_y, road_deck_y)` for the project deck.

These are deliberately not full physics or navigation solves. They are cheap
geometry rules that stop visually obvious mistakes before bots spend voxel edit
budget.

## Diversity Rule

District project selection prefers unused project kinds before repeating. A city
can still grow many towers or houses over time, but the early skyline should
mix civic pads, service pads, plazas, landmarks, residential forms, and road
details before repeating one template.

## Low-End Budget

Every planning step must stay bounded:

- fixed candidate counts;
- local road and lot probes;
- no full-world voxel scans;
- no per-frame city rebuilds;
- edit queues that yield to chunk streaming when the horizon is behind;
- regression tests for roads, lots, frontage rows, deck grades, and city
  pressure.

The result is an engine workflow that can look more intelligent without becoming
too expensive for low-end machines.
