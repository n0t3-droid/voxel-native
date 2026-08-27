# Far-field terminal skirts v1

## Scope and baseline

The far-field clipmap formerly emitted a downward outer-edge skirt on every
one of its six resident terrain rings. A complete deterministic six-ring build
therefore contained 28,086 terrain vertices, 117,960 terrain indices, and
1,819,968 bytes of generated attribute/index payload. Each ring contributed
exactly 960 skirt vertices and 1,440 skirt indices.

L0-L4 do not end at a finite horizon: their outer edge sits inside the fixed
overlap and morph band of the next coarser ring. The five intermediate skirts
could consequently intersect an already present parent top surface and appear
as dark triangular teeth. L5 is different because its outer edge is the actual
finite rendered horizon.

## Selected policy and alternatives

Terminal-skirts v1 emits the unchanged outer skirt only for L5. L0-L4 emit no
skirt vertices or indices. This is a source-only rendering policy: it adds no
runtime option, persisted setting, cache identity, task, entity, or allocation
whose lifetime could diverge from the ring request.

Alternatives considered were making intermediate skirts shallower, changing
their colour, or depth-biasing them. Each alternative retained intersecting
geometry and could only make the artifact less obvious for a particular view.
Removing every skirt was also rejected because it would expose the finite L5
horizon. Terminal-only emission removes the overlapping geometry while
preserving the outer closure.

## Fixed population and proof matrix

With no near-coverage cutout, the exact terrain population is now 23,286
vertices, 110,760 indices, and 1,560,768 payload bytes. The change removes
4,800 vertices, 7,200 indices, and 259,200 bytes. Existing conservative public
ceilings remain unchanged at 35,000 vertices, 150,000 indices, and 2,280,000
bytes; rollback therefore cannot exceed a previously admitted budget.

Unit coverage checks all five adjacent pairs L0-L1 through L4-L5. For every
pair it enumerates all four nested X/Z snap phases at positive, negative, and
mixed-sign world coordinates. Fine-step microcells prove that the union of the
two top surfaces has no XZ hole outside the intentional finer-ring inner hole.
At shared nested-lattice vertices in the handoff band, the underlying morphed
source height is exact across both levels; rendered Y differs only by the
existing constant per-level anti-z-fighting bias. A separate exact population
test requires L0-L4 to contribute 0/0 skirt vertices/indices and L5 to retain
960/1,440.

## Failure mode, visual gate, and rollback

The principal residual risk is a view-dependent crack that the discrete
topology/height proof does not reveal, especially under a camera projection or
GPU depth-precision regime. Acceptance therefore still requires a real-engine
Natural river route with every captured frame inspected; population and FPS
alone are insufficient. The expected failure signature is an exposed seam at
an adjacent LOD transition or a missing outer-horizon closure.

Rollback is one condition at the skirt-emission boundary: restore emission for
all levels. No world, save, cache, or evidence-schema migration is involved.
The focused non-visual gates are:

```powershell
cargo test --bin voxel-native planetary_streaming::tests
cargo check --bin voxel-native
cargo check --target wasm32-unknown-unknown --bin voxel-native
cargo fmt --all -- --check
git diff --check -- src/planetary_streaming.rs docs/FAR_TERMINAL_SKIRTS_V1.md
```
