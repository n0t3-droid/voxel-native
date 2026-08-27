# Agent Direct Bridge Phase 1: Fixed Region Core

Status: **bounded in-process core proven; OS transport and engine integration are
not implemented; `DIRECT_BRIDGE_READY` must remain `false`.**

The core module is compiled by the application so ABI and test drift are caught,
but no live system constructs a region or consumes its commands.

This phase implements the ABI-shaped data core described by
`docs/AGENT_ENGINE_BRIDGE.md` without pretending that a Rust allocation is an
operating-system shared-memory transport. It does not touch saves, editor
history, Mission Control, Agent Control, Bevy command handlers, or authority.
The existing RON/OCR path remains the only integrated path.

## Problem frame and success metric

Inputs are one launcher-provided 128-bit session nonce, one authoritative world
epoch, fixed binary command/event payloads, and bounded telemetry bytes. Outputs
are ordered commands for a single engine consumer, ordered events for a single
agent consumer, and the latest internally consistent telemetry generation.

The hard invariants are:

- fixed region, queue, payload, and per-drain sizes;
- no queue growth and no core allocation after one setup allocation;
- exactly one producer and one consumer per ring;
- stale, duplicate, out-of-order, expired, malformed, or foreign-session data
  never reaches the accepted-command callback;
- a telemetry reader accepts only a matching even generation;
- no save, world, or renderer is mutated by this Phase 1 module.

The latency targets inherited from the bridge contract are command ingress p99
below 0.25 ms and telemetry publication p99 below 0.20 ms in a runtime build.
The benchmark records a distribution rather than a best sample.

## Verified baseline

The current integrated compatibility plane remains:

| Existing path | Verified source constant / contract | Earliest normal visibility |
| --- | ---: | ---: |
| `agent_control.ron` poll | `src/agent_control.rs`, `poll_timer = 0.05` | 50 ms plus frame and parse work |
| `status.ron` write | `src/agent_control.rs`, `status_timer = 0.25` | 250 ms plus serialization and I/O |
| mission preview | current bridge contract default | 1.5 s |

Those are configured cadence floors, not a same-work microbenchmark. The direct
core benchmark measures only memory ingress/publication and intentionally does
not claim that Bevy command completion is already this fast.

## Candidate decision record

### A. Faster RON polling

Lowering the 50 ms poll interval keeps inspection and recovery simple, but it
multiplies file opens, partial-write races, parse allocations, and filesystem
traffic. It remains the durable fallback and is rejected as the frame-level hot
path.

### B. Local RPC or WebSocket

Typed RPC is excellent for tooling and remote dashboards. Framing, server
lifecycle, schema translation, kernel transitions, and a broader attack surface
are unnecessary for a same-machine per-frame control plane. It remains a future
interoperability plane, not the local core.

### C. Mutex-protected MPMC mapped queues

An MPMC queue would make accidental endpoint duplication easier to tolerate and
can simplify process recovery. It also adds contention, poison/recovery policy,
platform-specific process-shared locking, and an ability the contract does not
need: the launcher already assigns one agent producer and one engine consumer.
It is rejected until a real multiple-writer requirement exists.

### D. Cache-line-separated SPSC rings plus atomic-word seqlock (chosen)

Two fixed SPSC rings make ownership explicit and keep each hot cursor on its own
64-byte line. Telemetry is a latest-value store, so an even/odd generation is
more appropriate than queueing every obsolete observation. The unusual but
important choice is to store the 64 KiB payload as atomic `u64` words. Reading a
plain byte array concurrently with a writer would be a Rust data race even if a
seqlock later rejected the copy; atomic words keep the attempted read sound.
This costs per-word atomic operations but wins the measured runtime target with
a large safety margin.

Pseudo-novel alternatives were rejected: no unchecked plain-byte seqlock, no
overwrite of a published ring slot, no unbounded semantic dump, no unsafe mmap
cast before prefix/length/alignment validation, and no claim that `repr(C)` alone
makes cross-process atomics portable.

## Pinned ABI-shaped layout

All mapped-looking structs are `repr(C)` and the hot structures are aligned to
64 bytes. Compile-time assertions pin every size, alignment, and important
offset on the tested `x86_64-pc-windows-msvc` target.

| Region section | Offset | Bytes | Fixed population |
| --- | ---: | ---: | ---: |
| versioned header | 0 | 576 | 1 |
| command slots | 576 | 147,456 | 256 x 576 |
| event slots | 148,032 | 163,840 | 512 x 320 |
| telemetry area | 311,872 | 65,600 | 64 KiB payload + 24-byte metadata + padding |
| **total region** | **0** | **377,472** | **constant** |

The first 64 bytes are independently byte-validated before any future mapped
transport may form a typed view:

| Prefix field | Offset | Encoding |
| --- | ---: | --- |
| magic `VXAGBRG1` | 0 | 8 literal bytes |
| ABI version | 8 | little-endian `u16`, currently 1 |
| header size | 10 | little-endian `u16`, exactly 576 |
| endian tag | 12 | little-endian `u32`, `LE` + version tag |
| total region size | 16 | little-endian `u64`, exactly 377,472 |
| session nonce | 24 | 16 nonzero launcher bytes |
| reserved | 40 | 24 zero bytes |

The header then contains separate cache lines for world epoch, command producer,
command consumer, event producer, event consumer, and telemetry generation,
followed by fixed corruption/pressure counters. The constructor fails on a
non-little-endian target. Dynamic cursors are therefore native atomics on a
little-endian target; a future cross-target ABI requires a new version, not an
implicit byte swap.

### Fixed budgets

| Resource | Hard cap |
| --- | ---: |
| command slots | 256 |
| command payload | 512 bytes |
| event slots | 512 |
| event payload | 256 bytes |
| telemetry payload | 65,536 bytes |
| commands removed per engine drain | 32 |
| recent sequence/replay window | 64 |
| telemetry writers | 1 |
| command producers/consumers | 1 / 1 |
| event producers/consumers | 1 / 1 |

Current and peak command/event occupancy, producer/consumer cursors, telemetry
generation, full-queue observations, retries, and every corruption/rejection
class are queryable without allocation. Diagnostic counters saturate at
`u64::MAX` instead of wrapping to a misleading healthy zero.

## Memory-ordering argument

### Command ring: agent producer to engine consumer

1. The producer reads its own producer cursor with `Relaxed` and the consumer
   cursor with `Acquire`.
2. It writes the selected non-atomic slot through `UnsafeCell`.
3. It publishes the incremented producer cursor with `Release`.
4. The consumer reads producer with `Acquire`; that makes the complete slot
   write visible before it reads the slot.
5. After copying the slot, the consumer publishes its cursor with `Release`.
6. The producer's next `Acquire` of consumer prevents reuse until the read is
   finished.

The event ring is identical with engine and agent roles reversed. Safe endpoint
construction requires a mutable region borrow and returns exactly one endpoint
of each role. Endpoints contain `PhantomData<Cell<()>>`: they can move to their
owner thread (`Send`) but cannot be concurrently shared by reference (`!Sync`).
That type property is part of the safety proof for the `unsafe impl Sync` on the
fixed region.

Counters use wrapping `u64` serials. Occupancy is
`producer.wrapping_sub(consumer)` and any value above capacity fails as cursor
corruption. Power-of-two masks select slots. The tests initialize cursors near
`u64::MAX` and cross zero while preserving FIFO order.

### Telemetry writer to reader

1. The sole writer changes an even generation to odd using `compare_exchange`
   with `AcqRel`; an odd value rejects a second writer.
2. Metadata and payload are stored with relaxed atomics while generation is odd.
3. The writer publishes the next even generation with `Release`.
4. A reader loads generation with `Acquire`, rejecting zero or odd.
5. It reads atomic metadata and only the bounded number of atomic payload words.
6. An `Acquire` fence prevents those reads moving after validation; a second
   generation load with `Acquire` must match the first and be even.

If a writer overlaps the attempted copy, the payload loads are still legal
atomic operations and the changed/odd generation rejects the mixed snapshot.
A crash that leaves an odd generation results in `TelemetryBusy`; it is never
reported as healthy data.

This is an argument and adversarial proof on the current compiler/CPU, not a
formal model-check of every Rust target. Adding Loom just for this isolated phase
would be a substantial dependency and would still not prove Windows mapped
atomics. The module instead combines the narrow protocol, type-level SPSC
ownership, 100,000-update races, forced odd-generation tests, cursor-wrap tests,
and compile-time layout assertions. A future transport phase should add a small
model crate or equivalent CI job before enabling the bridge by default.

## Sequence, deadline, epoch, and coalescing policy

- Epoch mismatch is checked before sequence tracking, so a stale high sequence
  cannot poison the new world's tracker.
- Recent duplicates return an idempotent duplicate disposition and never reach
  the mutation callback.
- Serial-number arithmetic accepts a delta in `(0, 2^63)` as newer. Zero is a
  duplicate; the ambiguous half-space and older values are out of order.
- A syntactically valid current-epoch sequence is recorded before its deadline
  check. An expired message therefore cannot be replayed later under the same
  sequence.
- The 64-entry replay window is bounded. An older replay that has fallen out of
  the window is still rejected as out of order.
- Published ring slots are immutable until consumed. `ContinuousCommandStager`
  may replace only a producer-local pending `Continuous` command with the same
  opcode and epoch and a newer sequence. Mutation and authority commands cannot
  enter the coalescer; a full queue returns `QueueFull` instead of silently
  discarding them.

Phase 1 reports dispositions and counters; future engine integration must emit
the corresponding ack/busy/expired/rejected event and route accepted mutations
through the human tool/history path.

## Test evidence

Standalone compilation isolates the Phase 1 protocol core until transport
proof is available. Reproduce the focused checks with:

```text
rustc --edition 2021 --test src\agent_direct_bridge.rs -o target\agent_direct_bridge_tests_debug.exe
target\agent_direct_bridge_tests_debug.exe --test-threads=1
```

Result on 2026-08-09: **15 passed, 0 failed, 1 ignored benchmark**, in 0.16 s.

Additional repository evidence from the same checkpoint:

- `rustfmt --edition 2021 --check src\agent_direct_bridge.rs`: pass;
- native standalone library compile with `-D warnings`: pass;
- `wasm32-unknown-unknown` standalone library compile with `-D warnings`: pass;
- `cargo check --bin voxel-native`: pass with pre-existing dead-code warnings;
- `cargo check --target wasm32-unknown-unknown --bin voxel-native`: pass with
  pre-existing target-specific warnings.

Coverage includes:

- compile-time and runtime ABI sizes, alignments, offsets, magic, version,
  little-endian fields, total size, and nonzero nonce;
- truncated/corrupt magic, version, extreme total size, wrong/zero nonce;
- command and event empty/full/FIFO/wrap behavior;
- corrupt command length, class, and reserved fields consumed without delivery;
- fixed 32-command drain budget;
- stale epoch, explicit epoch tracker reset, duplicate mutation, out-of-order,
  expiry, and sequence wrap;
- continuous-only producer-local coalescing;
- forced odd telemetry generation;
- 100,000 concurrent telemetry publications while a reader verifies every
  accepted 64-byte snapshot contains one repeated sequence marker;
- saturating diagnostic counters that never wrap to zero;
- 100,000 concurrent SPSC command updates with exact ordering, stable region
  address, stable 377,472-byte storage, and no queue growth.

Both 100,000-update concurrent tests also passed ten consecutive adversarial
runs (one million publications and one million commands per test family in
aggregate) with synchronized starts and forced scheduler yields.

The core hot path contains no `Vec`, `String`, serializer, file API, or allocator
call. `new_boxed` performs the one setup allocation; test/benchmark vectors are
outside the timed core. A callback supplied by integration can of course
allocate, so its cost and allocations must be measured separately rather than
attributed to this core.

## Benchmark distribution

Reference machine and toolchain:

- AMD Ryzen 7 5700G, 8 cores / 16 logical processors;
- Windows 11 Pro 64-bit, build 10.0.26200, approximately 32 GiB RAM;
- `rustc 1.92.0`, LLVM 21.1.3, `x86_64-pc-windows-msvc`;
- standalone test binary, no graphical engine, no frame load;
- 20,000 command and 64-byte telemetry samples; 2,000 full-telemetry samples.

| Profile and operation | p50 | p95 | p99 | Runtime target |
| --- | ---: | ---: | ---: | ---: |
| debug command enqueue + one drain | 0.9 us | 1.2 us | 1.4 us | informational |
| debug telemetry publish + read, 64 B | 1.0 us | 1.1 us | 1.1 us | informational |
| debug telemetry publish, 64 KiB | 395.2 us | 419.9 us | 449.0 us | informational; misses runtime gate |
| optimized command enqueue + one drain | 0.1 us | 0.1 us | 0.1 us | < 250 us: pass |
| optimized telemetry publish + read, 64 B | 0.1 us | 0.1 us | 0.1 us | < 200 us: pass |
| optimized telemetry publish, 64 KiB | 16.5 us | 16.6 us | 29.3 us | < 200 us: pass |

The 100 ns floor reflects Windows timer resolution at this scale; it should be
read as “below useful single-operation resolution,” not as a universal 100 ns
promise. Full-region publication is more informative. These results prove the
isolated algorithm budget, not command-to-ECS completion, authority transfer,
frame visibility, or behavior under renderer pressure.

## Elite-standard handback

| Standard level | Phase 1 evidence | Status |
| --- | --- | --- |
| Level 0, safety | no save/world/UI access; versioned prefix; corrupt input fails closed; old bridge untouched | pass for isolated core |
| Level 1, correctness | integer epochs/sequences, extreme size rejection, stale/corrupt tests, no float identity | pass for isolated core |
| Level 2, boundedness | compile-time region/slot/payload/drain caps; current + peak occupancy; distribution benchmark | pass for isolated core |
| Level 7, agent parity | nonce/epoch/sequence/expiry, fixed command/event/telemetry contracts | partial: no capability manifest or engine parity yet |
| Level 8, adversarial state | 100k concurrent command + telemetry soak, saturation, wrap, malformed bytes, counters | partial: no process crash/device/focus/engine soak yet |
| Level 9, release evidence | scoped fmt, warning-denied native + WASM compile, optimized benchmark, native/WASM cargo checks, full registered suite, path audit | partial: the core is compile-registered but has no OS transport, live consumer, or real-engine route |

## Honest transport and integration gap

`repr(C)` and passing thread stress do **not** prove an OS mapping safe. Phase 2
must resolve all of the following before a readiness flag changes:

1. native-only mapping API, exact create/open ownership, restrictive local ACL,
   unpredictable launcher nonce, mapping length, and 64-byte base alignment;
2. validation of the 64-byte prefix from raw bytes before forming any atomic or
   typed reference;
3. documented support for inter-process 64-bit atomics on every enabled target,
   including process crash/lifetime and stale mapping reclamation;
4. a safe endpoint ownership design across independently mapped processes;
5. engine-side ack/completion events, bounded semantic query paging, explicit
   CODEX/USER/PAUSED lease transitions, and fallback diagnostics;
6. exact routing through existing editor history, undo/redo, bot, flight,
   streaming-promotion, and save invariants;
7. native and supported WebAssembly compilation policy (the direct mapping is
   expected to remain native-only while WASM retains fallback capability);
8. loaded-engine p50/p95/p99, queue peak, stale/corruption, one-frame visibility,
   disconnect recovery, Natural/Astral, and Spectate/Join tests.

Until those gates pass, Mission Control must label this as **fixed-region core,
transport unavailable**, and `DIRECT_BRIDGE_READY` remains false.
