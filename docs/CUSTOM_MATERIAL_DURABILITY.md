# Custom Material Identity and Save Durability

Status: authoritative design and acceptance contract, August 2026. This
document distinguishes the code that exists today from proposed persistence
work. It is not a claim that custom PNG materials already survive every
restart, missing-file event, legacy save, editor transform, or failed write.

## Purpose and safety boundary

Voxel chunks persist a compact `u16` material value. A renderer can only turn
that number back into the intended surface if the meaning of the number is
stable. This contract defines how Voxel-Native may make that meaning durable
without silently changing old worlds, rewriting user chunks as a migration,
or reassigning an absent material to a different PNG.

The implementation work described here must not delete, rename, move, or
rewrite existing user saves as part of discovery or migration. An old world
must remain loadable as geometry even when one or more custom materials cannot
be resolved. In that state the engine keeps the raw saved IDs, renders an
obvious unresolved-material placeholder, reports the condition, and refuses
operations that would falsely certify or overwrite an ambiguous mapping.

Terms used below have deliberately narrow meanings:

- A **saved ID** is the raw `MaterialId` stored next to a voxel.
- A **declared slot** is an explicit custom material number carried by a
  filename such as `material-32768__display-name.png`.
- **Slot identity** means that the declared number is the semantic asset
  identity. Display-name changes and PNG content updates are revisions of the
  same slot.
- **File provenance** means proof that a particular file or persistent GUID is
  the source. The current declared-slot scheme does not provide that stronger
  guarantee.
- A **binding** is durable evidence that a saved ID was intentionally assigned
  to a declared slot under a known versioned scheme.
- **Unresolved** means that the raw saved ID is preserved but no normal custom
  material is allowed to render for it.

## Current implementation: what exists now

The following is a description of the current source tree, not a future-state
diagram.

### Material representation

`src/blocks.rs` defines:

```rust
pub type MaterialId = u16;
pub const DEFAULT_MATERIAL: MaterialId = 0;
pub const CUSTOM_MATERIAL_BASE: MaterialId = 1024;
```

Built-in materials use the numeric value of their `BlockType`; the current
enumeration occupies `0..=44`. `material_is_custom` classifies every value at
or above 1024 as custom. Values `45..=1023` have no assigned built-in meaning.

`src/chunk.rs` stores a full `MaterialId` array beside every voxel array. A
non-air voxel edit through `Chunk::set` changes only the voxel and leaves the
existing material in place. Setting the cell to air resets its material to
`DEFAULT_MATERIAL`.

`src/world.rs` exposes three relevant edit paths:

- `edit_set_voxel_batched` changes only the voxel;
- `edit_set_cell_batched` changes `(Voxel, MaterialId)` together;
- `edit_set_material_batched` changes the material of an existing non-air
  voxel.

The cell and material functions accept a raw `u16`. They do not currently
prove that a custom value has a durable binding.

### Edited-world save format

`EditedChunkOverride` currently contains:

```rust
pub voxels: Vec<Voxel>,
#[serde(default)]
pub materials: Vec<MaterialId>,
```

`EditedChunkFile` contains only a chunk position and that override. It has no
format version, source catalog, identity-scheme marker, binding digest, or
remap table. `save_edited_overrides_snapshot` serializes the raw material
array. `load_edited_overrides_for_world` accepts a correctly sized array and
copies it back without source validation or remapping.

This is backward-compatible for worlds created before material arrays were
added: a missing or incorrectly sized material vector becomes an all-default
array. It is not sufficient to reconstruct the historical meaning of a custom
ID.

Native edited chunks live below:

```text
saves/<world-storage-stem>_edits/chunks/<cx>_<cy>_<cz>.ron
```

The save routine writes expected chunks and then removes `.ron` files in the
`chunks` directory that are no longer in the authoritative override set.
Future catalog data must therefore be placed in a sibling directory, never as
`chunks/catalog.ron`.

On WebAssembly, edited chunk persistence is not implemented: the save path
returns a manifest without storing the overrides and the load path returns an
empty map. Custom edit durability must remain disabled on that target until a
real browser edit-store contract exists.

### Startup and active-world ordering

`WorldPlugin` inserts a default `MaterialLibrary`. `init_world` builds the
library in `Startup`, before the main menu inserts an `ActiveWorld`.
`reinit_world_for_active` later clears the previous world and loads edited
overrides, but it does not currently load a per-world material scheme or
install a per-world remap.

This ordering naturally supports a project-wide source inventory, but a world
binding or world-meta scheme must be applied on world entry before the first
custom-material mesh is accepted as resolved.

### Explicit declared-slot prototype

The current custom loader work introduces an explicit convention:

```text
material-32768__display-name.png
...
material-65535__display-name.png
```

It also introduces these important properties:

- declared IDs must be at least 32768;
- duplicate declared IDs reject the candidate transaction;
- active PNGs are decoded under fixed source, dimension, image-payload, and
  directory-entry budgets;
- material and image `AssetId`s are replaced in place during reload;
- an identity removed during the same process is retained as a one-texel
  tombstone;
- reordering filenames no longer shifts explicit declared IDs.

This is a substantial safety improvement over list-order allocation and should
be retained. Its current source key is effectively `material-id:<u16>`. That
defines a semantic slot: changing the display suffix or replacing the PNG
contents while retaining the declared ID is an intentional revision of the
same identity.

It does **not** persist an inventory or tombstone registry across process
restarts. It also cannot distinguish deliberate slot reuse from mistaken slot
reuse; both are explicit declarations of the same semantic slot. It must not
be described as cryptographic file provenance.

### Legacy allocator range is not provably bounded

The historical loader enumerated every sorted PNG and used:

```rust
CUSTOM_MATERIAL_BASE.saturating_add(i as MaterialId)
```

It did not enforce the new 4096-identity limit. Consequently, although normal
legacy libraries occupied values near `1024`, an old catalog-less save can in
principle contain values at or above 32768 and can eventually alias at 65535.

Therefore a raw high ID alone does not prove that a saved cell was authored
under the new explicit-slot convention. Any absolute safety design must either
prove that the world was created under the new scheme or require a per-ID
binding. Existing catalog-less worlds must never be upgraded automatically
merely because an ID happens to be high.

### Current renderer fallback is fail-visible but not world-bound

The current working tree now gives `MaterialLibrary` one stable unresolved
custom handle backed by a four-byte magenta image. `handle_for` returns that
handle, rather than Stone, for a missing raw ID at or above
`CUSTOM_MATERIAL_BASE`. Its `AssetId` is replaced in place during reloads, so
existing chunk handles do not drift. This closes the silent-Stone rendering
failure, but it still cannot distinguish a bound missing source from an
unbound historical value until active-world bindings exist.

The complete behavior contract is:

- unknown built-in/reserved values may use the engine's normal invalid-data
  fallback;
- a bound and present custom value uses its source handle;
- a bound but absent custom source uses the implemented unresolved handle;
- an unbound custom value also uses that implemented unresolved handle;
- the raw chunk ID is never replaced by the placeholder's ID.

The unresolved surface should be unmistakable, for example a magenta/black
checker with a non-emissive, opaque material. A one-pixel magenta tombstone is
acceptable as an initial safety implementation, but status and telemetry must
still distinguish active, missing, and unbound states.

### Material identity is currently lost by editor history and transforms

The disk format is not the only gap. `BuilderHistory::VoxelChange` stores only
`before: Voxel` and `after: Voxel`. Undo and redo apply those values through
`edit_set_voxel_batched`. The sculpt transform pipeline likewise builds maps
of `IVec3 -> Voxel`, and applies moves, copies, rotations, and scale operations
without the material value.

`VoxelBlob` has a `materials` vector, but the current transform paths do not
populate or consume it.

A simple failure sequence is:

1. remove a custom-material block;
2. setting air clears its material to default;
3. undo restores only the block type;
4. the custom material is lost before any save occurs.

Move, copy, rotate, and scale can similarly drop a source material or inherit
an unrelated destination material. Public custom-material painting must not be
called durable until editor history and object transforms preserve an exact
cell state:

```rust
struct VoxelCellState {
    voxel: Voxel,
    material: MaterialId,
}
```

### Save-success reporting is currently too optimistic

`save_edited_overrides_snapshot` reports a manifest after attempting writes
but does not return a structured error for every failure. The background bot
autosave clears `world.edit_save_dirty` after a writer thread is successfully
queued, rather than after that thread confirms a successful snapshot.

This does not itself reassign a material ID, but it prevents a truthful
durability claim. A future custom-material gate must propagate preflight and
write results and keep the dirty flag set after failure.

## Current bounded budgets

The custom loader prototype defines fixed ceilings. These are part of the
current implementation and should remain pinned by tests:

| Budget | Current ceiling |
|---|---:|
| remembered custom identities per process | 4,096 |
| direct material-directory entries scanned | 8,192 |
| compressed bytes per PNG | 64 MiB |
| maximum PNG edge | 4,096 pixels |
| active custom `Image::data` payload | 256 MiB |
| one inactive tombstone payload | 4 bytes |
| all tombstone image payload | 16,384 bytes |
| successful resident custom image payload | 268,451,840 bytes |
| old resident plus fully staged candidate image payload | 536,887,296 bytes |
| PNG decoder allocation budget | 268,435,456 bytes |
| declared CPU byte envelope for a reload | 872,431,616 bytes |

The last three numbers describe specific counted buffers, not total process
RSS, GPU allocation, allocator bookkeeping, driver staging, or renderer-owned
copies. Documentation and telemetry must not relabel them as total memory.

Any persisted binding implementation additionally needs fixed limits. The
proposed initial values are:

| Binding budget | Proposed ceiling |
|---|---:|
| bindings per world | 4,096 |
| binding-directory entries scanned | 8,192 |
| serialized bytes per immutable record | 4 KiB |
| retained unresolved IDs in diagnostics | 4,096 plus one overflow count |

## Candidate persistence designs

### A. Minimal world-meta scheme marker

The smallest integration alternative adds a serde-defaulted marker to
`WorldMeta`:

```rust
#[serde(default)]
pub custom_material_binding_version: Option<u16>,
```

Semantics:

- legacy worlds deserialize with `None`;
- newly created native worlds use `Some(1)` before any custom edit can occur;
- version 1 permits only explicit declared-slot IDs `32768..=65535`;
- every custom ID in a `None` world is unresolved;
- low custom IDs remain unresolved even in a version-1 world;
- a missing version-1 source renders the unresolved handle;
- display-label and PNG-content replacement under the same declared number are
  intentional revisions of that slot;
- an old world is never automatically changed from `None` to `Some(1)`.

#### Safety properties

This marker safely distinguishes worlds created by the new engine from normal
legacy worlds, including the theoretical legacy high-ID case, as long as the
engine itself created and retained the world metadata. It requires no new
sidecar writes per material and has no mutable ID table.

It is not an individual binding proof. A version-1 marker blesses the scheme
for every high ID in that world's edited chunks. Manually combining a new
world-meta file with arbitrary old `_edits/chunks` can therefore make a legacy
high ID appear to be a declared slot. The action is external/manual rather
than an automatic engine reassignment, but the per-ID ledger is stronger.

#### Imported and copied worlds

- Copying the complete world metadata, edit directory, and required textures
  preserves the marker and slot semantics.
- Copying the world metadata and edits without a PNG leaves the raw IDs safely
  unresolved.
- Copying only the edit directory loses the marker; the imported data must be
  treated as legacy/unresolved.
- Copying legacy edits into a different new world with `Some(1)` is not
  self-authenticating. Import tooling must not silently do this.

#### Low-ID legacy adoption

Version 1 cannot express `saved 1024 -> declared slot 32768`. A legacy world
must remain unresolved or be migrated under a later explicit process. Setting
the whole world to version 1 is not a valid low-ID migration.

#### Integration footprint

This design requires relatively few changes:

1. add the serde-defaulted field in `src/settings.rs`;
2. initialize it to `Some(1)` only in `WorldMeta::new_with_profile` for newly
   created worlds;
3. preserve `None` in every load/save/checkpoint path;
4. pass the active marker to world-entry material resolution;
5. add the dedicated unresolved custom handle;
6. reject custom assignment when the active world's scheme does not authorize
   that ID;
7. preflight edited-world saves and keep dirty state on rejection.

It does not require writing an extra file before each custom edit. It is the
preferred small first persistence gate if declared numeric slot identity is
accepted as the product contract and legacy adoption is deliberately deferred.

### B. Immutable per-world binding ledger

The stronger proposal stores one create-once record per saved custom ID:

```text
saves/<world-storage-stem>_edits/
  chunks/
    <cx>_<cy>_<cz>.ron
  material_bindings/
    v1/
      01024.ron
      32768.ron
```

Suggested authoritative payload:

```rust
enum MaterialIdentityScheme {
    DeclaredSlotV1,
}

struct WorldMaterialBindingV1 {
    version: u16,
    saved_id: MaterialId,
    declared_source_id: MaterialId,
    identity_scheme: MaterialIdentityScheme,
}
```

New material usage normally records `32768 -> 32768`. An explicit legacy
adoption can record `1024 -> 32768` without rewriting the old chunk. Loading
installs an active-world handle alias from the saved ID to the declared source
slot; the raw arrays stay unchanged.

Bindings are monotonic:

- an absent record means unresolved;
- an identical existing record is idempotent;
- an existing saved ID with a different source is a permanent conflict;
- a removed PNG does not remove its record;
- records are never rewritten as part of normal reload;
- no automatic filename-order inference is permitted.

#### Why this is stronger

- each used ID carries its own proof rather than inheriting a world-wide
  classification;
- imported edit folders can carry the binding records alongside their chunks;
- legacy low IDs can be adopted explicitly;
- a theoretically old high ID remains unresolved unless it has a record;
- a corrupt or missing record cannot remap another ID;
- a binding can be published before the corresponding world mutation.

#### Cost and complexity

- every custom assignment path needs a durable precondition;
- native, browser, background-save, import, and world-delete behavior must be
  specified;
- a bounded, race-safe binding loader and publisher are required;
- the editor needs migration UX for unresolved legacy IDs;
- tests must cover concurrent publication and partial files.

This is the recommended final native save authority, but it need not block a
well-labelled world-meta version-1 gate for new worlds.

### C. Project-wide source catalog

A project file such as `textures/materials/catalog-v1.ron` can map persistent
source identities to declared/runtime IDs. It fits the existing startup order
and is useful for UI names, source paths, project-wide tombstones, and new-ID
allocation.

Alone it is insufficient as world-save authority:

- an exported world can outlive or omit the project catalog;
- replacing or losing the project catalog removes validation evidence;
- an old save cannot prove which catalog generation authored it;
- two projects can assign different meanings unless the full catalog travels
  with the world.

The long-term strongest arrangement is:

```text
project catalog = source inventory, paths, names, and new-slot allocation
world ledger    = immutable proof of each raw saved-ID binding
```

If stronger file provenance is required, the project catalog can assign a
persistent 128- or 256-bit source GUID. A content hash may be stored as a
revision/check value, but it should not be the identity itself if artists are
allowed to update a PNG in place.

### D. Deterministic `u16` hash

Hashing a canonical path into `32768..=65535` has only 32,768 outcomes. Using
the birthday approximation, collision probability is approximately:

| Source count | Probability of at least one collision |
|---:|---:|
| 213 | about 50% |
| 256 | about 63% |
| 512 | about 98% |
| 4,096 | effectively 100% |

Collision rejection prevents wrong rendering but produces unpredictable
availability failures. Open addressing, deterministic probing, or salting
cannot preserve existing IDs when the candidate set changes unless the chosen
result is persisted. Once persisted, the catalog rather than the hash is the
authority.

A wider hash is useful as evidence or corruption detection. A truncated
`u16` hash must not be the standalone allocator.

### E. Disable persistence and fail closed

This is the required fallback whenever the active scheme cannot prove a
binding. In that state:

- the raw ID is retained;
- the unresolved handle renders;
- apply/paint is refused;
- automatic remapping is refused;
- edit-save preflight refuses to overwrite an affected snapshot;
- world metadata, bot state, or unrelated settings may still report their own
  independent save result;
- the UI names the blocked IDs and the reason.

For legacy worlds and current WebAssembly edited worlds, this is safer than a
best-effort guess.

## Decision matrix

| Design | New high IDs | Legacy low-ID adoption | Imported-edit safety | Per-edit write | File provenance |
|---|---|---|---|---|---|
| explicit filename only | stable slot if file remains | no | weak | no | no |
| world-meta `Some(1)` | authorized for new worlds | no | medium; meta must travel | no | no |
| immutable world ledger | per-ID proof | yes | strong when ledger travels | first use only | slot proof |
| project catalog only | project-stable | possible | weak without catalog | catalog updates | can support GUID |
| project catalog + ledger | strongest | yes | strongest | first use/catalog updates | yes with GUID |
| disabled/fail-closed | unresolved only | no | safe | no | n/a |

Recommended sequence:

1. retain explicit declared slots;
2. make unresolved custom rendering loud and preserve raw IDs;
3. optionally ship the small world-meta version-1 gate for newly created
   worlds only;
4. keep legacy worlds disabled until explicit adoption exists;
5. implement the immutable per-world ledger before claiming general legacy,
   import, or per-ID durability;
6. add a project source catalog only when source-path/GUID lifecycle and
   multi-world authoring require it.

## Non-overwriting publication contract

The existing `settings::atomic_write_text` is not a sufficient primitive for
immutable binding publication on Windows. It writes a temporary file, attempts
`fs::rename`, and on rename failure directly writes the final path. A direct
fallback write can be interrupted and is not an atomic replacement.

An immutable binding must use a no-overwrite protocol:

1. validate the complete record and serialize it below the 4 KiB cap;
2. create a unique sibling `.partial-*` with `create_new(true)`;
3. perform one bounded `write_all`, then `sync_all`;
4. publish to a final path that must not already exist, preferably through an
   atomic non-overwriting link or rename supported by the platform;
5. if the final path already exists, read and validate it;
6. exact equality is idempotent success;
7. any different binding for the same saved ID is rejection;
8. remove only the publisher's own partial file;
9. never replace or delete an existing binding record.

The binding must be durable before the engine mutates a voxel to use it. A
crash after binding but before the edit leaves a harmless unused record. A
crash after edit but before binding would create another ambiguous saved ID and
is forbidden by ordering.

Concurrent publishers are safe under the same rule: one wins publication;
every loser validates the winner and either accepts exact equality or reports
a conflict. Directory enumeration counts all direct entries before extension
or partial filtering so abandoned files cannot make work unbounded.

Platform documentation must distinguish process-crash atomic publication from
stronger power-loss guarantees. File `sync_all` alone does not prove that every
platform durably flushed parent-directory metadata.

## Migration and fail-closed UX

### Legacy world with no scheme or binding

The engine loads voxel geometry and keeps every raw material ID. It does not
bind any custom ID, including a high value. A bounded diagnostic reports the
unique unresolved IDs and an overflow count. Rendering uses the unresolved
material.

Normal save operations must not silently bless that world by setting the
world-meta marker. An unrelated settings or pose checkpoint must preserve
`None`.

### Legacy unprefixed PNG

An unprefixed file is inventoried as legacy/unregistered, not assigned by
sorted position. A safe migration UI presents:

- the unresolved saved ID;
- the candidate source preview and path;
- the affected cell/chunk count under a bounded scan;
- an explicit adoption action;
- the resulting immutable binding before any remesh is labelled resolved.

No migration action renames or moves the PNG or rewrites all affected chunks.
If migration UI is unavailable, the source remains disabled.

Known legacy/unregistered files should not be treated as active candidates.
They may be skipped with a loud status while correctly declared files load.
Duplicate declared IDs, an invalid declared ID, or corruption within the active
candidate set still rejects that active transaction.

### Missing declared source

A world-meta version or binding proves identity, not availability. If the PNG
is absent, the world keeps its raw IDs and renders unresolved. Reintroducing a
file with the same declared slot restores that slot according to the
DeclaredSlotV1 contract. A different display label or different pixels are a
revision of that same slot, not a new identity.

### Actual file provenance

If the product must detect that a different file has reused a declared number,
DeclaredSlotV1 is not enough. That future requirement needs a persistent GUID
owned by a project catalog and copied into the world binding. It must be a new
versioned identity scheme rather than a silent reinterpretation of version 1.

## Required implementation gates

### Gate 0: current safe statement

- Explicit high filename IDs are an in-progress slot-identity mechanism.
- Same-process tombstones and payload caps are implemented in the current
  custom-loader work.
- Bounded single-handle PNG reads, total directory-entry work, decode/mip
  limits, transaction rollback, and one stable unresolved-magenta asset are
  implemented and covered by focused tests.
- General cross-restart custom source durability is **not** implemented.
- Loud unresolved rendering is implemented. Legacy material recovery, world
  binding, unresolved-ID diagnostics, and material-preserving transforms are
  not complete.

No release note, QA report, or capability manifest may claim more.

### Gate 1: renderer and range fail-closed behavior

Implemented in the current working tree:

- explicit declared-slot filenames reject duplicate or malformed candidates;
- one stable dedicated unresolved custom handle exists;
- unknown custom buckets no longer fall through to Stone or generic grain;
- raw IDs remain unchanged across remesh and material reload.

Still required to complete Gate 1:

- centralize the durable declared-slot range in `blocks.rs`;
- reject reserved IDs at every new-edit authority, not only in the PNG loader;
- report bounded active, missing, unbound, and rejected-ID diagnostics.

### Gate 2: small new-world scheme gate

- Add the serde-defaulted optional version to `WorldMeta`.
- Set version 1 only during creation of a genuinely new world.
- Never auto-upgrade a loaded legacy world.
- Apply the marker on world entry before custom meshes resolve.
- Refuse low-ID and unbound persistence.
- Propagate save failure and retain dirty state.
- Keep WebAssembly custom edit durability disabled.

Passing Gate 2 supports a narrow claim: newly created version-1 native worlds
can persist explicit declared-slot IDs, with content replacement intentionally
defined as a slot revision. It does not support legacy adoption, arbitrary
imports, or file provenance.

### Gate 3: material-preserving editing

- Replace voxel-only history states with exact cell states.
- Preserve materials through remove/undo/redo.
- Preserve materials through object move, array copy, rotation, enlargement,
  and reduction.
- Define a deterministic reduction rule when scale-down combines cells with
  different materials.
- Include material state in rollback and history-cap accounting.

### Gate 4: immutable per-world ledger

- Add bounded version-1 binding records in a sibling edit directory.
- Publish the record before first use.
- Load active-world bindings before remesh.
- Alias saved IDs to declared source handles without rewriting arrays.
- Support explicit low-ID legacy adoption.
- Reject conflicts, corrupt records, excess entries, and partial records.
- Preflight every edited-world snapshot.

### Gate 5: optional project catalog and provenance

- Add a project source inventory only if multi-world authoring needs it.
- Keep world bindings authoritative for saved IDs.
- Introduce GUID provenance as a new scheme version, never as an implicit
  reinterpretation of DeclaredSlotV1.
- Treat content hashes as revision evidence, not automatically as identity.

### Gate 6: release evidence

- Run native format, check, clippy, workspace tests, and release gates.
- Run the registered WASM check and verify its fail-closed capability status.
- Use only disposable QA worlds for live save/restart tests.
- Record exact world scheme, source slots, missing-source route, migration
  route, build profile, and expected unresolved counts.
- Inspect screenshots; a green test with silent Stone fallback is a failure.

## Test matrix

### Declared-slot loader

- boundary IDs 32768 and 65535 load;
- 32767 is rejected as a new declared source;
- duplicate declarations reject the complete active candidate transaction;
- directory reorder does not change IDs;
- display-label rename keeps the same slot;
- content replacement keeps the same slot and asset handles;
- removal becomes a bounded tombstone;
- re-addition restores the existing slot;
- one entry above every input, identity, and payload cap is rejected;
- exact payload accounting matches resident and transient formulas.

### World-meta scheme

- old RON without the field deserializes to `None`;
- new-world construction emits `Some(1)`;
- loading and re-saving a legacy world preserves `None`;
- pose/settings checkpoints preserve the active value exactly;
- version 1 authorizes only high declared slots;
- low IDs and unknown versions remain unresolved;
- a missing source renders unresolved while the raw ID remains unchanged;
- copying edits without metadata cannot activate a high ID;
- importing metadata without PNGs remains unresolved rather than failing
  geometry load.

### Immutable binding records

- `32768 -> 32768` survives reconstruction with a fresh library and process
  state;
- an identical second publication is idempotent;
- a conflicting second publication changes no file or runtime map;
- `1024 -> 32768` resolves a legacy saved ID without chunk rewriting;
- 4096 bindings load and binding 4097 is rejected;
- a 4097-byte record, wrong version, invalid range, duplicate ID, symlink, or
  reparse point fails closed;
- a `.partial-*` file is never accepted as authority;
- two concurrent publishers cannot publish different meanings;
- a corrupt record cannot partially activate the remaining table;
- no test touches a real `saves/` path.

### World save/load

- built-in and bound custom cell states round-trip exactly;
- a legacy missing material vector still becomes default;
- a catalog-less low or high custom ID stays raw but unresolved;
- save preflight rejects an unbound custom ID before changing any file;
- writer failure retains the dirty state;
- a missing source does not normalize the saved ID to the placeholder;
- remesh and world reload do not mutate stored material arrays;
- catalog status in a manifest is updated only after successful persistence.

### Mesher and renderer

- saved alias and declared source use the same normal material handle;
- absent and unbound custom IDs use the unresolved handle, never Stone;
- separate raw material buckets stay separate even if both are unresolved;
- renderer fallback cannot alter save state;
- current and reloaded asset handles remain stable under the declared budgets.

### Editor, selection, and history

- remove plus undo restores exact `(Voxel, MaterialId)`;
- redo restores the matching after-state;
- move, rotate, scale, and linear array copy preserve materials;
- reduction with competing materials is deterministic and insertion-order
  independent;
- a failed semantic/document transform rolls back both voxel and material
  state;
- object editing changes only selected parts;
- history work and retained bytes remain under documented ceilings.

### Migration UX

- unresolved IDs are listed deterministically with a bounded overflow count;
- no migration action occurs without explicit confirmation;
- adoption publishes its binding before the world becomes resolved;
- cancelling adoption makes no filesystem or world change;
- missing, corrupt, duplicate, over-budget, and wrong-version states explain
  why apply/save is disabled;
- viewport sizes from the responsive QA matrix do not hide the warning or
  confirmation controls.

## Acceptance statement

Custom PNG durability may be claimed only for the exact gate that has passed:

- **Today:** explicit declared slots improve reload stability, with bounded
  same-process identity handling. General durability is not yet proven.
- **After Gate 2:** only newly created version-1 native worlds with high
  declared slots qualify, under semantic slot identity.
- **After Gate 4:** individual saved IDs, including explicitly adopted legacy
  aliases, have durable per-world evidence.
- **After Gate 5:** file/GUID provenance may be claimed only for worlds and
  sources written under that new versioned scheme.

At every stage, a missing proof is an unresolved material, not a guessed
mapping and not a silent Stone surface.
