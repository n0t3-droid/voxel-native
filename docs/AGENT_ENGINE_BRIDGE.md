# Direct Agent-Engine Bridge

Status: implementation contract, August 2026. This replaces OCR and periodic
filesystem polling as the normal agent work path while preserving both as
diagnostic fallbacks.

## Outcome

An agent must be able to observe, reason about and change the same authoritative
world that a user sees without pretending pixels are the world state. Visual
captures remain mandatory for perceptual QA, but navigation, editor commands,
streaming state, semantic selection, simulation state and command completion
must travel over a typed direct bridge.

The bridge is not permission to bypass the engine's invariants. It enters the
same command, history, ownership and save paths used by human tools. Every
mutation is sequenced, acknowledged, bounded, attributable and reversible when
the corresponding human operation is reversible.

## Current measured baseline

The existing Agent Control loop is a useful compatibility plane:

- `agent_control.ron` is polled every 50 ms;
- `status.ron` is serialized and written every 250 ms;
- mission PNG previews normally update every 1.5 seconds;
- an OCR-oriented overlay mirrors selected status fields for pixel readers;
- Live Link already provides a local authority hand-off for Spectate and Join.

This design is inspectable and robust across process restarts, but repeated
parse/allocation/filesystem work is the wrong hot path for frame-level control.
It also tempts an agent to infer semantic state from a screenshot that the
engine already knows exactly.

## Candidate approaches

### 1. Faster file polling

Reducing the interval to one frame is simple and keeps the current schema. It is
rejected as the primary path because it multiplies file opens, parsing,
allocation and partially-written-file races. Files remain the durable fallback
and human-readable recovery surface.

### 2. Local HTTP/WebSocket or RPC server

This offers excellent tooling and language interoperability. It is suitable
for remote dashboards later, but framing, string schemas, server lifecycle and
network exposure are unnecessary for the local per-frame hot path. It also
needs a larger security surface before the engine is ready for remote control.

### 3. Loopback UDP action packets

Fixed binary datagrams are low latency, naturally message-oriented and already
fit the engine's Live Link model. Kernel/user copies still occur, so calling
this literally zero-copy would be inaccurate. UDP is selected for low-rate
events, discovery and compatibility, not as the only telemetry store.

### 4. Fixed shared-memory command ring plus seqlock telemetry (chosen)

The engine and local agent map one versioned, fixed-capacity session segment.
Commands use a single-producer/single-consumer ring. The latest telemetry uses
a seqlock snapshot: the writer publishes an odd generation while updating and
an even generation when complete; a reader accepts only matching even
generations. The hot path performs no parsing, no per-message allocation and no
filesystem open after session setup.

This requires an exact ABI, atomics, corruption containment and a fallback when
mapping is unavailable. Those invariants are testable, and the old RON bridge
remains reachable until the measured direct path proves better.

## Two-plane architecture

```text
agent planner / future MCP adapter
  |
  | typed ActionEnvelope { session, sequence, deadline, authority, payload }
  v
bounded SPSC command ring  ----->  Bevy bridge ingress (drain budget)
                                        |
                                        v
                              authoritative engine commands
                              editor history / bots / flight
                                        |
                                        v
seqlock telemetry snapshot  <-----  ECS observation exporter
  |
  +---- bounded event ring: ack, completion, warning, error, authority change
  +---- low-rate PNG: visual/perceptual QA only
  +---- RON fallback: recovery, inspection and compatibility
```

The engine is the only authority for world truth. Mission Control is an
observer/launcher, never a second simulation.

## Fixed contracts

Every mapped region starts with a little-endian header containing magic,
schema version, total byte size, session nonce, producer/consumer generations
and corruption counters. Unknown versions fail closed and fall back to RON.
No process trusts a path, length or offset supplied inside a mapped payload.

Initial hard bounds:

| Resource | Bound |
| --- | ---: |
| command slots | 256 |
| event slots | 512 |
| command payload | 512 bytes |
| telemetry snapshot | 64 KiB |
| commands drained per frame | 32 |
| semantic query results per frame | 64 |
| screenshots in flight | 1 |
| unacknowledged mutation window | 64 sequences |

Queue saturation never allocates more memory. Continuous camera input may
coalesce to the newest value. Mutations, editor operations and authority events
may not silently coalesce; they return `busy`, `expired` or `rejected` with the
last accepted sequence.

## Action vocabulary

Low-level human-equivalent controls remain available for UI and flight QA:

- movement axes, sprint/fly, look delta or absolute yaw/pitch;
- key and mouse edges, fire/scope, screenshot request;
- enter/leave game and explicit authority hand-off.

Normal agent construction uses semantic operations:

- ray/query hit with entity, object, voxel, face, material and ownership data;
- select object, enter/exit Edit Object and select part;
- draw, push/pull, move, rotate, scale, shrink and delete through tool history;
- inspect bounds, connectivity, collision, visibility, lock and save state;
- create/preview/approve/execute/pause/resume/cancel bot work commands;
- request streaming promotion for an edit target without expanding the global
  full-chunk frontier;
- wait for an explicit completion condition rather than sleeping or reading a
  screenshot repeatedly.

No bridge command writes directly into voxel arrays, Bevy transforms or save
files. Those are implementation details behind authoritative command handlers.

## Observation vocabulary

The telemetry snapshot publishes only bounded data and stable identifiers:

- world/profile/seed/time and authority owner/lease;
- camera, player and shuttle pose/velocity/mode;
- active editor tool, phase, selection/object/part IDs and undo/redo heads;
- near chunks, pending jobs, midfield bricks, far rings, resident bytes,
  epochs, stale drops and per-tier p50/p95/p99 work time;
- surface/biome/ecology/river/weather sample under the subject;
- bot command state, cost estimate, progress, warnings and errors;
- frame/GPU timing, stalls, shader status and screenshot completion;
- monotonic observation generation and the command sequence it reflects.

Large lists are queried through paged semantic requests with stable cursors;
they never turn the fixed snapshot into an unbounded serialized world dump.

## Authority, safety and failure containment

- Only loopback/local-session peers with the launcher-provided nonce map the
  control plane.
- CODEX, USER and PAUSED are explicit lease owners. Join transfers authority;
  timeout, disconnect or malformed traffic returns it safely.
- A stale session/world epoch cannot mutate the newly loaded world.
- Duplicate mutation sequences are acknowledged idempotently, not applied
  twice.
- Deadlines prevent a delayed flight or editor command from executing after the
  agent has changed plans.
- A panic/corrupt header disables only the direct bridge and preserves the RON
  fallback; it cannot erase a save or reset a world.
- Commands are recorded with source, sequence, handler result and history step
  so visual findings can be traced to the change that caused them.

## Elite agent work loop

The bridge supports an evidence loop rather than uncontrolled activity:

1. observe exact state and current performance budgets;
2. form a bounded hypothesis and acceptance condition;
3. preview or query without mutation;
4. submit one sequenced command or atomic command batch;
5. wait for its acknowledgement and authoritative completion generation;
6. inspect semantic effects, history/save integrity and performance;
7. capture pixels only when appearance or human usability is the question;
8. accept, undo, revise or record a concrete unresolved risk.

Subagents publish this loop to Mission Control so the user can see which world,
hypothesis, command, evidence and result each mini-screen represents. A feed
that merely says "working" is insufficient.

## Acceptance tests

- 100,000 continuous input updates do not allocate or grow either queue;
- sequence wrap, duplicates, out-of-order and expired commands fail safely;
- corrupt magic/version/size/offset values cannot escape the mapped region;
- telemetry readers never accept a torn generation under concurrent writes;
- command-to-ECS ingress p99 stays below 0.25 ms on the reference machine and
  is visible by the next engine frame under normal load;
- telemetry publication p99 stays below 0.20 ms without filesystem access;
- semantic edit results and undo/redo match the human tool path byte-for-byte;
- stale world/session epochs cannot change the new world;
- queue pressure preserves mutations and authority events while coalescing only
  explicitly coalescible continuous controls;
- disabling the direct bridge restores the current RON/OCR workflow;
- Spectate cannot issue mutations; Join can, and disconnect restores authority.

These are initial targets to measure, not benchmark claims. Implementation must
record actual debug and release results before the direct plane becomes the
default.
