//! Deterministic, constant-budget summaries between full voxels and far clipmaps.
//!
//! This module deliberately has no Bevy or renderer dependency. Near-field
//! generation can reduce full voxels into [`SummaryBrick`] values, while the
//! far-field scheduler can sample those values without taking ownership of
//! authoritative world data. Resident summaries are reconstructible cache
//! entries; sparse edits live in a separate, snapshot-able overlay and are
//! never discarded by cache pressure.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::mem::size_of;

pub type MaterialId = u16;

pub const BRICK_EDGE: usize = 8;
pub const BRICK_CELL_COUNT: usize = BRICK_EDGE * BRICK_EDGE * BRICK_EDGE;
pub const MAX_LOD: u8 = 30;
pub const DEFAULT_MAX_LOD: u8 = 12;
pub const MIDFIELD_RESIDENT_BRICK_LIMIT: usize = 512;
pub const MAX_ACTIVE_GENERATION_TASKS: usize = 128;
pub const BRICK_PAYLOAD_BYTES: usize = BRICK_CELL_COUNT * size_of::<CellSummary>();

/// Compact, fixed-width macro-cell payload.
///
/// `occupancy` and `error` are quantised to `[0, 255]`. Occupancy is the
/// fraction of source volume that is solid; error is a conservative signal
/// used by a screen-space scheduler to request a finer representation.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellSummary {
    pub material: MaterialId,
    pub occupancy: u8,
    pub error: u8,
}

impl CellSummary {
    pub const EMPTY: Self = Self {
        material: 0,
        occupancy: 0,
        error: 0,
    };

    pub const fn new(material: MaterialId, occupancy: u8, error: u8) -> Self {
        Self {
            material,
            occupancy,
            error,
        }
    }

    pub const fn solid(material: MaterialId) -> Self {
        Self::new(material, u8::MAX, 0)
    }

    pub const fn is_empty(self) -> bool {
        self.occupancy == 0 && self.error == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorldVoxel {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

impl WorldVoxel {
    pub const fn new(x: i64, y: i64, z: i64) -> Self {
        Self { x, y, z }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BrickKey {
    lod: u8,
    x: i32,
    y: i32,
    z: i32,
}

impl BrickKey {
    pub fn new(lod: u8, x: i32, y: i32, z: i32) -> Result<Self, HierarchyError> {
        validate_lod(lod)?;
        Ok(Self { lod, x, y, z })
    }

    pub const fn lod(self) -> u8 {
        self.lod
    }

    pub const fn coordinates(self) -> (i32, i32, i32) {
        (self.x, self.y, self.z)
    }

    pub fn world_origin(self) -> Result<WorldVoxel, HierarchyError> {
        let span = brick_span(self.lod)?;
        Ok(WorldVoxel::new(
            i64::from(self.x)
                .checked_mul(span)
                .ok_or(HierarchyError::ArithmeticOverflow)?,
            i64::from(self.y)
                .checked_mul(span)
                .ok_or(HierarchyError::ArithmeticOverflow)?,
            i64::from(self.z)
                .checked_mul(span)
                .ok_or(HierarchyError::ArithmeticOverflow)?,
        ))
    }

    pub fn parent(self) -> Result<Self, HierarchyError> {
        let parent_lod = self
            .lod
            .checked_add(1)
            .ok_or(HierarchyError::ArithmeticOverflow)?;
        Self::new(
            parent_lod,
            self.x.div_euclid(2),
            self.y.div_euclid(2),
            self.z.div_euclid(2),
        )
    }

    pub fn children(self) -> Result<[Self; 8], HierarchyError> {
        if self.lod == 0 {
            return Err(HierarchyError::LeafHasNoChildren);
        }
        let child_lod = self.lod - 1;
        let mut children = [Self::new(child_lod, 0, 0, 0)?; 8];
        for oy in 0..2_i32 {
            for oz in 0..2_i32 {
                for ox in 0..2_i32 {
                    let index = octant_index(ox as usize, oy as usize, oz as usize);
                    children[index] = Self::new(
                        child_lod,
                        self.x
                            .checked_mul(2)
                            .and_then(|value| value.checked_add(ox))
                            .ok_or(HierarchyError::ArithmeticOverflow)?,
                        self.y
                            .checked_mul(2)
                            .and_then(|value| value.checked_add(oy))
                            .ok_or(HierarchyError::ArithmeticOverflow)?,
                        self.z
                            .checked_mul(2)
                            .and_then(|value| value.checked_add(oz))
                            .ok_or(HierarchyError::ArithmeticOverflow)?,
                    )?;
                }
            }
        }
        Ok(children)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrickAddress {
    pub key: BrickKey,
    pub cell_index: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BrickStamp {
    pub epoch: u64,
    pub source_version: u64,
    pub overlay_version: u64,
}

/// A fixed `8^3` summary payload. It owns no authoritative voxel or edit data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryBrick {
    key: BrickKey,
    stamp: BrickStamp,
    cells: Box<[CellSummary; BRICK_CELL_COUNT]>,
    aggregate: CellSummary,
}

impl SummaryBrick {
    pub fn from_cells(
        key: BrickKey,
        stamp: BrickStamp,
        cells: [CellSummary; BRICK_CELL_COUNT],
    ) -> Result<Self, HierarchyError> {
        validate_lod(key.lod)?;
        let aggregate = reduce_brick_summaries(&cells)?;
        Ok(Self {
            key,
            stamp,
            cells: Box::new(cells),
            aggregate,
        })
    }

    pub fn uniform(
        key: BrickKey,
        stamp: BrickStamp,
        cell: CellSummary,
    ) -> Result<Self, HierarchyError> {
        Self::from_cells(key, stamp, [cell; BRICK_CELL_COUNT])
    }

    pub const fn key(&self) -> BrickKey {
        self.key
    }

    pub const fn stamp(&self) -> BrickStamp {
        self.stamp
    }

    pub const fn aggregate(&self) -> CellSummary {
        self.aggregate
    }

    pub fn cells(&self) -> &[CellSummary; BRICK_CELL_COUNT] {
        &self.cells
    }

    pub fn cell(&self, index: u16) -> Option<CellSummary> {
        self.cells.get(usize::from(index)).copied()
    }

    pub fn sample_world(
        &self,
        position: WorldVoxel,
    ) -> Result<Option<CellSummary>, HierarchyError> {
        let address = address_of(position, self.key.lod)?;
        if address.key != self.key {
            return Ok(None);
        }
        Ok(self.cell(address.cell_index))
    }

    fn set_cell(&mut self, index: u16, summary: CellSummary) -> Result<(), HierarchyError> {
        let target = self
            .cells
            .get_mut(usize::from(index))
            .ok_or(HierarchyError::CellIndexOutOfRange(index))?;
        *target = summary;
        Ok(())
    }

    fn recompute_aggregate(&mut self) -> Result<(), HierarchyError> {
        self.aggregate = reduce_brick_summaries(&self.cells)?;
        Ok(())
    }
}

/// Integer-only address conversion. Euclidean division makes `-1` land in
/// brick `-1`, local cell `7`, rather than aliasing positive space.
pub fn address_of(position: WorldVoxel, lod: u8) -> Result<BrickAddress, HierarchyError> {
    validate_lod(lod)?;
    let cell_span = cell_span(lod)?;
    let brick_span = cell_span
        .checked_mul(BRICK_EDGE as i64)
        .ok_or(HierarchyError::ArithmeticOverflow)?;

    let bx = checked_brick_coordinate(position.x.div_euclid(brick_span))?;
    let by = checked_brick_coordinate(position.y.div_euclid(brick_span))?;
    let bz = checked_brick_coordinate(position.z.div_euclid(brick_span))?;
    let lx = usize::try_from(position.x.rem_euclid(brick_span) / cell_span)
        .map_err(|_| HierarchyError::ArithmeticOverflow)?;
    let ly = usize::try_from(position.y.rem_euclid(brick_span) / cell_span)
        .map_err(|_| HierarchyError::ArithmeticOverflow)?;
    let lz = usize::try_from(position.z.rem_euclid(brick_span) / cell_span)
        .map_err(|_| HierarchyError::ArithmeticOverflow)?;

    Ok(BrickAddress {
        key: BrickKey::new(lod, bx, by, bz)?,
        cell_index: cell_index(lx, ly, lz)?,
    })
}

pub fn cell_index(x: usize, y: usize, z: usize) -> Result<u16, HierarchyError> {
    if x >= BRICK_EDGE || y >= BRICK_EDGE || z >= BRICK_EDGE {
        return Err(HierarchyError::LocalCellOutOfRange { x, y, z });
    }
    let index = x + z * BRICK_EDGE + y * BRICK_EDGE * BRICK_EDGE;
    u16::try_from(index).map_err(|_| HierarchyError::ArithmeticOverflow)
}

pub fn local_cell(index: u16) -> Result<(usize, usize, usize), HierarchyError> {
    let index = usize::from(index);
    if index >= BRICK_CELL_COUNT {
        return Err(HierarchyError::CellIndexOutOfRange(index as u16));
    }
    let y = index / (BRICK_EDGE * BRICK_EDGE);
    let remainder = index % (BRICK_EDGE * BRICK_EDGE);
    let z = remainder / BRICK_EDGE;
    let x = remainder % BRICK_EDGE;
    Ok((x, y, z))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct WeightedMaterial {
    material: MaterialId,
    weight: u64,
}

/// Reduces exactly eight summaries without heap allocation or caller-sized
/// scratch memory.
pub fn reduce_eight_summaries(samples: &[CellSummary; 8]) -> Result<CellSummary, HierarchyError> {
    reduce_fixed_summaries(samples)
}

/// Reduces exactly one complete brick without heap allocation or
/// caller-sized scratch memory.
pub fn reduce_brick_summaries(
    samples: &[CellSummary; BRICK_CELL_COUNT],
) -> Result<CellSummary, HierarchyError> {
    reduce_fixed_summaries(samples)
}

/// Order-independent reduction used only by the two fixed public entrypoints.
/// Dominant material is weighted by occupancy. Sorting fixed stack storage by
/// material id makes equal-weight selection independent of input order while
/// bounding worst-case work to `O(512 log 512)`.
fn reduce_fixed_summaries<const N: usize>(
    samples: &[CellSummary; N],
) -> Result<CellSummary, HierarchyError> {
    debug_assert!(N == 8 || N == BRICK_CELL_COUNT);

    let mut weighted_materials = [WeightedMaterial::default(); N];
    let mut weighted_count = 0_usize;
    let mut occupancy_sum = 0_u64;
    let mut total_material_weight = 0_u64;
    let mut minimum_occupancy = u8::MAX;
    let mut maximum_occupancy = 0_u8;
    let mut inherited_error = 0_u8;

    for sample in samples {
        occupancy_sum = occupancy_sum
            .checked_add(u64::from(sample.occupancy))
            .ok_or(HierarchyError::ArithmeticOverflow)?;
        minimum_occupancy = minimum_occupancy.min(sample.occupancy);
        maximum_occupancy = maximum_occupancy.max(sample.occupancy);
        inherited_error = inherited_error.max(sample.error);

        if sample.occupancy == 0 {
            continue;
        }
        total_material_weight = total_material_weight
            .checked_add(u64::from(sample.occupancy))
            .ok_or(HierarchyError::ArithmeticOverflow)?;
        weighted_materials[weighted_count] = WeightedMaterial {
            material: sample.material,
            weight: u64::from(sample.occupancy),
        };
        weighted_count += 1;
    }

    let rounded_occupancy = (occupancy_sum + (N as u64 / 2)) / N as u64;
    let mut occupancy =
        u8::try_from(rounded_occupancy).map_err(|_| HierarchyError::ArithmeticOverflow)?;
    if total_material_weight > 0 && occupancy == 0 {
        occupancy = 1;
    }
    if total_material_weight == 0 {
        return Ok(CellSummary::new(
            0,
            occupancy,
            inherited_error.max(maximum_occupancy.saturating_sub(minimum_occupancy)),
        ));
    }

    weighted_materials[..weighted_count].sort_unstable_by_key(|entry| entry.material);
    let mut dominant = WeightedMaterial::default();
    let mut secondary_weight = 0_u64;
    let mut cursor = 0_usize;
    while cursor < weighted_count {
        let material = weighted_materials[cursor].material;
        let mut weight = 0_u64;
        while cursor < weighted_count && weighted_materials[cursor].material == material {
            weight = weight
                .checked_add(weighted_materials[cursor].weight)
                .ok_or(HierarchyError::ArithmeticOverflow)?;
            cursor += 1;
        }
        if weight > dominant.weight || (weight == dominant.weight && material < dominant.material) {
            secondary_weight = secondary_weight.max(dominant.weight);
            dominant = WeightedMaterial { material, weight };
        } else {
            secondary_weight = secondary_weight.max(weight);
        }
    }
    let material_error = ((u128::from(secondary_weight) * u128::from(u8::MAX))
        / u128::from(total_material_weight))
    .min(u128::from(u8::MAX)) as u8;
    let occupancy_error = maximum_occupancy.saturating_sub(minimum_occupancy);

    Ok(CellSummary::new(
        dominant.material,
        occupancy,
        inherited_error.max(occupancy_error).max(material_error),
    ))
}

/// Deterministically reduces exactly eight child bricks into their parent.
/// Child slice order is irrelevant; spatial octants are recovered from keys.
fn reduce_child_bricks_validated(
    parent_key: BrickKey,
    children: &[SummaryBrick],
) -> Result<SummaryBrick, HierarchyError> {
    if parent_key.lod == 0 {
        return Err(HierarchyError::LeafHasNoChildren);
    }
    if children.len() != 8 {
        return Err(HierarchyError::WrongChildCount(children.len()));
    }

    let first = children.first().ok_or(HierarchyError::WrongChildCount(0))?;
    let epoch = first.stamp.epoch;
    let source_version = first.stamp.source_version;
    let mut overlay_version = first.stamp.overlay_version;
    let mut octants: [Option<&SummaryBrick>; 8] = [None; 8];

    for child in children {
        if child.key.lod + 1 != parent_key.lod || child.key.parent()? != parent_key {
            return Err(HierarchyError::UnexpectedChild {
                parent: parent_key,
                child: child.key,
            });
        }
        if child.stamp.epoch != epoch {
            return Err(HierarchyError::StaleEpoch {
                expected: epoch,
                found: child.stamp.epoch,
            });
        }
        if child.stamp.source_version != source_version {
            return Err(HierarchyError::StaleVersion {
                minimum: source_version,
                found: child.stamp.source_version,
            });
        }
        overlay_version = overlay_version.max(child.stamp.overlay_version);
        let ox = child.key.x.rem_euclid(2) as usize;
        let oy = child.key.y.rem_euclid(2) as usize;
        let oz = child.key.z.rem_euclid(2) as usize;
        let octant = octant_index(ox, oy, oz);
        if octants[octant].replace(child).is_some() {
            return Err(HierarchyError::DuplicateChild(child.key));
        }
    }
    if octants.iter().any(Option::is_none) {
        return Err(HierarchyError::MissingChild(parent_key));
    }

    let mut parent_cells = [CellSummary::EMPTY; BRICK_CELL_COUNT];
    for py in 0..BRICK_EDGE {
        for pz in 0..BRICK_EDGE {
            for px in 0..BRICK_EDGE {
                let mut fine = [CellSummary::EMPTY; 8];
                for dy in 0..2 {
                    for dz in 0..2 {
                        for dx in 0..2 {
                            let gx = px * 2 + dx;
                            let gy = py * 2 + dy;
                            let gz = pz * 2 + dz;
                            let child_index =
                                octant_index(gx / BRICK_EDGE, gy / BRICK_EDGE, gz / BRICK_EDGE);
                            let local_index =
                                cell_index(gx % BRICK_EDGE, gy % BRICK_EDGE, gz % BRICK_EDGE)?;
                            fine[octant_index(dx, dy, dz)] = octants[child_index]
                                .expect("all child octants validated")
                                .cell(local_index)
                                .ok_or(HierarchyError::CellIndexOutOfRange(local_index))?;
                        }
                    }
                }
                let parent_index = cell_index(px, py, pz)?;
                parent_cells[usize::from(parent_index)] = reduce_eight_summaries(&fine)?;
            }
        }
    }

    SummaryBrick::from_cells(
        parent_key,
        BrickStamp {
            epoch,
            source_version,
            overlay_version,
        },
        parent_cells,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheInsert {
    Installed,
    Updated,
    Idempotent,
    Evicted(BrickKey),
}

#[derive(Debug)]
struct ResidentSlot {
    brick: SummaryBrick,
    referenced: bool,
}

/// Deterministic second-chance/clock cache with a compile-time production cap.
///
/// The sorted lookup vector makes lookup order deterministic and reserves its
/// full capacity once. The only per-resident allocation is the fixed brick
/// payload. Public callers cannot raise either the brick or byte budget.
#[derive(Debug)]
struct ClockBrickCache {
    epoch: u64,
    slots: Vec<Option<ResidentSlot>>,
    lookup: Vec<(BrickKey, usize)>,
    hand: usize,
}

impl ClockBrickCache {
    pub fn new(epoch: u64) -> Self {
        Self::with_capacity(epoch, MIDFIELD_RESIDENT_BRICK_LIMIT)
    }

    fn with_capacity(epoch: u64, capacity: usize) -> Self {
        debug_assert!(capacity > 0);
        debug_assert!(capacity <= MIDFIELD_RESIDENT_BRICK_LIMIT);
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, || None);
        Self {
            epoch,
            slots,
            lookup: Vec::with_capacity(capacity),
            hand: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    pub fn len(&self) -> usize {
        self.lookup.len()
    }

    pub fn hard_capacity_bytes(&self) -> usize {
        checked_accounted_cache_bytes(self.capacity())
            .expect("fixed cache capacity has a compile-time-accounted size")
    }

    #[cfg(test)]
    fn resident_payload_bytes(&self) -> usize {
        self.len() * BRICK_PAYLOAD_BYTES
    }

    pub fn begin_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
        for slot in &mut self.slots {
            *slot = None;
        }
        self.lookup.clear();
        self.hand = 0;
    }

    pub fn insert(&mut self, brick: SummaryBrick) -> Result<CacheInsert, HierarchyError> {
        if brick.stamp.epoch != self.epoch {
            return Err(HierarchyError::StaleEpoch {
                expected: self.epoch,
                found: brick.stamp.epoch,
            });
        }

        match self
            .lookup
            .binary_search_by_key(&brick.key, |entry| entry.0)
        {
            Ok(lookup_index) => {
                let slot_index = self.lookup[lookup_index].1;
                let slot = self.slots[slot_index]
                    .as_mut()
                    .expect("lookup only points at occupied slots");
                match compare_stamp_freshness(brick.stamp, slot.brick.stamp) {
                    StampFreshness::Stale | StampFreshness::Crossed => {
                        return Err(HierarchyError::StaleBrickStamp {
                            resident: slot.brick.stamp,
                            candidate: brick.stamp,
                        });
                    }
                    StampFreshness::Equal if brick == slot.brick => {
                        slot.referenced = true;
                        return Ok(CacheInsert::Idempotent);
                    }
                    StampFreshness::Equal => {
                        return Err(HierarchyError::ConflictingBrickStamp {
                            key: brick.key,
                            stamp: brick.stamp,
                        });
                    }
                    StampFreshness::Newer => {}
                }
                slot.brick = brick;
                slot.referenced = true;
                Ok(CacheInsert::Updated)
            }
            Err(_) => {
                let brick_key = brick.key;
                let vacant = self.slots.iter().position(Option::is_none);
                let (slot_index, evicted) = if let Some(slot_index) = vacant {
                    (slot_index, None)
                } else {
                    let slot_index = self.select_clock_victim();
                    let evicted = self.slots[slot_index]
                        .as_ref()
                        .map(|slot| slot.brick.key)
                        .expect("full cache victim must be occupied");
                    self.remove_lookup(evicted);
                    (slot_index, Some(evicted))
                };

                self.slots[slot_index] = Some(ResidentSlot {
                    brick,
                    referenced: true,
                });
                let lookup_index = self
                    .lookup
                    .binary_search_by_key(&brick_key, |entry| entry.0)
                    .expect_err("new brick key cannot already be resident");
                self.lookup.insert(lookup_index, (brick_key, slot_index));
                debug_assert!(self.lookup.len() <= self.capacity());
                Ok(evicted.map_or(CacheInsert::Installed, CacheInsert::Evicted))
            }
        }
    }

    pub fn get(
        &mut self,
        key: BrickKey,
        expected_epoch: u64,
    ) -> Result<Option<&SummaryBrick>, HierarchyError> {
        self.validate_epoch(expected_epoch)?;
        let Ok(index) = self.lookup.binary_search_by_key(&key, |entry| entry.0) else {
            return Ok(None);
        };
        let slot_index = self.lookup[index].1;
        let slot = self.slots[slot_index]
            .as_mut()
            .expect("lookup only points at occupied slots");
        slot.referenced = true;
        Ok(Some(&slot.brick))
    }

    pub fn invalidate(&mut self, key: BrickKey) -> bool {
        let Ok(index) = self.lookup.binary_search_by_key(&key, |entry| entry.0) else {
            return false;
        };
        let (_, slot_index) = self.lookup.remove(index);
        self.slots[slot_index] = None;
        true
    }

    fn validate_epoch(&self, expected_epoch: u64) -> Result<(), HierarchyError> {
        if expected_epoch != self.epoch {
            return Err(HierarchyError::StaleEpoch {
                expected: self.epoch,
                found: expected_epoch,
            });
        }
        Ok(())
    }

    fn remove_lookup(&mut self, key: BrickKey) {
        let index = self
            .lookup
            .binary_search_by_key(&key, |entry| entry.0)
            .expect("resident victim must be present in lookup");
        self.lookup.remove(index);
    }

    fn select_clock_victim(&mut self) -> usize {
        let capacity = self.capacity();
        debug_assert!(capacity > 0);
        loop {
            let index = self.hand;
            self.hand = (self.hand + 1) % capacity;
            let slot = self.slots[index]
                .as_mut()
                .expect("clock victim selection only runs when cache is full");
            if slot.referenced {
                slot.referenced = false;
            } else {
                return index;
            }
        }
    }
}

impl Default for ClockBrickCache {
    fn default() -> Self {
        Self::new(0)
    }
}

pub const fn checked_accounted_cache_bytes(capacity: usize) -> Option<usize> {
    capacity.checked_mul(
        BRICK_PAYLOAD_BYTES + size_of::<Option<ResidentSlot>>() + size_of::<(BrickKey, usize)>(),
    )
}

pub const MIDFIELD_RESIDENT_BYTE_LIMIT: usize =
    match checked_accounted_cache_bytes(MIDFIELD_RESIDENT_BRICK_LIMIT) {
        Some(bytes) => bytes,
        None => panic!("fixed midfield cache byte accounting overflowed"),
    };

/// Authoritative replacement information for one edited source voxel.
///
/// `before` is retained across a chain of edits, so coarser occupancy can be
/// adjusted against regenerated base data after every summary eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditRecord {
    pub epoch: u64,
    pub version: u64,
    pub position: WorldVoxel,
    pub before: CellSummary,
    pub after: CellSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditWriteOutcome {
    pub version: u64,
    pub affected_bricks: Vec<BrickKey>,
    pub idempotent: bool,
}

/// Sparse edit records and their derived macro-cell index.
///
/// This store is intentionally not part of [`ClockBrickCache`]. Its snapshot
/// is save data; the cache may evict every summary without touching a record.
#[derive(Debug)]
struct SparseEditOverlay {
    epoch: u64,
    max_lod: u8,
    latest_version: u64,
    records: BTreeMap<WorldVoxel, EditRecord>,
    index: BTreeMap<(BrickKey, u16), BTreeSet<WorldVoxel>>,
    key_versions: BTreeMap<BrickKey, u64>,
}

impl SparseEditOverlay {
    pub fn new(epoch: u64, max_lod: u8) -> Result<Self, HierarchyError> {
        validate_lod(max_lod)?;
        Ok(Self {
            epoch,
            max_lod,
            latest_version: 0,
            records: BTreeMap::new(),
            index: BTreeMap::new(),
            key_versions: BTreeMap::new(),
        })
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn version_for(&self, key: BrickKey) -> Result<u64, HierarchyError> {
        self.validate_key_lod(key)?;
        Ok(self.key_versions.get(&key).copied().unwrap_or(0))
    }

    pub fn record_edit(&mut self, record: EditRecord) -> Result<EditWriteOutcome, HierarchyError> {
        self.validate_epoch(record.epoch)?;
        if record.version == 0 {
            return Err(HierarchyError::InvalidEditVersion(0));
        }

        if let Some(existing) = self.records.get(&record.position).copied() {
            if existing == record {
                return Ok(EditWriteOutcome {
                    version: record.version,
                    affected_bricks: addresses_for(record.position, self.max_lod)?
                        .into_iter()
                        .map(|address| address.key)
                        .collect(),
                    idempotent: true,
                });
            }
            if record.version <= self.latest_version {
                return Err(HierarchyError::StaleVersion {
                    minimum: self.latest_version.saturating_add(1),
                    found: record.version,
                });
            }
            if existing.after != record.before {
                return Err(HierarchyError::EditChainMismatch {
                    position: record.position,
                });
            }
        } else if record.version <= self.latest_version {
            return Err(HierarchyError::StaleVersion {
                minimum: self.latest_version.saturating_add(1),
                found: record.version,
            });
        }

        let addresses = addresses_for(record.position, self.max_lod)?;
        let is_new = !self.records.contains_key(&record.position);
        let stored = if let Some(existing) = self.records.get(&record.position) {
            EditRecord {
                before: existing.before,
                ..record
            }
        } else {
            record
        };

        if is_new {
            for address in &addresses {
                self.index
                    .entry((address.key, address.cell_index))
                    .or_default()
                    .insert(record.position);
            }
        }
        for address in &addresses {
            self.key_versions.insert(address.key, record.version);
        }
        self.records.insert(record.position, stored);
        self.latest_version = record.version;

        Ok(EditWriteOutcome {
            version: record.version,
            affected_bricks: addresses.into_iter().map(|address| address.key).collect(),
            idempotent: false,
        })
    }

    pub fn snapshot(&self) -> Vec<EditRecord> {
        let mut records: Vec<_> = self.records.values().copied().collect();
        records.sort_by_key(|record| (record.version, record.position));
        records
    }

    pub fn from_snapshot(
        epoch: u64,
        max_lod: u8,
        mut records: Vec<EditRecord>,
    ) -> Result<Self, HierarchyError> {
        records.sort_by_key(|record| (record.version, record.position));
        let mut overlay = Self::new(epoch, max_lod)?;
        for record in records {
            overlay.record_edit(record)?;
        }
        Ok(overlay)
    }

    pub fn apply_to_brick(&self, base: &SummaryBrick) -> Result<SummaryBrick, HierarchyError> {
        self.validate_epoch(base.stamp.epoch)?;
        let expected_overlay_version = self.version_for(base.key)?;
        if base.stamp.overlay_version != expected_overlay_version {
            return Err(HierarchyError::StaleVersion {
                minimum: expected_overlay_version,
                found: base.stamp.overlay_version,
            });
        }

        let mut reconstructed = base.clone();
        let start = (base.key, 0_u16);
        let end = (base.key, (BRICK_CELL_COUNT - 1) as u16);
        for ((_, cell), positions) in self.index.range(start..=end) {
            let base_cell = reconstructed
                .cell(*cell)
                .ok_or(HierarchyError::CellIndexOutOfRange(*cell))?;
            let merged = self.merge_indexed_cell(base.key.lod, base_cell, positions)?;
            reconstructed.set_cell(*cell, merged)?;
        }
        reconstructed.stamp.overlay_version = expected_overlay_version;
        reconstructed.recompute_aggregate()?;
        Ok(reconstructed)
    }

    fn merge_indexed_cell(
        &self,
        lod: u8,
        base: CellSummary,
        positions: &BTreeSet<WorldVoxel>,
    ) -> Result<CellSummary, HierarchyError> {
        if lod == 0 {
            let position = positions
                .iter()
                .next()
                .ok_or(HierarchyError::MissingEditRecord)?;
            return self
                .records
                .get(position)
                .map(|record| record.after)
                .ok_or(HierarchyError::MissingEditRecord);
        }

        let volume_shift = u32::from(lod) * 3;
        let voxel_count = 1_u128
            .checked_shl(volume_shift)
            .ok_or(HierarchyError::ArithmeticOverflow)?;
        let maximum_mass = voxel_count
            .checked_mul(u128::from(u8::MAX))
            .ok_or(HierarchyError::ArithmeticOverflow)?;
        let mut occupancy_mass = i128::try_from(
            voxel_count
                .checked_mul(u128::from(base.occupancy))
                .ok_or(HierarchyError::ArithmeticOverflow)?,
        )
        .map_err(|_| HierarchyError::ArithmeticOverflow)?;
        let mut edited_materials = BTreeMap::<MaterialId, u64>::new();

        for position in positions {
            let record = self
                .records
                .get(position)
                .ok_or(HierarchyError::MissingEditRecord)?;
            occupancy_mass = occupancy_mass
                .checked_add(i128::from(record.after.occupancy))
                .and_then(|value| value.checked_sub(i128::from(record.before.occupancy)))
                .ok_or(HierarchyError::ArithmeticOverflow)?;
            if record.after.occupancy > 0 {
                let weight = edited_materials.entry(record.after.material).or_default();
                *weight = weight
                    .checked_add(u64::from(record.after.occupancy))
                    .ok_or(HierarchyError::ArithmeticOverflow)?;
            }
        }

        let maximum_mass =
            i128::try_from(maximum_mass).map_err(|_| HierarchyError::ArithmeticOverflow)?;
        occupancy_mass = occupancy_mass.clamp(0, maximum_mass);
        let rounded = occupancy_mass
            .checked_add(
                i128::try_from(voxel_count / 2).map_err(|_| HierarchyError::ArithmeticOverflow)?,
            )
            .ok_or(HierarchyError::ArithmeticOverflow)?;
        let mut occupancy = u8::try_from(
            rounded
                / i128::try_from(voxel_count).map_err(|_| HierarchyError::ArithmeticOverflow)?,
        )
        .map_err(|_| HierarchyError::ArithmeticOverflow)?;
        if occupancy_mass > 0 && occupancy == 0 {
            occupancy = 1;
        }

        let material = if occupancy == 0 {
            0
        } else if base.occupancy > 0 {
            base.material
        } else {
            edited_materials
                .into_iter()
                .max_by(|(material_a, weight_a), (material_b, weight_b)| {
                    weight_a
                        .cmp(weight_b)
                        .then_with(|| material_b.cmp(material_a))
                })
                .map(|(material, _)| material)
                .unwrap_or(base.material)
        };

        Ok(CellSummary::new(material, occupancy, u8::MAX))
    }

    fn validate_epoch(&self, epoch: u64) -> Result<(), HierarchyError> {
        if epoch != self.epoch {
            return Err(HierarchyError::StaleEpoch {
                expected: self.epoch,
                found: epoch,
            });
        }
        Ok(())
    }

    fn validate_key_lod(&self, key: BrickKey) -> Result<(), HierarchyError> {
        if key.lod > self.max_lod {
            return Err(HierarchyError::LodOutsideHierarchy {
                requested: key.lod,
                maximum: self.max_lod,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GenerationTicket {
    key: BrickKey,
    stamp: BrickStamp,
    task_nonce: u64,
}

impl GenerationTicket {
    pub const fn key(self) -> BrickKey {
        self.key
    }

    pub const fn stamp(self) -> BrickStamp {
        self.stamp
    }

    pub const fn task_nonce(self) -> u64 {
        self.task_nonce
    }
}

pub const ACTIVE_GENERATION_TASK_BYTES: usize =
    MAX_ACTIVE_GENERATION_TASKS * size_of::<Option<GenerationTicket>>();

#[derive(Debug)]
struct ActiveGenerationTasks {
    slots: [Option<GenerationTicket>; MAX_ACTIVE_GENERATION_TASKS],
    next_nonce: u64,
}

impl ActiveGenerationTasks {
    fn new() -> Self {
        Self {
            slots: [None; MAX_ACTIVE_GENERATION_TASKS],
            next_nonce: 1,
        }
    }

    fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    fn issue(
        &mut self,
        key: BrickKey,
        stamp: BrickStamp,
    ) -> Result<GenerationTicket, HierarchyError> {
        let task_nonce = self.next_nonce;
        self.next_nonce = task_nonce
            .checked_add(1)
            .ok_or(HierarchyError::TaskNonceExhausted)?;
        let ticket = GenerationTicket {
            key,
            stamp,
            task_nonce,
        };

        if let Some(slot) = self
            .slots
            .iter_mut()
            .find(|slot| slot.is_some_and(|active| active.key == key))
        {
            *slot = Some(ticket);
            return Ok(ticket);
        }
        let slot = self.slots.iter_mut().find(|slot| slot.is_none()).ok_or(
            HierarchyError::ActiveTaskLimitReached(MAX_ACTIVE_GENERATION_TASKS),
        )?;
        *slot = Some(ticket);
        Ok(ticket)
    }

    fn validate(&self, ticket: GenerationTicket) -> Result<(), HierarchyError> {
        let Some(active) = self
            .slots
            .iter()
            .flatten()
            .find(|active| active.key == ticket.key)
            .copied()
        else {
            return Err(HierarchyError::UnknownGenerationTask {
                key: ticket.key,
                nonce: ticket.task_nonce,
            });
        };
        if active != ticket {
            return Err(HierarchyError::StaleTaskNonce {
                key: ticket.key,
                expected: active.task_nonce,
                found: ticket.task_nonce,
            });
        }
        Ok(())
    }

    fn consume(&mut self, ticket: GenerationTicket) -> Result<(), HierarchyError> {
        self.validate(ticket)?;
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.is_some_and(|active| active == ticket))
            .expect("validated generation ticket remains resident");
        *slot = None;
        Ok(())
    }

    fn cancel_keys(&mut self, keys: &[BrickKey]) {
        for slot in &mut self.slots {
            if slot.is_some_and(|ticket| keys.contains(&ticket.key)) {
                *slot = None;
            }
        }
    }

    fn clear(&mut self) {
        self.slots.fill(None);
    }

    fn reset_epoch(&mut self) {
        self.clear();
        self.next_nonce = 1;
    }
}

impl Default for ActiveGenerationTasks {
    fn default() -> Self {
        Self::new()
    }
}

/// Self-budgeted pure coordinator intended for near/far integration.
#[derive(Debug)]
pub struct VirtualVoxelHierarchy {
    epoch: u64,
    source_version: u64,
    max_lod: u8,
    residency: ClockBrickCache,
    overlays: SparseEditOverlay,
    active_tasks: ActiveGenerationTasks,
}

impl VirtualVoxelHierarchy {
    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            source_version: 0,
            max_lod: DEFAULT_MAX_LOD,
            residency: ClockBrickCache::new(epoch),
            overlays: SparseEditOverlay::new(epoch, DEFAULT_MAX_LOD)
                .expect("compile-time default LOD is valid"),
            active_tasks: ActiveGenerationTasks::new(),
        }
    }

    #[cfg(test)]
    fn with_test_capacity(epoch: u64, capacity: usize, max_lod: u8) -> Self {
        Self {
            epoch,
            source_version: 0,
            max_lod,
            residency: ClockBrickCache::with_capacity(epoch, capacity),
            overlays: SparseEditOverlay::new(epoch, max_lod).expect("test LOD is valid"),
            active_tasks: ActiveGenerationTasks::new(),
        }
    }

    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    pub const fn max_lod(&self) -> u8 {
        self.max_lod
    }

    pub const fn source_version(&self) -> u64 {
        self.source_version
    }

    pub fn resident_bricks(&self) -> usize {
        self.residency.len()
    }

    pub fn resident_limit(&self) -> usize {
        self.residency.capacity()
    }

    pub fn hard_resident_byte_limit(&self) -> usize {
        self.residency.hard_capacity_bytes()
    }

    pub fn active_generation_tasks(&self) -> usize {
        self.active_tasks.len()
    }

    pub const fn active_generation_task_limit(&self) -> usize {
        MAX_ACTIVE_GENERATION_TASKS
    }

    pub const fn hard_active_generation_task_bytes(&self) -> usize {
        ACTIVE_GENERATION_TASK_BYTES
    }

    pub fn edit_count(&self) -> usize {
        self.overlays.len()
    }

    /// Advances the global generator/source authority. Summary residency and
    /// active tasks are reconstructible and are discarded; sparse edits remain
    /// authoritative. A global version avoids an unbounded per-travel-key map.
    pub fn advance_source_version(&mut self, source_version: u64) -> Result<(), HierarchyError> {
        if source_version <= self.source_version {
            return Err(HierarchyError::StaleVersion {
                minimum: self.source_version.saturating_add(1),
                found: source_version,
            });
        }
        self.source_version = source_version;
        self.residency.begin_epoch(self.epoch);
        self.active_tasks.clear();
        Ok(())
    }

    /// Issues the only authority that can install one generated or reduced
    /// brick. Issuing a replacement for the same key cancels the older nonce.
    pub fn begin_generation(&mut self, key: BrickKey) -> Result<GenerationTicket, HierarchyError> {
        self.validate_key_lod(key)?;
        let stamp = BrickStamp {
            epoch: self.epoch,
            source_version: self.source_version,
            overlay_version: self.overlays.version_for(key)?,
        };
        self.active_tasks.issue(key, stamp)
    }

    /// Applies authoritative edits to a generated base summary and installs
    /// only the reconstructed cache value. A task whose overlay stamp became
    /// stale is rejected before it can overwrite newer edits.
    pub fn install_generated_base(
        &mut self,
        ticket: GenerationTicket,
        base: SummaryBrick,
    ) -> Result<CacheInsert, HierarchyError> {
        self.validate_generation_payload(ticket, &base)?;
        let reconstructed = self.overlays.apply_to_brick(&base)?;
        let outcome = self.residency.insert(reconstructed)?;
        self.active_tasks.consume(ticket)?;
        Ok(outcome)
    }

    /// Validates every child against current source/edit authority and current
    /// residency before reducing and installing a parent. There is deliberately
    /// no public unvalidated resolved-brick installation path.
    pub fn reduce_and_install_parent(
        &mut self,
        ticket: GenerationTicket,
        children: &[SummaryBrick],
    ) -> Result<CacheInsert, HierarchyError> {
        self.validate_ticket_current(ticket)?;
        if ticket.key.lod == 0 {
            return Err(HierarchyError::LeafHasNoChildren);
        }
        for child in children {
            self.validate_key_lod(child.key)?;
            self.validate_epoch(child.stamp.epoch)?;
            let expected_overlay = self.overlays.version_for(child.key)?;
            if child.stamp.source_version != self.source_version
                || child.stamp.overlay_version != expected_overlay
            {
                return Err(HierarchyError::StaleBrickStamp {
                    resident: BrickStamp {
                        epoch: self.epoch,
                        source_version: self.source_version,
                        overlay_version: expected_overlay,
                    },
                    candidate: child.stamp,
                });
            }
            let Some(resident) = self.residency.get(child.key, self.epoch)? else {
                return Err(HierarchyError::MissingResidentChild(child.key));
            };
            if resident != child {
                return Err(HierarchyError::ConflictingBrickStamp {
                    key: child.key,
                    stamp: child.stamp,
                });
            }
        }
        let parent = reduce_child_bricks_validated(ticket.key, children)?;
        if parent.stamp != ticket.stamp {
            return Err(HierarchyError::TaskPayloadMismatch {
                key: ticket.key,
                nonce: ticket.task_nonce,
            });
        }
        let outcome = self.residency.insert(parent)?;
        self.active_tasks.consume(ticket)?;
        Ok(outcome)
    }

    pub fn sample(
        &mut self,
        position: WorldVoxel,
        lod: u8,
        expected_epoch: u64,
    ) -> Result<Option<CellSummary>, HierarchyError> {
        self.validate_epoch(expected_epoch)?;
        self.validate_requested_lod(lod)?;
        let address = address_of(position, lod)?;
        let Some(brick) = self.residency.get(address.key, expected_epoch)? else {
            return Ok(None);
        };
        let expected_overlay = self.overlays.version_for(address.key)?;
        if brick.stamp.source_version != self.source_version
            || brick.stamp.overlay_version != expected_overlay
        {
            return Err(HierarchyError::StaleBrickStamp {
                resident: BrickStamp {
                    epoch: self.epoch,
                    source_version: self.source_version,
                    overlay_version: expected_overlay,
                },
                candidate: brick.stamp,
            });
        }
        Ok(brick.cell(address.cell_index))
    }

    pub fn record_edit(&mut self, record: EditRecord) -> Result<EditWriteOutcome, HierarchyError> {
        self.validate_epoch(record.epoch)?;
        let outcome = self.overlays.record_edit(record)?;
        for key in &outcome.affected_bricks {
            self.residency.invalidate(*key);
        }
        self.active_tasks.cancel_keys(&outcome.affected_bricks);
        Ok(outcome)
    }

    pub fn edit_snapshot(&self) -> Vec<EditRecord> {
        self.overlays.snapshot()
    }

    pub fn restore_edits(&mut self, records: Vec<EditRecord>) -> Result<(), HierarchyError> {
        let restored = SparseEditOverlay::from_snapshot(self.epoch, self.max_lod, records)?;
        self.overlays = restored;
        self.residency.begin_epoch(self.epoch);
        self.active_tasks.clear();
        Ok(())
    }

    pub fn begin_epoch(&mut self, epoch: u64) -> bool {
        if epoch == self.epoch {
            return false;
        }
        self.epoch = epoch;
        self.source_version = 0;
        self.residency.begin_epoch(epoch);
        self.overlays =
            SparseEditOverlay::new(epoch, self.max_lod).expect("compile-time default LOD is valid");
        self.active_tasks.reset_epoch();
        true
    }

    fn validate_generation_payload(
        &self,
        ticket: GenerationTicket,
        brick: &SummaryBrick,
    ) -> Result<(), HierarchyError> {
        self.validate_ticket_current(ticket)?;
        if brick.key != ticket.key || brick.stamp != ticket.stamp {
            return Err(HierarchyError::TaskPayloadMismatch {
                key: ticket.key,
                nonce: ticket.task_nonce,
            });
        }
        Ok(())
    }

    fn validate_ticket_current(&self, ticket: GenerationTicket) -> Result<(), HierarchyError> {
        self.validate_key_lod(ticket.key)?;
        self.validate_epoch(ticket.stamp.epoch)?;
        self.active_tasks.validate(ticket)?;
        let expected_overlay = self.overlays.version_for(ticket.key)?;
        let expected_stamp = BrickStamp {
            epoch: self.epoch,
            source_version: self.source_version,
            overlay_version: expected_overlay,
        };
        if ticket.stamp != expected_stamp {
            return Err(HierarchyError::StaleBrickStamp {
                resident: expected_stamp,
                candidate: ticket.stamp,
            });
        }
        Ok(())
    }

    fn validate_key_lod(&self, key: BrickKey) -> Result<(), HierarchyError> {
        self.validate_requested_lod(key.lod)
    }

    fn validate_requested_lod(&self, lod: u8) -> Result<(), HierarchyError> {
        validate_lod(lod)?;
        if lod > self.max_lod {
            return Err(HierarchyError::LodOutsideHierarchy {
                requested: lod,
                maximum: self.max_lod,
            });
        }
        Ok(())
    }

    fn validate_epoch(&self, epoch: u64) -> Result<(), HierarchyError> {
        if epoch != self.epoch {
            return Err(HierarchyError::StaleEpoch {
                expected: self.epoch,
                found: epoch,
            });
        }
        Ok(())
    }
}

impl Default for VirtualVoxelHierarchy {
    fn default() -> Self {
        Self::new(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HierarchyError {
    InvalidLod(u8),
    LodOutsideHierarchy {
        requested: u8,
        maximum: u8,
    },
    ArithmeticOverflow,
    CoordinateOutOfRange(i64),
    LocalCellOutOfRange {
        x: usize,
        y: usize,
        z: usize,
    },
    CellIndexOutOfRange(u16),
    EmptyReduction,
    LeafHasNoChildren,
    WrongChildCount(usize),
    UnexpectedChild {
        parent: BrickKey,
        child: BrickKey,
    },
    DuplicateChild(BrickKey),
    MissingChild(BrickKey),
    StaleEpoch {
        expected: u64,
        found: u64,
    },
    StaleVersion {
        minimum: u64,
        found: u64,
    },
    StaleBrickStamp {
        resident: BrickStamp,
        candidate: BrickStamp,
    },
    ConflictingBrickStamp {
        key: BrickKey,
        stamp: BrickStamp,
    },
    ActiveTaskLimitReached(usize),
    TaskNonceExhausted,
    UnknownGenerationTask {
        key: BrickKey,
        nonce: u64,
    },
    StaleTaskNonce {
        key: BrickKey,
        expected: u64,
        found: u64,
    },
    TaskPayloadMismatch {
        key: BrickKey,
        nonce: u64,
    },
    MissingResidentChild(BrickKey),
    InvalidEditVersion(u64),
    EditChainMismatch {
        position: WorldVoxel,
    },
    MissingEditRecord,
}

impl fmt::Display for HierarchyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLod(lod) => write!(formatter, "LOD {lod} exceeds maximum {MAX_LOD}"),
            Self::LodOutsideHierarchy { requested, maximum } => write!(
                formatter,
                "LOD {requested} exceeds configured hierarchy maximum {maximum}"
            ),
            Self::ArithmeticOverflow => write!(formatter, "checked hierarchy arithmetic overflow"),
            Self::CoordinateOutOfRange(value) => {
                write!(formatter, "brick coordinate {value} is outside i32 range")
            }
            Self::LocalCellOutOfRange { x, y, z } => {
                write!(formatter, "local cell ({x}, {y}, {z}) is outside 8^3 brick")
            }
            Self::CellIndexOutOfRange(index) => {
                write!(formatter, "cell index {index} is outside brick payload")
            }
            Self::EmptyReduction => write!(formatter, "cannot reduce zero summaries"),
            Self::LeafHasNoChildren => write!(formatter, "LOD 0 brick has no summary children"),
            Self::WrongChildCount(count) => {
                write!(
                    formatter,
                    "parent reduction requires 8 children, found {count}"
                )
            }
            Self::UnexpectedChild { parent, child } => {
                write!(formatter, "brick {child:?} is not a child of {parent:?}")
            }
            Self::DuplicateChild(child) => write!(formatter, "duplicate child {child:?}"),
            Self::MissingChild(parent) => write!(formatter, "parent {parent:?} is missing a child"),
            Self::StaleEpoch { expected, found } => {
                write!(
                    formatter,
                    "stale epoch {found}; current epoch is {expected}"
                )
            }
            Self::StaleVersion { minimum, found } => {
                write!(
                    formatter,
                    "stale version {found}; minimum accepted is {minimum}"
                )
            }
            Self::StaleBrickStamp {
                resident,
                candidate,
            } => write!(
                formatter,
                "brick stamp {candidate:?} is stale or crossed relative to {resident:?}"
            ),
            Self::ConflictingBrickStamp { key, stamp } => write!(
                formatter,
                "brick {key:?} has conflicting payloads for identical stamp {stamp:?}"
            ),
            Self::ActiveTaskLimitReached(limit) => {
                write!(formatter, "active generation task limit {limit} is full")
            }
            Self::TaskNonceExhausted => write!(formatter, "generation task nonce exhausted"),
            Self::UnknownGenerationTask { key, nonce } => write!(
                formatter,
                "generation task {nonce} for brick {key:?} is not active"
            ),
            Self::StaleTaskNonce {
                key,
                expected,
                found,
            } => write!(
                formatter,
                "generation task {found} for brick {key:?} was replaced by task {expected}"
            ),
            Self::TaskPayloadMismatch { key, nonce } => write!(
                formatter,
                "generation task {nonce} payload does not match brick {key:?}"
            ),
            Self::MissingResidentChild(key) => {
                write!(formatter, "parent reduction child {key:?} is not resident")
            }
            Self::InvalidEditVersion(version) => {
                write!(formatter, "edit version {version} is reserved/invalid")
            }
            Self::EditChainMismatch { position } => {
                write!(
                    formatter,
                    "edit chain does not match prior value at {position:?}"
                )
            }
            Self::MissingEditRecord => write!(formatter, "overlay index references no edit record"),
        }
    }
}

impl std::error::Error for HierarchyError {}

fn validate_lod(lod: u8) -> Result<(), HierarchyError> {
    if lod > MAX_LOD {
        return Err(HierarchyError::InvalidLod(lod));
    }
    Ok(())
}

fn cell_span(lod: u8) -> Result<i64, HierarchyError> {
    validate_lod(lod)?;
    1_i64
        .checked_shl(u32::from(lod))
        .ok_or(HierarchyError::ArithmeticOverflow)
}

fn brick_span(lod: u8) -> Result<i64, HierarchyError> {
    cell_span(lod)?
        .checked_mul(BRICK_EDGE as i64)
        .ok_or(HierarchyError::ArithmeticOverflow)
}

fn checked_brick_coordinate(value: i64) -> Result<i32, HierarchyError> {
    i32::try_from(value).map_err(|_| HierarchyError::CoordinateOutOfRange(value))
}

const fn octant_index(x: usize, y: usize, z: usize) -> usize {
    x + z * 2 + y * 4
}

fn addresses_for(position: WorldVoxel, max_lod: u8) -> Result<Vec<BrickAddress>, HierarchyError> {
    validate_lod(max_lod)?;
    let mut addresses = Vec::with_capacity(usize::from(max_lod) + 1);
    for lod in 0..=max_lod {
        addresses.push(address_of(position, lod)?);
    }
    Ok(addresses)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StampFreshness {
    Stale,
    Equal,
    Newer,
    Crossed,
}

fn compare_stamp_freshness(candidate: BrickStamp, resident: BrickStamp) -> StampFreshness {
    if candidate.epoch != resident.epoch {
        return StampFreshness::Crossed;
    }
    let source = candidate.source_version.cmp(&resident.source_version);
    let overlay = candidate.overlay_version.cmp(&resident.overlay_version);
    match (source, overlay) {
        (std::cmp::Ordering::Equal, std::cmp::Ordering::Equal) => StampFreshness::Equal,
        (std::cmp::Ordering::Less, std::cmp::Ordering::Greater)
        | (std::cmp::Ordering::Greater, std::cmp::Ordering::Less) => StampFreshness::Crossed,
        (std::cmp::Ordering::Less, _) | (_, std::cmp::Ordering::Less) => StampFreshness::Stale,
        _ => StampFreshness::Newer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;
    use std::time::Instant;

    fn stamp(epoch: u64, source_version: u64, overlay_version: u64) -> BrickStamp {
        BrickStamp {
            epoch,
            source_version,
            overlay_version,
        }
    }

    fn uniform_brick(
        key: BrickKey,
        epoch: u64,
        source_version: u64,
        material: MaterialId,
    ) -> SummaryBrick {
        SummaryBrick::uniform(
            key,
            stamp(epoch, source_version, 0),
            CellSummary::solid(material),
        )
        .unwrap()
    }

    fn begin_uniform_generation(
        hierarchy: &mut VirtualVoxelHierarchy,
        key: BrickKey,
        cell: CellSummary,
    ) -> (GenerationTicket, SummaryBrick) {
        let ticket = hierarchy.begin_generation(key).unwrap();
        let brick = SummaryBrick::uniform(key, ticket.stamp(), cell).unwrap();
        (ticket, brick)
    }

    fn install_uniform(
        hierarchy: &mut VirtualVoxelHierarchy,
        key: BrickKey,
        cell: CellSummary,
    ) -> CacheInsert {
        let (ticket, brick) = begin_uniform_generation(hierarchy, key, cell);
        hierarchy.install_generated_base(ticket, brick).unwrap()
    }

    #[test]
    fn fixed_payload_and_production_budget_are_pinned() {
        assert_eq!(size_of::<CellSummary>(), 4);
        assert_eq!(BRICK_PAYLOAD_BYTES, 2_048);
        assert_eq!(MIDFIELD_RESIDENT_BRICK_LIMIT, 512);
        assert_eq!(
            MIDFIELD_RESIDENT_BYTE_LIMIT,
            checked_accounted_cache_bytes(MIDFIELD_RESIDENT_BRICK_LIMIT).unwrap()
        );
        assert_eq!(checked_accounted_cache_bytes(usize::MAX), None);
        assert!(MIDFIELD_RESIDENT_BYTE_LIMIT < 2 * 1024 * 1024);
    }

    #[test]
    fn negative_coordinates_use_euclidean_bricks_and_cells() {
        let minus_one = address_of(WorldVoxel::new(-1, -1, -1), 0).unwrap();
        assert_eq!(minus_one.key.coordinates(), (-1, -1, -1));
        assert_eq!(local_cell(minus_one.cell_index).unwrap(), (7, 7, 7));

        let boundary = address_of(WorldVoxel::new(-8, -8, -8), 0).unwrap();
        assert_eq!(boundary.key.coordinates(), (-1, -1, -1));
        assert_eq!(local_cell(boundary.cell_index).unwrap(), (0, 0, 0));

        let beyond = address_of(WorldVoxel::new(-9, -9, -9), 0).unwrap();
        assert_eq!(beyond.key.coordinates(), (-2, -2, -2));
        assert_eq!(local_cell(beyond.cell_index).unwrap(), (7, 7, 7));
    }

    #[test]
    fn deterministic_material_reduction_ignores_input_order() {
        let original = [
            CellSummary::new(9, 200, 1),
            CellSummary::new(4, 200, 3),
            CellSummary::new(9, 55, 2),
            CellSummary::new(4, 55, 8),
            CellSummary::EMPTY,
            CellSummary::new(12, 10, 0),
            CellSummary::new(12, 10, 0),
            CellSummary::EMPTY,
        ];
        let expected = reduce_eight_summaries(&original).unwrap();
        assert_eq!(expected.material, 4, "equal weight must choose lower id");

        for rotation in 0..original.len() {
            let mut permuted = original;
            permuted.rotate_left(rotation);
            if rotation % 2 == 1 {
                permuted.reverse();
            }
            assert_eq!(reduce_eight_summaries(&permuted).unwrap(), expected);
        }
    }

    #[test]
    fn positive_mass_and_refinement_never_become_known_empty() {
        let uncertain = CellSummary::new(0, 0, u8::MAX);
        assert!(!uncertain.is_empty());

        let mut level = CellSummary::solid(19);
        for lod in 1..=MAX_LOD {
            let mut children = [CellSummary::EMPTY; 8];
            children[0] = level;
            level = reduce_eight_summaries(&children).unwrap();
            assert!(level.occupancy > 0, "positive mass vanished at LOD {lod}");
            assert!(!level.is_empty(), "LOD {lod} became known-empty");
        }

        let mut brick = [CellSummary::EMPTY; BRICK_CELL_COUNT];
        brick[BRICK_CELL_COUNT - 1] = CellSummary::new(27, 1, 0);
        let reduced = reduce_brick_summaries(&brick).unwrap();
        assert_eq!(reduced.occupancy, 1);
        assert!(!reduced.is_empty());

        let epoch = 44;
        let position = WorldVoxel::new(0, 0, 0);
        let mut overlay = SparseEditOverlay::new(epoch, MAX_LOD).unwrap();
        overlay
            .record_edit(EditRecord {
                epoch,
                version: 1,
                position,
                before: CellSummary::EMPTY,
                after: CellSummary::solid(31),
            })
            .unwrap();
        for lod in 0..=MAX_LOD {
            let key = address_of(position, lod).unwrap().key;
            let base = SummaryBrick::uniform(
                key,
                BrickStamp {
                    epoch,
                    source_version: 0,
                    overlay_version: overlay.version_for(key).unwrap(),
                },
                CellSummary::EMPTY,
            )
            .unwrap();
            let resolved = overlay.apply_to_brick(&base).unwrap();
            let sample = resolved.sample_world(position).unwrap().unwrap();
            assert!(sample.occupancy > 0, "overlay mass vanished at LOD {lod}");
            assert!(
                !sample.is_empty(),
                "overlay became known-empty at LOD {lod}"
            );
            if lod > 0 {
                assert_eq!(sample.error, u8::MAX);
            }
        }
    }

    #[test]
    fn parent_reduction_is_independent_of_child_completion_order() {
        let parent = BrickKey::new(1, -1, 0, -1).unwrap();
        let mut children: Vec<_> = parent
            .children()
            .unwrap()
            .into_iter()
            .enumerate()
            .map(|(index, key)| uniform_brick(key, 3, 11, (index % 3 + 2) as u16))
            .collect();
        let forward = reduce_child_bricks_validated(parent, &children).unwrap();
        children.reverse();
        let reverse = reduce_child_bricks_validated(parent, &children).unwrap();
        assert_eq!(forward, reverse);
    }

    #[test]
    fn hierarchy_owned_parent_reduction_rejects_a_stale_masked_sibling() {
        let epoch = 45;
        let parent = BrickKey::new(1, 0, 0, 0).unwrap();
        let child_keys = parent.children().unwrap();
        let mut hierarchy = VirtualVoxelHierarchy::with_test_capacity(epoch, 16, 1);
        let mut children = Vec::with_capacity(8);
        for key in child_keys {
            install_uniform(&mut hierarchy, key, CellSummary::EMPTY);
            children.push(
                hierarchy
                    .residency
                    .get(key, epoch)
                    .unwrap()
                    .unwrap()
                    .clone(),
            );
        }
        let stale_first_child = children[0].clone();

        let first_position = child_keys[0].world_origin().unwrap();
        hierarchy
            .record_edit(EditRecord {
                epoch,
                version: 1,
                position: first_position,
                before: CellSummary::EMPTY,
                after: CellSummary::solid(7),
            })
            .unwrap();
        let second_position = child_keys[1].world_origin().unwrap();
        hierarchy
            .record_edit(EditRecord {
                epoch,
                version: 2,
                position: second_position,
                before: CellSummary::EMPTY,
                after: CellSummary::solid(8),
            })
            .unwrap();
        install_uniform(&mut hierarchy, child_keys[1], CellSummary::EMPTY);
        children[0] = stale_first_child;
        children[1] = hierarchy
            .residency
            .get(child_keys[1], epoch)
            .unwrap()
            .unwrap()
            .clone();

        let parent_ticket = hierarchy.begin_generation(parent).unwrap();
        assert!(matches!(
            hierarchy.reduce_and_install_parent(parent_ticket, &children),
            Err(HierarchyError::StaleBrickStamp { .. })
        ));
        assert_eq!(hierarchy.active_generation_tasks(), 1);
    }

    #[test]
    fn hierarchy_owned_parent_reduction_accepts_only_current_resident_children() {
        let epoch = 46;
        let parent = BrickKey::new(1, -1, 0, -1).unwrap();
        let mut hierarchy = VirtualVoxelHierarchy::with_test_capacity(epoch, 16, 1);
        let mut children = Vec::new();
        for (index, key) in parent.children().unwrap().into_iter().enumerate() {
            install_uniform(
                &mut hierarchy,
                key,
                CellSummary::solid(index as MaterialId + 1),
            );
            children.push(
                hierarchy
                    .residency
                    .get(key, epoch)
                    .unwrap()
                    .unwrap()
                    .clone(),
            );
        }
        children.reverse();
        let ticket = hierarchy.begin_generation(parent).unwrap();
        hierarchy
            .reduce_and_install_parent(ticket, &children)
            .unwrap();
        assert!(hierarchy
            .sample(parent.world_origin().unwrap(), 1, epoch)
            .unwrap()
            .is_some());
    }

    #[test]
    fn many_kilometres_of_travel_keep_residency_and_bytes_bounded() {
        const CAPACITY: usize = 31;
        let epoch = 5;
        let mut cache = ClockBrickCache::with_capacity(epoch, CAPACITY);
        let hard_bytes = cache.hard_capacity_bytes();

        for kilometre in -10_000_i32..=10_000_i32 {
            let world_x = i64::from(kilometre) * 1_000;
            let key = address_of(WorldVoxel::new(world_x, 64, -world_x / 3), 4)
                .unwrap()
                .key;
            cache
                .insert(uniform_brick(
                    key,
                    epoch,
                    kilometre.unsigned_abs() as u64 + 1,
                    7,
                ))
                .unwrap();
            assert!(cache.len() <= CAPACITY);
            assert!(cache.resident_payload_bytes() <= CAPACITY * BRICK_PAYLOAD_BYTES);
            assert_eq!(cache.hard_capacity_bytes(), hard_bytes);
        }
        assert_eq!(cache.len(), CAPACITY);
    }

    #[test]
    fn clock_pressure_has_repeatable_victims() {
        fn eviction_trace() -> Vec<BrickKey> {
            let epoch = 6;
            let mut cache = ClockBrickCache::with_capacity(epoch, 3);
            let keys: Vec<_> = (0..8).map(|x| BrickKey::new(0, x, 0, 0).unwrap()).collect();
            let mut evicted = Vec::new();
            for (version, key) in keys.iter().copied().enumerate() {
                if let CacheInsert::Evicted(victim) = cache
                    .insert(uniform_brick(key, epoch, version as u64 + 1, 5))
                    .unwrap()
                {
                    evicted.push(victim);
                }
                if version == 4 {
                    let _ = cache.get(keys[3], epoch).unwrap();
                }
            }
            evicted
        }

        let first = eviction_trace();
        let second = eviction_trace();
        assert_eq!(first, second);
        assert_eq!(first.len(), 5);
    }

    #[test]
    fn cache_stamp_order_is_component_wise_and_equal_payloads_are_checked() {
        let epoch = 47;
        let key = BrickKey::new(0, 0, 0, 0).unwrap();
        let resident =
            SummaryBrick::uniform(key, stamp(epoch, 2, 2), CellSummary::solid(4)).unwrap();
        let mut cache = ClockBrickCache::with_capacity(epoch, 1);
        assert_eq!(
            cache.insert(resident.clone()).unwrap(),
            CacheInsert::Installed
        );
        assert_eq!(
            cache.insert(resident.clone()).unwrap(),
            CacheInsert::Idempotent
        );

        let conflicting =
            SummaryBrick::uniform(key, resident.stamp(), CellSummary::solid(5)).unwrap();
        assert!(matches!(
            cache.insert(conflicting),
            Err(HierarchyError::ConflictingBrickStamp { .. })
        ));
        let crossed =
            SummaryBrick::uniform(key, stamp(epoch, 3, 1), CellSummary::solid(6)).unwrap();
        assert!(matches!(
            cache.insert(crossed),
            Err(HierarchyError::StaleBrickStamp { .. })
        ));
        assert_eq!(
            cache.get(key, epoch).unwrap().unwrap(),
            &resident,
            "rejected conflicts must not replace authority"
        );
    }

    #[test]
    fn bounded_task_tickets_reject_replay_after_replacement_and_eviction() {
        let epoch = 48;
        let key = BrickKey::new(0, 0, 0, 0).unwrap();
        let other = BrickKey::new(0, 1, 0, 0).unwrap();
        let mut hierarchy = VirtualVoxelHierarchy::with_test_capacity(epoch, 1, 1);

        let slow = hierarchy.begin_generation(key).unwrap();
        let slow_payload = SummaryBrick::uniform(key, slow.stamp(), CellSummary::solid(1)).unwrap();
        let replacement = hierarchy.begin_generation(key).unwrap();
        let replacement_payload =
            SummaryBrick::uniform(key, replacement.stamp(), CellSummary::solid(2)).unwrap();
        hierarchy
            .install_generated_base(replacement, replacement_payload)
            .unwrap();
        install_uniform(&mut hierarchy, other, CellSummary::solid(3));
        assert!(hierarchy
            .sample(WorldVoxel::new(0, 0, 0), 0, epoch)
            .unwrap()
            .is_none());
        assert!(matches!(
            hierarchy.install_generated_base(slow, slow_payload),
            Err(HierarchyError::UnknownGenerationTask { .. })
                | Err(HierarchyError::StaleTaskNonce { .. })
        ));

        let pre_source_change = hierarchy.begin_generation(key).unwrap();
        let pre_source_payload =
            SummaryBrick::uniform(key, pre_source_change.stamp(), CellSummary::solid(4)).unwrap();
        hierarchy.advance_source_version(1).unwrap();
        assert!(matches!(
            hierarchy.install_generated_base(pre_source_change, pre_source_payload),
            Err(HierarchyError::UnknownGenerationTask { .. })
                | Err(HierarchyError::StaleBrickStamp { .. })
        ));
    }

    #[test]
    fn active_task_table_has_a_fixed_saturating_ceiling() {
        let epoch = 49;
        let mut hierarchy = VirtualVoxelHierarchy::with_test_capacity(epoch, 1, 1);
        for index in 0..MAX_ACTIVE_GENERATION_TASKS {
            hierarchy
                .begin_generation(BrickKey::new(0, index as i32, 0, 0).unwrap())
                .unwrap();
        }
        assert_eq!(
            hierarchy.active_generation_tasks(),
            MAX_ACTIVE_GENERATION_TASKS
        );
        assert_eq!(
            hierarchy.hard_active_generation_task_bytes(),
            ACTIVE_GENERATION_TASK_BYTES
        );
        assert!(matches!(
            hierarchy.begin_generation(
                BrickKey::new(0, MAX_ACTIVE_GENERATION_TASKS as i32, 0, 0).unwrap()
            ),
            Err(HierarchyError::ActiveTaskLimitReached(
                MAX_ACTIVE_GENERATION_TASKS
            ))
        ));
        hierarchy.advance_source_version(1).unwrap();
        assert_eq!(hierarchy.active_generation_tasks(), 0);
    }

    #[test]
    fn configured_max_lod_guards_every_public_hierarchy_path() {
        let epoch = 50;
        let mut hierarchy = VirtualVoxelHierarchy::with_test_capacity(epoch, 2, 2);
        let outside = BrickKey::new(3, 0, 0, 0).unwrap();
        assert!(matches!(
            hierarchy.begin_generation(outside),
            Err(HierarchyError::LodOutsideHierarchy {
                requested: 3,
                maximum: 2
            })
        ));
        assert!(matches!(
            hierarchy.sample(WorldVoxel::new(0, 0, 0), 3, epoch),
            Err(HierarchyError::LodOutsideHierarchy {
                requested: 3,
                maximum: 2
            })
        ));
        assert!(matches!(
            hierarchy.overlays.version_for(outside),
            Err(HierarchyError::LodOutsideHierarchy { .. })
        ));
        let base = SummaryBrick::uniform(
            outside,
            stamp(epoch, hierarchy.source_version(), 0),
            CellSummary::EMPTY,
        )
        .unwrap();
        assert!(matches!(
            hierarchy.overlays.apply_to_brick(&base),
            Err(HierarchyError::LodOutsideHierarchy { .. })
        ));
    }

    #[test]
    fn edits_survive_summary_eviction_reconstruction_and_snapshot_replay() {
        let epoch = 7;
        let mut hierarchy = VirtualVoxelHierarchy::with_test_capacity(epoch, 1, 3);
        hierarchy.advance_source_version(9).unwrap();
        let edited_position = WorldVoxel::new(1, 1, 1);
        let edited_key = address_of(edited_position, 0).unwrap().key;
        install_uniform(&mut hierarchy, edited_key, CellSummary::EMPTY);
        hierarchy
            .record_edit(EditRecord {
                epoch,
                version: 1,
                position: edited_position,
                before: CellSummary::EMPTY,
                after: CellSummary::solid(42),
            })
            .unwrap();

        install_uniform(&mut hierarchy, edited_key, CellSummary::EMPTY);
        assert_eq!(
            hierarchy.sample(edited_position, 0, epoch).unwrap(),
            Some(CellSummary::solid(42))
        );

        let other_key = BrickKey::new(0, 50, 0, 0).unwrap();
        install_uniform(&mut hierarchy, other_key, CellSummary::solid(3));
        assert!(hierarchy
            .sample(edited_position, 0, epoch)
            .unwrap()
            .is_none());
        assert_eq!(
            hierarchy.edit_count(),
            1,
            "cache eviction must not own edits"
        );

        let snapshot = hierarchy.edit_snapshot();
        let mut restored = VirtualVoxelHierarchy::with_test_capacity(epoch, 1, 3);
        restored.advance_source_version(9).unwrap();
        restored.restore_edits(snapshot).unwrap();
        install_uniform(&mut restored, edited_key, CellSummary::EMPTY);
        assert_eq!(
            restored.sample(edited_position, 0, epoch).unwrap(),
            Some(CellSummary::solid(42))
        );
    }

    #[test]
    fn coarse_overlay_adjusts_occupancy_but_forces_refinement() {
        let epoch = 8;
        let position = WorldVoxel::new(2, 2, 2);
        let key = address_of(position, 1).unwrap().key;
        let mut overlay = SparseEditOverlay::new(epoch, 2).unwrap();
        overlay
            .record_edit(EditRecord {
                epoch,
                version: 1,
                position,
                before: CellSummary::EMPTY,
                after: CellSummary::solid(17),
            })
            .unwrap();
        let base = SummaryBrick::uniform(
            key,
            BrickStamp {
                epoch,
                source_version: 4,
                overlay_version: overlay.version_for(key).unwrap(),
            },
            CellSummary::EMPTY,
        )
        .unwrap();
        let reconstructed = overlay.apply_to_brick(&base).unwrap();
        let cell = reconstructed.sample_world(position).unwrap().unwrap();
        assert_eq!(cell.occupancy, 32, "one of eight voxels rounds to 32/255");
        assert_eq!(cell.material, 17);
        assert_eq!(cell.error, u8::MAX);
    }

    #[test]
    fn overflow_and_i32_extremes_fail_closed_without_wrapping() {
        assert!(matches!(
            address_of(WorldVoxel::new(i64::MAX, 0, 0), 0),
            Err(HierarchyError::CoordinateOutOfRange(_))
        ));
        assert!(matches!(
            BrickKey::new(MAX_LOD, i32::MAX, 0, 0)
                .unwrap()
                .world_origin(),
            Err(HierarchyError::ArithmeticOverflow)
        ));
        assert!(matches!(
            BrickKey::new(1, i32::MAX, 0, 0).unwrap().children(),
            Err(HierarchyError::ArithmeticOverflow)
        ));
        assert!(matches!(
            BrickKey::new(MAX_LOD + 1, 0, 0, 0),
            Err(HierarchyError::InvalidLod(_))
        ));

        let maximum_safe = address_of(
            WorldVoxel::new(i64::from(i32::MAX), i64::from(i32::MIN), 0),
            0,
        )
        .unwrap();
        let origin = maximum_safe.key.world_origin().unwrap();
        assert!(origin.x <= i64::from(i32::MAX));
        assert!(origin.y <= i64::from(i32::MIN));
    }

    #[test]
    fn stale_epochs_and_pre_edit_tasks_are_rejected() {
        let mut hierarchy = VirtualVoxelHierarchy::with_test_capacity(20, 2, 3);
        hierarchy.advance_source_version(1).unwrap();
        let key = BrickKey::new(0, 0, 0, 0).unwrap();
        let old_ticket = hierarchy.begin_generation(key).unwrap();
        let old_task = SummaryBrick::uniform(key, old_ticket.stamp(), CellSummary::EMPTY).unwrap();
        hierarchy
            .record_edit(EditRecord {
                epoch: 20,
                version: 1,
                position: WorldVoxel::new(0, 0, 0),
                before: CellSummary::EMPTY,
                after: CellSummary::solid(2),
            })
            .unwrap();
        assert!(matches!(
            hierarchy.install_generated_base(old_ticket, old_task),
            Err(HierarchyError::UnknownGenerationTask { .. })
        ));

        let current_ticket = hierarchy.begin_generation(key).unwrap();
        let stale_world =
            SummaryBrick::uniform(key, current_ticket.stamp(), CellSummary::EMPTY).unwrap();
        assert!(hierarchy.begin_epoch(21));
        assert!(matches!(
            hierarchy.install_generated_base(current_ticket, stale_world),
            Err(HierarchyError::StaleEpoch {
                expected: 21,
                found: 20
            })
        ));
        assert!(matches!(
            hierarchy.sample(WorldVoxel::new(0, 0, 0), 0, 20),
            Err(HierarchyError::StaleEpoch { .. })
        ));
    }

    #[test]
    fn benchmark_hot_sampling_and_reduction() {
        const SAMPLE_ITERATIONS: usize = 5_000_000;
        const REDUCE_ITERATIONS: usize = 2_500_000;
        let epoch = 99;
        let key = BrickKey::new(0, 0, 0, 0).unwrap();
        let mut hierarchy = VirtualVoxelHierarchy::with_test_capacity(epoch, 1, 1);
        install_uniform(&mut hierarchy, key, CellSummary::solid(6));

        let sampling_started = Instant::now();
        let mut sample_checksum = 0_u64;
        for index in 0..SAMPLE_ITERATIONS {
            let coordinate = (index & 7) as i64;
            let sample = black_box(
                hierarchy
                    .sample(
                        WorldVoxel::new(coordinate, coordinate, coordinate),
                        0,
                        epoch,
                    )
                    .unwrap()
                    .unwrap(),
            );
            sample_checksum = sample_checksum.wrapping_add(u64::from(sample.material));
        }
        let sampling_elapsed = sampling_started.elapsed();

        let inputs = [
            CellSummary::solid(4),
            CellSummary::new(7, 192, 4),
            CellSummary::solid(4),
            CellSummary::new(7, 64, 2),
            CellSummary::EMPTY,
            CellSummary::solid(9),
            CellSummary::new(4, 128, 1),
            CellSummary::EMPTY,
        ];
        let reduction_started = Instant::now();
        let mut reduction_checksum = 0_u64;
        for _ in 0..REDUCE_ITERATIONS {
            let reduced = black_box(reduce_eight_summaries(black_box(&inputs)).unwrap());
            reduction_checksum = reduction_checksum.wrapping_add(u64::from(reduced.material));
        }
        let reduction_elapsed = reduction_started.elapsed();

        let mut worst_case = [CellSummary::EMPTY; BRICK_CELL_COUNT];
        for (index, sample) in worst_case.iter_mut().enumerate() {
            *sample = CellSummary::new(index as MaterialId + 1, u8::MAX, 0);
        }
        const BRICK_REDUCE_ITERATIONS: usize = 2_000;
        let brick_reduction_started = Instant::now();
        let mut brick_checksum = 0_u64;
        for _ in 0..BRICK_REDUCE_ITERATIONS {
            let reduced = black_box(reduce_brick_summaries(black_box(&worst_case)).unwrap());
            brick_checksum = brick_checksum.wrapping_add(u64::from(reduced.material));
        }
        let brick_reduction_elapsed = brick_reduction_started.elapsed();

        let sample_ns = sampling_elapsed.as_nanos() as f64 / SAMPLE_ITERATIONS as f64;
        let reduce_ns = reduction_elapsed.as_nanos() as f64 / REDUCE_ITERATIONS as f64;
        let reduce_512_ns =
            brick_reduction_elapsed.as_nanos() as f64 / BRICK_REDUCE_ITERATIONS as f64;
        eprintln!(
            "VVH_BENCH payload_bytes={} brick_struct_bytes={} slot_bytes={} lookup_entry_bytes={} hard_512_bytes={} active_task_bytes={} hot_sample_ns={sample_ns:.2} reduce8_ns={reduce_ns:.2} reduce512_worst_ns={reduce_512_ns:.2}",
            BRICK_PAYLOAD_BYTES,
            size_of::<SummaryBrick>(),
            size_of::<Option<ResidentSlot>>(),
            size_of::<(BrickKey, usize)>(),
            MIDFIELD_RESIDENT_BYTE_LIMIT,
            ACTIVE_GENERATION_TASK_BYTES,
        );
        assert_eq!(sample_checksum, SAMPLE_ITERATIONS as u64 * 6);
        assert_eq!(reduction_checksum, REDUCE_ITERATIONS as u64 * 4);
        assert_eq!(brick_checksum, BRICK_REDUCE_ITERATIONS as u64);
    }
}
