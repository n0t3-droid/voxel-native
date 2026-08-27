//! Fixed-budget, read-only bridge between macro morphogenesis, hierarchy
//! observations, and explicitly identified implicit volumes.
//!
//! This module is deliberately not terrain, edit, physics, renderer, or save
//! authority.  It keeps a small reconstructible pyramid of descriptive macro
//! summaries and correlates other representations without blending their
//! semantics.  In particular, hierarchy occupancy/material remains a
//! [`CellSummary`], while an implicit classification remains a
//! [`CellClassification`].

use std::fmt;
use std::mem::size_of;

use crate::continuum_morphogenesis::{
    ContinuumGenerator, ContinuumTile, GenerationError, MacroTileCoord, MorphogenesisDomain,
    MorphogenesisProfile, SpeciesGuild, FAR_LOD_STRIDE, MACRO_TILE_CELLS, MACRO_TILE_SIDE,
    MID_LOD_STRIDE, MORPHOGENESIS_GRAMMAR_VERSION, NEAR_LOD_STRIDE, OUTPUT_ACCOUNTED_BYTES,
};
use crate::implicit_voxels::{
    Aabb3d, AxisAlignedEllipsoid, CellClassification, ConservativeImplicitVolume,
    OrientedEllipsoid, SphereVolume, ORIENTED_ELLIPSOID_INTERVAL_AXES_PER_CLASSIFICATION,
    ORIENTED_ELLIPSOID_VERTEX_SAMPLES_PER_CLASSIFICATION,
};
use crate::virtual_voxel_hierarchy::{BrickStamp, CellSummary, WorldVoxel, MAX_LOD};

pub const WORLD_CONTINUUM_SCHEMA_VERSION: u32 = 1;
pub const CONTINUUM_SLOT_COUNT: usize = 8;
pub const SURFACE_FAMILY_COUNT: usize = 5;
pub const SPECIES_GUILD_COUNT: usize = 9;

pub const NEAR_SUMMARY_SIDE: usize = MACRO_TILE_SIDE / NEAR_LOD_STRIDE;
pub const MID_SUMMARY_SIDE: usize = MACRO_TILE_SIDE / MID_LOD_STRIDE;
pub const FAR_SUMMARY_SIDE: usize = MACRO_TILE_SIDE / FAR_LOD_STRIDE;
pub const NEAR_SUMMARY_COUNT: usize = NEAR_SUMMARY_SIDE * NEAR_SUMMARY_SIDE;
pub const MID_SUMMARY_COUNT: usize = MID_SUMMARY_SIDE * MID_SUMMARY_SIDE;
pub const FAR_SUMMARY_COUNT: usize = FAR_SUMMARY_SIDE * FAR_SUMMARY_SIDE;
pub const SUMMARIES_PER_SLOT: usize = NEAR_SUMMARY_COUNT + MID_SUMMARY_COUNT + FAR_SUMMARY_COUNT;

/// One unit is one source cell classification or one shared height vertex.
pub const FAR_REDUCTION_WORK_UNITS: usize = FAR_SUMMARY_COUNT
    * (FAR_LOD_STRIDE * FAR_LOD_STRIDE + (FAR_LOD_STRIDE + 1) * (FAR_LOD_STRIDE + 1));
pub const MID_REDUCTION_WORK_UNITS: usize = MID_SUMMARY_COUNT
    * (MID_LOD_STRIDE * MID_LOD_STRIDE + (MID_LOD_STRIDE + 1) * (MID_LOD_STRIDE + 1));
pub const NEAR_REDUCTION_WORK_UNITS: usize = NEAR_SUMMARY_COUNT
    * (NEAR_LOD_STRIDE * NEAR_LOD_STRIDE + (NEAR_LOD_STRIDE + 1) * (NEAR_LOD_STRIDE + 1));
pub const MAX_REDUCTION_WORK_UNITS_PER_CALL: usize = NEAR_REDUCTION_WORK_UNITS;
pub const MAX_SLOT_PROBES_PER_SAMPLE: usize = 1;
pub const MAX_IMPLICIT_CLASSIFICATIONS_PER_CORRELATION: usize = 1;
pub const MAX_IMPLICIT_VERTEX_SAMPLES_PER_CORRELATION: usize =
    ORIENTED_ELLIPSOID_VERTEX_SAMPLES_PER_CLASSIFICATION;
pub const MAX_IMPLICIT_INTERVAL_AXES_PER_CORRELATION: usize =
    ORIENTED_ELLIPSOID_INTERVAL_AXES_PER_CLASSIFICATION;
/// One finite-value pass plus one deterministic fingerprint pass.
pub const MAX_TICKET_VALIDATION_BYTES: usize = OUTPUT_ACCOUNTED_BYTES * 2;

pub const MAX_WORLD_CONTINUUM_BYTES: usize = 512 * 1024;

const READY_FAR: u8 = 1 << 0;
const READY_MID: u8 = 1 << 1;
const READY_NEAR: u8 = 1 << 2;

/// Stable identity shared by every representation admitted through the bridge.
///
/// `voxels_per_macro_cell` is explicit because the hierarchy module does not
/// currently declare that one voxel equals one metre.  No hidden unit
/// conversion or floating render position participates in identity.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorldContinuumIdentity {
    schema_version: u32,
    grammar_version: u32,
    world_id: u64,
    seed: u64,
    profile: MorphogenesisProfile,
    world_epoch: u64,
    source_revision: u64,
    voxels_per_macro_cell: u32,
}

impl WorldContinuumIdentity {
    pub fn new(
        world_id: u64,
        seed: u64,
        profile: MorphogenesisProfile,
        world_epoch: u64,
        source_revision: u64,
        voxels_per_macro_cell: u32,
    ) -> Result<Self, ContinuumError> {
        Self::versioned(
            WORLD_CONTINUUM_SCHEMA_VERSION,
            MORPHOGENESIS_GRAMMAR_VERSION,
            world_id,
            seed,
            profile,
            world_epoch,
            source_revision,
            voxels_per_macro_cell,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn versioned(
        schema_version: u32,
        grammar_version: u32,
        world_id: u64,
        seed: u64,
        profile: MorphogenesisProfile,
        world_epoch: u64,
        source_revision: u64,
        voxels_per_macro_cell: u32,
    ) -> Result<Self, ContinuumError> {
        let identity = Self {
            schema_version,
            grammar_version,
            world_id,
            seed,
            profile,
            world_epoch,
            source_revision,
            voxels_per_macro_cell,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub const fn schema_version(self) -> u32 {
        self.schema_version
    }

    pub const fn grammar_version(self) -> u32 {
        self.grammar_version
    }

    pub const fn world_id(self) -> u64 {
        self.world_id
    }

    pub const fn seed(self) -> u64 {
        self.seed
    }

    pub const fn profile(self) -> MorphogenesisProfile {
        self.profile
    }

    pub const fn world_epoch(self) -> u64 {
        self.world_epoch
    }

    pub const fn source_revision(self) -> u64 {
        self.source_revision
    }

    pub const fn voxels_per_macro_cell(self) -> u32 {
        self.voxels_per_macro_cell
    }

    pub fn with_epoch_and_revision(
        self,
        world_epoch: u64,
        source_revision: u64,
    ) -> Result<Self, ContinuumError> {
        Self::versioned(
            self.schema_version,
            self.grammar_version,
            self.world_id,
            self.seed,
            self.profile,
            world_epoch,
            source_revision,
            self.voxels_per_macro_cell,
        )
    }

    fn validate(self) -> Result<(), ContinuumError> {
        if self.schema_version != WORLD_CONTINUUM_SCHEMA_VERSION {
            return Err(ContinuumError::UnsupportedSchemaVersion {
                requested: self.schema_version,
                supported: WORLD_CONTINUUM_SCHEMA_VERSION,
            });
        }
        if self.grammar_version != MORPHOGENESIS_GRAMMAR_VERSION {
            return Err(ContinuumError::UnsupportedGrammarVersion {
                requested: self.grammar_version,
                supported: MORPHOGENESIS_GRAMMAR_VERSION,
            });
        }
        if self.voxels_per_macro_cell == 0 {
            return Err(ContinuumError::InvalidVoxelScale(0));
        }
        Ok(())
    }

    fn stable_world_matches(self, other: Self) -> bool {
        self.schema_version == other.schema_version
            && self.grammar_version == other.grammar_version
            && self.world_id == other.world_id
            && self.seed == other.seed
            && self.profile == other.profile
            && self.voxels_per_macro_cell == other.voxels_per_macro_cell
    }

    fn fingerprint(self) -> u64 {
        let mut hash = mix64(self.world_id ^ self.seed.rotate_left(17));
        for word in [
            u64::from(self.schema_version),
            u64::from(self.grammar_version),
            self.world_id,
            self.seed,
            self.profile as u64,
            self.world_epoch,
            self.source_revision,
            u64::from(self.voxels_per_macro_cell),
        ] {
            hash = mix64(hash ^ word);
        }
        hash
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContinuumBand {
    Near = 0,
    Mid = 1,
    Far = 2,
}

impl ContinuumBand {
    pub const fn stride(self) -> usize {
        match self {
            Self::Near => NEAR_LOD_STRIDE,
            Self::Mid => MID_LOD_STRIDE,
            Self::Far => FAR_LOD_STRIDE,
        }
    }
}

/// A descriptive surface family, not an authoritative voxel material id.
///
/// Counts are conservative only with respect to the versioned classifier in
/// this adapter.  Actual hierarchy material remains separate.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DescriptiveSurfaceFamily {
    ExposedSubstrate = 0,
    Regolith = 1,
    WetSediment = 2,
    BiogenicTopsoil = 3,
    AstralCrystal = 4,
}

impl DescriptiveSurfaceFamily {
    const fn from_index(index: usize) -> Self {
        match index {
            0 => Self::ExposedSubstrate,
            1 => Self::Regolith,
            2 => Self::WetSediment,
            3 => Self::BiogenicTopsoil,
            4 => Self::AstralCrystal,
            _ => unreachable!(),
        }
    }
}

/// Fixed-width conservative reduction of one macro region.
///
/// The height envelope includes every shared vertex and every source cell
/// height in the region.  Histograms retain every classified source cell, so
/// no majority vote can erase a minority family or guild.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConservativeMacroSummary {
    pub elevation_min_m: f32,
    pub elevation_mean_m: f32,
    pub elevation_max_m: f32,
    surface_family_counts: [u16; SURFACE_FAMILY_COUNT],
    guild_counts: [u16; SPECIES_GUILD_COUNT],
    source_cell_count: u16,
    reserved: u16,
}

impl ConservativeMacroSummary {
    pub const EMPTY: Self = Self {
        elevation_min_m: 0.0,
        elevation_mean_m: 0.0,
        elevation_max_m: 0.0,
        surface_family_counts: [0; SURFACE_FAMILY_COUNT],
        guild_counts: [0; SPECIES_GUILD_COUNT],
        source_cell_count: 0,
        reserved: 0,
    };

    pub const fn surface_family_counts(&self) -> &[u16; SURFACE_FAMILY_COUNT] {
        &self.surface_family_counts
    }

    pub const fn guild_counts(&self) -> &[u16; SPECIES_GUILD_COUNT] {
        &self.guild_counts
    }

    pub const fn source_cell_count(self) -> u16 {
        self.source_cell_count
    }

    pub fn surface_family_mask(self) -> u8 {
        self.surface_family_counts
            .iter()
            .enumerate()
            .fold(0_u8, |mask, (index, &count)| {
                if count == 0 {
                    mask
                } else {
                    mask | (1_u8 << index)
                }
            })
    }

    pub fn guild_mask(self) -> u16 {
        self.guild_counts
            .iter()
            .enumerate()
            .fold(0_u16, |mask, (index, &count)| {
                if count == 0 {
                    mask
                } else {
                    mask | (1_u16 << index)
                }
            })
    }

    pub fn dominant_surface_family(self) -> Option<DescriptiveSurfaceFamily> {
        dominant_index(&self.surface_family_counts).map(DescriptiveSurfaceFamily::from_index)
    }

    pub fn dominant_guild(self) -> Option<SpeciesGuild> {
        dominant_index(&self.guild_counts).map(guild_from_index)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MacroCellAddress {
    pub tile: MacroTileCoord,
    local_x: u8,
    local_z: u8,
}

impl MacroCellAddress {
    pub fn new(
        tile: MacroTileCoord,
        local_x: usize,
        local_z: usize,
    ) -> Result<Self, ContinuumError> {
        if local_x >= MACRO_TILE_SIDE || local_z >= MACRO_TILE_SIDE {
            return Err(ContinuumError::LocalMacroCellOutOfRange {
                x: local_x,
                z: local_z,
            });
        }
        Ok(Self {
            tile,
            local_x: local_x as u8,
            local_z: local_z as u8,
        })
    }

    /// Integer-only mapping over the complete `i64` tile-coordinate domain.
    pub fn from_global_macro_cell(global_x: i128, global_z: i128) -> Result<Self, ContinuumError> {
        let side = MACRO_TILE_SIDE as i128;
        let tile_x = global_x.div_euclid(side);
        let tile_z = global_z.div_euclid(side);
        let tile_x =
            i64::try_from(tile_x).map_err(|_| ContinuumError::MacroCoordinateOutOfRange(tile_x))?;
        let tile_z =
            i64::try_from(tile_z).map_err(|_| ContinuumError::MacroCoordinateOutOfRange(tile_z))?;
        let local_x = usize::try_from(global_x.rem_euclid(side))
            .map_err(|_| ContinuumError::MacroCoordinateOutOfRange(global_x))?;
        let local_z = usize::try_from(global_z.rem_euclid(side))
            .map_err(|_| ContinuumError::MacroCoordinateOutOfRange(global_z))?;
        Self::new(MacroTileCoord::new(tile_x, tile_z), local_x, local_z)
    }

    pub const fn local_x(self) -> u8 {
        self.local_x
    }

    pub const fn local_z(self) -> u8 {
        self.local_z
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContinuumSample {
    pub address: MacroCellAddress,
    pub requested_band: ContinuumBand,
    pub served_band: ContinuumBand,
    pub tile_fingerprint: u64,
    pub summary: ConservativeMacroSummary,
}

impl ContinuumSample {
    pub const fn used_fallback(self) -> bool {
        self.requested_band as u8 != self.served_band as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacroTileTicket {
    identity: WorldContinuumIdentity,
    coord: MacroTileCoord,
}

impl MacroTileTicket {
    pub const fn identity(self) -> WorldContinuumIdentity {
        self.identity
    }

    pub const fn coord(self) -> MacroTileCoord {
        self.coord
    }

    /// Stateless generation.  The caller cannot provide a second seed.
    pub fn generate(self) -> Result<GeneratedMacroTile, ContinuumError> {
        let generator = ContinuumGenerator;
        let tile = generator
            .generate_versioned(
                self.identity.seed,
                self.coord,
                self.identity.profile,
                self.identity.grammar_version,
            )
            .map_err(ContinuumError::Generation)?;
        if !tile.all_scalars_are_finite() {
            return Err(ContinuumError::NonFiniteMacroTile(self.coord));
        }
        Ok(GeneratedMacroTile {
            identity: self.identity,
            fingerprint: tile.fingerprint(),
            tile,
        })
    }
}

/// Private-field integrity envelope produced only by [`MacroTileTicket`].
/// Finite-value and full fingerprint scans happen once on the worker result,
/// not again during every staged publication.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedMacroTile {
    identity: WorldContinuumIdentity,
    fingerprint: u64,
    tile: ContinuumTile,
}

impl GeneratedMacroTile {
    pub const fn identity(&self) -> WorldContinuumIdentity {
        self.identity
    }

    pub const fn coord(&self) -> MacroTileCoord {
        self.tile.coord
    }

    pub const fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    pub const fn tile(&self) -> &ContinuumTile {
        &self.tile
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed,
    AlreadyCurrent,
    Evicted(MacroTileCoord),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContinuumTelemetry {
    pub identity_transitions: u64,
    pub parent_installs: u64,
    pub mid_promotions: u64,
    pub near_promotions: u64,
    pub direct_slot_evictions: u64,
    pub sample_hits: u64,
    pub sample_misses: u64,
    pub fallback_samples: u64,
    pub stale_rejections: u64,
    pub hierarchy_correlations: u64,
    pub implicit_correlations: u64,
    pub reduction_work_units: u64,
    pub last_reduction_work_units: usize,
    pub max_reduction_work_units: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContinuumBudget {
    pub fixed_slots: usize,
    pub summaries_per_slot: usize,
    pub summary_bytes: usize,
    pub slot_bytes: usize,
    pub adapter_bytes: usize,
    pub adapter_byte_cap: usize,
    pub max_reduction_work_units_per_call: usize,
    pub max_slot_probes_per_sample: usize,
    pub max_ticket_validation_bytes: usize,
}

#[derive(Clone, Copy)]
struct ContinuumSlot {
    occupied: bool,
    coord: MacroTileCoord,
    identity_fingerprint: u64,
    tile_fingerprint: u64,
    ready: u8,
    far: [ConservativeMacroSummary; FAR_SUMMARY_COUNT],
    mid: [ConservativeMacroSummary; MID_SUMMARY_COUNT],
    near: [ConservativeMacroSummary; NEAR_SUMMARY_COUNT],
}

impl ContinuumSlot {
    const EMPTY: Self = Self {
        occupied: false,
        coord: MacroTileCoord::new(0, 0),
        identity_fingerprint: 0,
        tile_fingerprint: 0,
        ready: 0,
        far: [ConservativeMacroSummary::EMPTY; FAR_SUMMARY_COUNT],
        mid: [ConservativeMacroSummary::EMPTY; MID_SUMMARY_COUNT],
        near: [ConservativeMacroSummary::EMPTY; NEAR_SUMMARY_COUNT],
    };

    fn invalidated(&mut self) {
        self.occupied = false;
        self.ready = 0;
    }

    fn matches(&self, coord: MacroTileCoord, identity_fingerprint: u64) -> bool {
        self.occupied && self.coord == coord && self.identity_fingerprint == identity_fingerprint
    }

    fn has(&self, band: ContinuumBand) -> bool {
        let bit = match band {
            ContinuumBand::Near => READY_NEAR,
            ContinuumBand::Mid => READY_MID,
            ContinuumBand::Far => READY_FAR,
        };
        self.ready & bit != 0
    }

    fn best_available(&self, requested: ContinuumBand) -> Option<ContinuumBand> {
        match requested {
            ContinuumBand::Near if self.has(ContinuumBand::Near) => Some(ContinuumBand::Near),
            ContinuumBand::Near | ContinuumBand::Mid if self.has(ContinuumBand::Mid) => {
                Some(ContinuumBand::Mid)
            }
            _ if self.has(ContinuumBand::Far) => Some(ContinuumBand::Far),
            _ => None,
        }
    }

    fn summary(
        &self,
        band: ContinuumBand,
        local_x: usize,
        local_z: usize,
    ) -> ConservativeMacroSummary {
        match band {
            ContinuumBand::Near => self.near[local_z * NEAR_SUMMARY_SIDE + local_x],
            ContinuumBand::Mid => {
                let x = local_x / MID_LOD_STRIDE;
                let z = local_z / MID_LOD_STRIDE;
                self.mid[z * MID_SUMMARY_SIDE + x]
            }
            ContinuumBand::Far => {
                let x = local_x / FAR_LOD_STRIDE;
                let z = local_z / FAR_LOD_STRIDE;
                self.far[z * FAR_SUMMARY_SIDE + x]
            }
        }
    }
}

/// Fixed-slot reconstructible adapter.  It owns no live-world authority.
pub struct WorldContinuumAdapter {
    identity: WorldContinuumIdentity,
    slots: Box<[ContinuumSlot]>,
    telemetry: ContinuumTelemetry,
}

pub const WORLD_CONTINUUM_ACCOUNTED_BYTES: usize =
    size_of::<WorldContinuumAdapter>() + CONTINUUM_SLOT_COUNT * size_of::<ContinuumSlot>();

impl WorldContinuumAdapter {
    pub fn new(identity: WorldContinuumIdentity) -> Result<Self, ContinuumError> {
        identity.validate()?;
        let mut slots = Vec::with_capacity(CONTINUUM_SLOT_COUNT);
        for _ in 0..CONTINUUM_SLOT_COUNT {
            slots.push(ContinuumSlot::EMPTY);
        }
        Ok(Self {
            identity,
            slots: slots.into_boxed_slice(),
            telemetry: ContinuumTelemetry::default(),
        })
    }

    pub const fn identity(&self) -> WorldContinuumIdentity {
        self.identity
    }

    pub const fn telemetry(&self) -> ContinuumTelemetry {
        self.telemetry
    }

    pub const fn fixed_budget() -> ContinuumBudget {
        ContinuumBudget {
            fixed_slots: CONTINUUM_SLOT_COUNT,
            summaries_per_slot: SUMMARIES_PER_SLOT,
            summary_bytes: size_of::<ConservativeMacroSummary>(),
            slot_bytes: size_of::<ContinuumSlot>(),
            adapter_bytes: WORLD_CONTINUUM_ACCOUNTED_BYTES,
            adapter_byte_cap: MAX_WORLD_CONTINUUM_BYTES,
            max_reduction_work_units_per_call: MAX_REDUCTION_WORK_UNITS_PER_CALL,
            max_slot_probes_per_sample: MAX_SLOT_PROBES_PER_SAMPLE,
            max_ticket_validation_bytes: MAX_TICKET_VALIDATION_BYTES,
        }
    }

    /// Includes the fixed boxed slot payload; allocator metadata is platform
    /// specific and intentionally excluded.
    pub const fn accounted_bytes(&self) -> usize {
        WORLD_CONTINUUM_ACCOUNTED_BYTES
    }

    pub fn resident_slots(&self) -> usize {
        self.slots.iter().filter(|slot| slot.occupied).count()
    }

    pub fn ready_counts(&self) -> (usize, usize, usize) {
        let far = self
            .slots
            .iter()
            .filter(|slot| slot.occupied && slot.has(ContinuumBand::Far))
            .count();
        let mid = self
            .slots
            .iter()
            .filter(|slot| slot.occupied && slot.has(ContinuumBand::Mid))
            .count();
        let near = self
            .slots
            .iter()
            .filter(|slot| slot.occupied && slot.has(ContinuumBand::Near))
            .count();
        (far, mid, near)
    }

    /// Changes only cache identity.  Existing payload arrays are logically
    /// invalidated in O(fixed slots), never migrated into new authority.
    pub fn advance_identity(
        &mut self,
        next: WorldContinuumIdentity,
    ) -> Result<bool, ContinuumError> {
        next.validate()?;
        if next.world_epoch < self.identity.world_epoch {
            self.record_stale_rejection();
            return Err(ContinuumError::StaleEpoch {
                expected: self.identity.world_epoch,
                found: next.world_epoch,
            });
        }
        if next.world_epoch == self.identity.world_epoch {
            if !self.identity.stable_world_matches(next) {
                return Err(ContinuumError::IdentityConflict);
            }
            if next.source_revision < self.identity.source_revision {
                self.record_stale_rejection();
                return Err(ContinuumError::StaleRevision {
                    expected: self.identity.source_revision,
                    found: next.source_revision,
                });
            }
            if next.source_revision == self.identity.source_revision {
                return Ok(false);
            }
        }

        self.identity = next;
        for slot in &mut self.slots {
            slot.invalidated();
        }
        self.telemetry.identity_transitions = self.telemetry.identity_transitions.saturating_add(1);
        Ok(true)
    }

    pub fn ticket(
        &self,
        expected_identity: WorldContinuumIdentity,
        coord: MacroTileCoord,
    ) -> Result<MacroTileTicket, ContinuumError> {
        self.validate_expected(expected_identity)?;
        Ok(MacroTileTicket {
            identity: expected_identity,
            coord,
        })
    }

    /// Publishes the Far parent first.  A direct-mapped collision evicts one
    /// whole reconstructible tile; it never leaves a child without its parent.
    pub fn install_parent(
        &mut self,
        generated: &GeneratedMacroTile,
    ) -> Result<InstallOutcome, ContinuumError> {
        self.validate_generated(generated)?;
        let identity_fingerprint = self.identity.fingerprint();
        let index = slot_index(identity_fingerprint, generated.coord());
        let current = &self.slots[index];
        if current.matches(generated.coord(), identity_fingerprint) {
            if current.tile_fingerprint != generated.fingerprint {
                return Err(ContinuumError::TileFingerprintConflict(generated.coord()));
            }
            if current.has(ContinuumBand::Far) {
                return Ok(InstallOutcome::AlreadyCurrent);
            }
        }

        let (far, work) =
            build_level::<FAR_LOD_STRIDE, FAR_SUMMARY_SIDE, FAR_SUMMARY_COUNT>(&generated.tile);
        debug_assert_eq!(work, FAR_REDUCTION_WORK_UNITS);

        let evicted = if current.occupied {
            self.telemetry.direct_slot_evictions =
                self.telemetry.direct_slot_evictions.saturating_add(1);
            Some(current.coord)
        } else {
            None
        };
        let slot = &mut self.slots[index];
        slot.occupied = true;
        slot.coord = generated.coord();
        slot.identity_fingerprint = identity_fingerprint;
        slot.tile_fingerprint = generated.fingerprint;
        slot.ready = READY_FAR;
        slot.far = far;

        self.telemetry.parent_installs = self.telemetry.parent_installs.saturating_add(1);
        self.record_reduction_work(work);
        Ok(evicted.map_or(InstallOutcome::Installed, InstallOutcome::Evicted))
    }

    pub fn promote_mid(
        &mut self,
        generated: &GeneratedMacroTile,
    ) -> Result<InstallOutcome, ContinuumError> {
        self.validate_generated(generated)?;
        let identity_fingerprint = self.identity.fingerprint();
        let index = slot_index(identity_fingerprint, generated.coord());
        let current = &self.slots[index];
        self.validate_parent(current, generated, identity_fingerprint)?;
        if current.has(ContinuumBand::Mid) {
            return Ok(InstallOutcome::AlreadyCurrent);
        }
        let (mid, work) =
            build_level::<MID_LOD_STRIDE, MID_SUMMARY_SIDE, MID_SUMMARY_COUNT>(&generated.tile);
        debug_assert_eq!(work, MID_REDUCTION_WORK_UNITS);
        let slot = &mut self.slots[index];
        slot.mid = mid;
        slot.ready |= READY_MID;
        self.telemetry.mid_promotions = self.telemetry.mid_promotions.saturating_add(1);
        self.record_reduction_work(work);
        Ok(InstallOutcome::Installed)
    }

    pub fn promote_near(
        &mut self,
        generated: &GeneratedMacroTile,
    ) -> Result<InstallOutcome, ContinuumError> {
        self.validate_generated(generated)?;
        let identity_fingerprint = self.identity.fingerprint();
        let index = slot_index(identity_fingerprint, generated.coord());
        let current = &self.slots[index];
        self.validate_parent(current, generated, identity_fingerprint)?;
        if !current.has(ContinuumBand::Mid) {
            return Err(ContinuumError::MissingMidParent(generated.coord()));
        }
        if current.has(ContinuumBand::Near) {
            return Ok(InstallOutcome::AlreadyCurrent);
        }
        let (near, work) =
            build_level::<NEAR_LOD_STRIDE, NEAR_SUMMARY_SIDE, NEAR_SUMMARY_COUNT>(&generated.tile);
        debug_assert_eq!(work, NEAR_REDUCTION_WORK_UNITS);
        let slot = &mut self.slots[index];
        slot.near = near;
        slot.ready |= READY_NEAR;
        self.telemetry.near_promotions = self.telemetry.near_promotions.saturating_add(1);
        self.record_reduction_work(work);
        Ok(InstallOutcome::Installed)
    }

    pub fn sample_cell(
        &mut self,
        expected_identity: WorldContinuumIdentity,
        address: MacroCellAddress,
        requested_band: ContinuumBand,
    ) -> Result<ContinuumSample, ContinuumError> {
        self.validate_expected_tracked(expected_identity)?;
        let identity_fingerprint = self.identity.fingerprint();
        let index = slot_index(identity_fingerprint, address.tile);
        let slot = &self.slots[index];
        if !slot.matches(address.tile, identity_fingerprint) {
            self.telemetry.sample_misses = self.telemetry.sample_misses.saturating_add(1);
            return Err(ContinuumError::MissingFarParent(address.tile));
        }
        let Some(served_band) = slot.best_available(requested_band) else {
            self.telemetry.sample_misses = self.telemetry.sample_misses.saturating_add(1);
            return Err(ContinuumError::MissingFarParent(address.tile));
        };
        let local_x = usize::from(address.local_x);
        let local_z = usize::from(address.local_z);
        let summary = slot.summary(served_band, local_x, local_z);
        self.telemetry.sample_hits = self.telemetry.sample_hits.saturating_add(1);
        if served_band != requested_band {
            self.telemetry.fallback_samples = self.telemetry.fallback_samples.saturating_add(1);
        }
        Ok(ContinuumSample {
            address,
            requested_band,
            served_band,
            tile_fingerprint: slot.tile_fingerprint,
            summary,
        })
    }

    pub fn address_for_world_voxel(
        &self,
        position: WorldVoxel,
    ) -> Result<MacroCellAddress, ContinuumError> {
        let scale = i64::from(self.identity.voxels_per_macro_cell);
        let global_x = i128::from(position.x.div_euclid(scale));
        let global_z = i128::from(position.z.div_euclid(scale));
        MacroCellAddress::from_global_macro_cell(global_x, global_z)
    }

    /// Correlates an already-produced hierarchy observation without calling
    /// hierarchy reduction, installation, edits, or residency APIs.
    pub fn correlate_hierarchy(
        &mut self,
        expected_identity: WorldContinuumIdentity,
        expected_stamp: BrickStamp,
        observation: HierarchyObservation,
        requested_band: ContinuumBand,
    ) -> Result<HierarchyCorrelation, ContinuumError> {
        self.validate_expected_tracked(expected_identity)?;
        if let Err(error) = validate_hierarchy_stamp(self.identity, expected_stamp) {
            self.record_stale_rejection();
            return Err(error);
        }
        if observation.stamp != expected_stamp {
            self.record_stale_rejection();
            return Err(ContinuumError::HierarchyStampMismatch {
                expected: expected_stamp,
                found: observation.stamp,
            });
        }
        if observation.configured_max_lod > MAX_LOD
            || observation.lod > observation.configured_max_lod
        {
            return Err(ContinuumError::HierarchyLodOutOfRange {
                requested: observation.lod,
                configured: observation.configured_max_lod,
            });
        }
        let address = self.address_for_world_voxel(observation.position)?;
        let macro_sample = self.sample_cell(expected_identity, address, requested_band)?;
        self.telemetry.hierarchy_correlations =
            self.telemetry.hierarchy_correlations.saturating_add(1);
        Ok(HierarchyCorrelation {
            macro_sample,
            observation,
        })
    }

    /// Classifies one explicit, versioned feature and returns it beside the
    /// macro sample.  It never synthesizes an implicit shape from macro data.
    #[allow(clippy::too_many_arguments)]
    pub fn correlate_implicit(
        &mut self,
        expected_identity: WorldContinuumIdentity,
        expected_feature: ImplicitFeatureStamp,
        observed_feature: ImplicitFeatureStamp,
        anchor: WorldVoxel,
        bounds: Aabb3d,
        volume: BoundedImplicitVolumeRef<'_>,
        requested_band: ContinuumBand,
    ) -> Result<ImplicitCorrelation, ContinuumError> {
        self.validate_expected_tracked(expected_identity)?;
        if let Err(error) = validate_feature_stamp(self.identity, expected_feature) {
            if matches!(
                error,
                ContinuumError::StaleEpoch { .. } | ContinuumError::StaleRevision { .. }
            ) {
                self.record_stale_rejection();
            }
            return Err(error);
        }
        if observed_feature != expected_feature {
            self.record_stale_rejection();
            return Err(ContinuumError::ImplicitFeatureStampMismatch {
                expected: expected_feature,
                found: observed_feature,
            });
        }
        let address = self.address_for_world_voxel(anchor)?;
        let macro_sample = self.sample_cell(expected_identity, address, requested_band)?;
        let classification = volume.classify_aabb(&bounds);
        self.telemetry.implicit_correlations =
            self.telemetry.implicit_correlations.saturating_add(1);
        Ok(ImplicitCorrelation {
            macro_sample,
            feature: observed_feature,
            implicit_kind: volume.kind(),
            classification,
        })
    }

    fn validate_expected(&self, found: WorldContinuumIdentity) -> Result<(), ContinuumError> {
        found.validate()?;
        if found.world_epoch != self.identity.world_epoch {
            return Err(ContinuumError::StaleEpoch {
                expected: self.identity.world_epoch,
                found: found.world_epoch,
            });
        }
        if !self.identity.stable_world_matches(found) {
            return Err(ContinuumError::IdentityMismatch);
        }
        if found.source_revision != self.identity.source_revision {
            return Err(ContinuumError::StaleRevision {
                expected: self.identity.source_revision,
                found: found.source_revision,
            });
        }
        Ok(())
    }

    fn validate_expected_tracked(
        &mut self,
        found: WorldContinuumIdentity,
    ) -> Result<(), ContinuumError> {
        let result = self.validate_expected(found);
        if matches!(
            &result,
            Err(ContinuumError::StaleEpoch { .. }) | Err(ContinuumError::StaleRevision { .. })
        ) {
            self.record_stale_rejection();
        }
        result
    }

    fn validate_generated(&mut self, generated: &GeneratedMacroTile) -> Result<(), ContinuumError> {
        if let Err(error) = self.validate_expected(generated.identity) {
            if matches!(
                error,
                ContinuumError::StaleEpoch { .. } | ContinuumError::StaleRevision { .. }
            ) {
                self.record_stale_rejection();
            }
            return Err(error);
        }
        let tile = &generated.tile;
        if tile.grammar_version != generated.identity.grammar_version
            || tile.seed != generated.identity.seed
            || tile.profile != generated.identity.profile
        {
            return Err(ContinuumError::GeneratedTileIdentityMismatch(tile.coord));
        }
        Ok(())
    }

    fn validate_parent(
        &self,
        slot: &ContinuumSlot,
        generated: &GeneratedMacroTile,
        identity_fingerprint: u64,
    ) -> Result<(), ContinuumError> {
        if !slot.matches(generated.coord(), identity_fingerprint) || !slot.has(ContinuumBand::Far) {
            return Err(ContinuumError::MissingFarParent(generated.coord()));
        }
        if slot.tile_fingerprint != generated.fingerprint {
            return Err(ContinuumError::TileFingerprintConflict(generated.coord()));
        }
        Ok(())
    }

    fn record_reduction_work(&mut self, work: usize) {
        self.telemetry.last_reduction_work_units = work;
        self.telemetry.max_reduction_work_units = self.telemetry.max_reduction_work_units.max(work);
        self.telemetry.reduction_work_units = self
            .telemetry
            .reduction_work_units
            .saturating_add(work as u64);
    }

    fn record_stale_rejection(&mut self) {
        self.telemetry.stale_rejections = self.telemetry.stale_rejections.saturating_add(1);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HierarchyObservation {
    pub position: WorldVoxel,
    pub lod: u8,
    /// Copy of `VirtualVoxelHierarchy::max_lod()` at observation time.
    pub configured_max_lod: u8,
    pub stamp: BrickStamp,
    pub summary: CellSummary,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HierarchyCorrelation {
    pub macro_sample: ContinuumSample,
    pub observation: HierarchyObservation,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImplicitFeatureStamp {
    pub world_epoch: u64,
    pub source_revision: u64,
    pub feature_id: u64,
    pub feature_revision: u64,
}

#[derive(Clone, Copy, Debug)]
pub enum BoundedImplicitVolumeRef<'a> {
    Sphere(&'a SphereVolume),
    AxisAlignedEllipsoid(&'a AxisAlignedEllipsoid),
    OrientedEllipsoid(&'a OrientedEllipsoid),
}

impl BoundedImplicitVolumeRef<'_> {
    fn classify_aabb(self, bounds: &Aabb3d) -> CellClassification {
        match self {
            Self::Sphere(volume) => volume.classify_aabb(bounds),
            Self::AxisAlignedEllipsoid(volume) => volume.classify_aabb(bounds),
            Self::OrientedEllipsoid(volume) => volume.classify_aabb(bounds),
        }
    }

    const fn kind(self) -> ImplicitKind {
        match self {
            Self::Sphere(_) => ImplicitKind::Sphere,
            Self::AxisAlignedEllipsoid(_) => ImplicitKind::AxisAlignedEllipsoid,
            Self::OrientedEllipsoid(_) => ImplicitKind::OrientedEllipsoid,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImplicitKind {
    Sphere = 0,
    AxisAlignedEllipsoid = 1,
    OrientedEllipsoid = 2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImplicitCorrelation {
    pub macro_sample: ContinuumSample,
    pub feature: ImplicitFeatureStamp,
    pub implicit_kind: ImplicitKind,
    pub classification: CellClassification,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContinuumError {
    UnsupportedSchemaVersion {
        requested: u32,
        supported: u32,
    },
    UnsupportedGrammarVersion {
        requested: u32,
        supported: u32,
    },
    InvalidVoxelScale(u32),
    StaleEpoch {
        expected: u64,
        found: u64,
    },
    StaleRevision {
        expected: u64,
        found: u64,
    },
    IdentityConflict,
    IdentityMismatch,
    Generation(GenerationError),
    GeneratedTileIdentityMismatch(MacroTileCoord),
    NonFiniteMacroTile(MacroTileCoord),
    TileFingerprintConflict(MacroTileCoord),
    MissingFarParent(MacroTileCoord),
    MissingMidParent(MacroTileCoord),
    LocalMacroCellOutOfRange {
        x: usize,
        z: usize,
    },
    MacroCoordinateOutOfRange(i128),
    HierarchyLodOutOfRange {
        requested: u8,
        configured: u8,
    },
    HierarchyStampMismatch {
        expected: BrickStamp,
        found: BrickStamp,
    },
    InvalidImplicitFeatureId,
    ImplicitFeatureStampMismatch {
        expected: ImplicitFeatureStamp,
        found: ImplicitFeatureStamp,
    },
}

impl fmt::Display for ContinuumError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion {
                requested,
                supported,
            } => write!(
                formatter,
                "world-continuum schema {requested} is unsupported; expected {supported}"
            ),
            Self::UnsupportedGrammarVersion {
                requested,
                supported,
            } => write!(
                formatter,
                "morphogenesis grammar {requested} is unsupported; expected {supported}"
            ),
            Self::InvalidVoxelScale(scale) => {
                write!(
                    formatter,
                    "voxels-per-macro-cell must be non-zero, found {scale}"
                )
            }
            Self::StaleEpoch { expected, found } => {
                write!(formatter, "stale world epoch {found}; expected {expected}")
            }
            Self::StaleRevision { expected, found } => {
                write!(
                    formatter,
                    "stale source revision {found}; expected {expected}"
                )
            }
            Self::IdentityConflict => write!(
                formatter,
                "world identity changed without advancing the world epoch"
            ),
            Self::IdentityMismatch => write!(formatter, "world identity does not match adapter"),
            Self::Generation(error) => write!(formatter, "macro generation failed: {error:?}"),
            Self::GeneratedTileIdentityMismatch(coord) => {
                write!(
                    formatter,
                    "generated macro tile {coord:?} failed identity validation"
                )
            }
            Self::NonFiniteMacroTile(coord) => {
                write!(formatter, "macro tile {coord:?} contains non-finite data")
            }
            Self::TileFingerprintConflict(coord) => write!(
                formatter,
                "macro tile {coord:?} changed under one immutable identity"
            ),
            Self::MissingFarParent(coord) => {
                write!(formatter, "macro tile {coord:?} has no resident Far parent")
            }
            Self::MissingMidParent(coord) => {
                write!(formatter, "macro tile {coord:?} has no resident Mid parent")
            }
            Self::LocalMacroCellOutOfRange { x, z } => write!(
                formatter,
                "local macro cell ({x}, {z}) is outside {MACRO_TILE_SIDE}x{MACRO_TILE_SIDE}"
            ),
            Self::MacroCoordinateOutOfRange(value) => {
                write!(
                    formatter,
                    "macro tile coordinate {value} is outside i64 range"
                )
            }
            Self::HierarchyLodOutOfRange {
                requested,
                configured,
            } => write!(
                formatter,
                "hierarchy LOD {requested} exceeds configured {configured} or hard {MAX_LOD}"
            ),
            Self::HierarchyStampMismatch { expected, found } => write!(
                formatter,
                "hierarchy observation stamp {found:?} does not match expected {expected:?}"
            ),
            Self::InvalidImplicitFeatureId => {
                write!(formatter, "implicit feature id zero is reserved as invalid")
            }
            Self::ImplicitFeatureStampMismatch { expected, found } => write!(
                formatter,
                "implicit feature stamp {found:?} does not match expected {expected:?}"
            ),
        }
    }
}

impl std::error::Error for ContinuumError {}

fn validate_hierarchy_stamp(
    identity: WorldContinuumIdentity,
    stamp: BrickStamp,
) -> Result<(), ContinuumError> {
    if stamp.epoch != identity.world_epoch {
        return Err(ContinuumError::StaleEpoch {
            expected: identity.world_epoch,
            found: stamp.epoch,
        });
    }
    if stamp.source_version != identity.source_revision {
        return Err(ContinuumError::StaleRevision {
            expected: identity.source_revision,
            found: stamp.source_version,
        });
    }
    Ok(())
}

fn validate_feature_stamp(
    identity: WorldContinuumIdentity,
    stamp: ImplicitFeatureStamp,
) -> Result<(), ContinuumError> {
    if stamp.feature_id == 0 {
        return Err(ContinuumError::InvalidImplicitFeatureId);
    }
    if stamp.world_epoch != identity.world_epoch {
        return Err(ContinuumError::StaleEpoch {
            expected: identity.world_epoch,
            found: stamp.world_epoch,
        });
    }
    if stamp.source_revision != identity.source_revision {
        return Err(ContinuumError::StaleRevision {
            expected: identity.source_revision,
            found: stamp.source_revision,
        });
    }
    Ok(())
}

fn build_level<const STRIDE: usize, const SIDE: usize, const COUNT: usize>(
    tile: &ContinuumTile,
) -> ([ConservativeMacroSummary; COUNT], usize) {
    debug_assert_eq!(SIDE * STRIDE, MACRO_TILE_SIDE);
    debug_assert_eq!(COUNT, SIDE * SIDE);
    let mut output = [ConservativeMacroSummary::EMPTY; COUNT];
    let mut work = 0_usize;
    for output_z in 0..SIDE {
        for output_x in 0..SIDE {
            output[output_z * SIDE + output_x] = reduce_region(
                tile,
                output_x * STRIDE,
                output_z * STRIDE,
                STRIDE,
                &mut work,
            );
        }
    }
    (output, work)
}

fn reduce_region(
    tile: &ContinuumTile,
    start_x: usize,
    start_z: usize,
    stride: usize,
    work: &mut usize,
) -> ConservativeMacroSummary {
    let mut minimum = f32::INFINITY;
    let mut maximum = f32::NEG_INFINITY;
    let mut mean_sum = 0.0_f64;
    let mut surface_family_counts = [0_u16; SURFACE_FAMILY_COUNT];
    let mut guild_counts = [0_u16; SPECIES_GUILD_COUNT];
    let mut source_cell_count = 0_u16;

    for z in start_z..start_z + stride {
        for x in start_x..start_x + stride {
            let index = z * MACRO_TILE_SIDE + x;
            let elevation = tile.visual.elevation_m[index];
            minimum = minimum.min(elevation);
            maximum = maximum.max(elevation);
            mean_sum += f64::from(elevation);
            let family = classify_surface_family(tile, index) as usize;
            surface_family_counts[family] = surface_family_counts[family].saturating_add(1);
            let guild = tile.visual.species_guild[index] as usize;
            guild_counts[guild] = guild_counts[guild].saturating_add(1);
            source_cell_count = source_cell_count.saturating_add(1);
            *work += 1;
        }
    }

    for z in start_z..=start_z + stride {
        for x in start_x..=start_x + stride {
            let index = z * (MACRO_TILE_SIDE + 1) + x;
            let elevation = tile.vertices.elevation_m[index];
            minimum = minimum.min(elevation);
            maximum = maximum.max(elevation);
            *work += 1;
        }
    }

    ConservativeMacroSummary {
        elevation_min_m: minimum,
        elevation_mean_m: (mean_sum / f64::from(source_cell_count)) as f32,
        elevation_max_m: maximum,
        surface_family_counts,
        guild_counts,
        source_cell_count,
        reserved: 0,
    }
}

fn classify_surface_family(tile: &ContinuumTile, index: usize) -> DescriptiveSurfaceFamily {
    if tile.profile.domain() == MorphogenesisDomain::Astral {
        return DescriptiveSurfaceFamily::AstralCrystal;
    }
    if tile.visual.routed_surface_water[index] >= 0.8 || tile.visual.moisture[index] >= 0.82 {
        return DescriptiveSurfaceFamily::WetSediment;
    }
    if tile.visual.soil_depth_m[index] <= 0.25 || tile.visual.slope_grade[index] >= 0.75 {
        return DescriptiveSurfaceFamily::ExposedSubstrate;
    }
    if tile.visual.vegetation_potential[index] >= 0.45
        && !matches!(
            tile.visual.species_guild[index],
            SpeciesGuild::Bare | SpeciesGuild::PioneerGrass | SpeciesGuild::Alpine
        )
    {
        return DescriptiveSurfaceFamily::BiogenicTopsoil;
    }
    DescriptiveSurfaceFamily::Regolith
}

fn dominant_index<const N: usize>(counts: &[u16; N]) -> Option<usize> {
    let mut best = None;
    for (index, &count) in counts.iter().enumerate() {
        if count == 0 {
            continue;
        }
        if best.is_none_or(|best_index| count > counts[best_index]) {
            best = Some(index);
        }
    }
    best
}

fn guild_from_index(index: usize) -> SpeciesGuild {
    match index {
        0 => SpeciesGuild::Bare,
        1 => SpeciesGuild::PioneerGrass,
        2 => SpeciesGuild::Shrubland,
        3 => SpeciesGuild::ClosedCanopy,
        4 => SpeciesGuild::Riparian,
        5 => SpeciesGuild::Alpine,
        6 => SpeciesGuild::XericScrub,
        7 => SpeciesGuild::CrystalPioneer,
        8 => SpeciesGuild::LuminousGrove,
        _ => unreachable!(),
    }
}

fn slot_index(identity_fingerprint: u64, coord: MacroTileCoord) -> usize {
    let mixed = mix64(
        identity_fingerprint ^ (coord.x as u64).rotate_left(19) ^ (coord.z as u64).rotate_left(43),
    );
    mixed as usize % CONTINUUM_SLOT_COUNT
}

fn mix64(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

const _: () = assert!(MACRO_TILE_CELLS == NEAR_SUMMARY_COUNT);
const _: () = assert!(SPECIES_GUILD_COUNT == SpeciesGuild::LuminousGrove as usize + 1);
const _: () = assert!(size_of::<ConservativeMacroSummary>() == 44);
const _: () = assert!(WORLD_CONTINUUM_ACCOUNTED_BYTES <= MAX_WORLD_CONTINUUM_BYTES);
const _: () = assert!(FAR_REDUCTION_WORK_UNITS <= MAX_REDUCTION_WORK_UNITS_PER_CALL);
const _: () = assert!(MID_REDUCTION_WORK_UNITS <= MAX_REDUCTION_WORK_UNITS_PER_CALL);

#[cfg(test)]
mod tests {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::implicit_voxels::Vec3d;

    const TEST_WORLD_ID: u64 = 0x574f_524c_445f_3031;
    const TEST_SEED: u64 = 0xa57a_1c0d_5eed_0021;

    fn identity(scale: u32) -> WorldContinuumIdentity {
        WorldContinuumIdentity::new(
            TEST_WORLD_ID,
            TEST_SEED,
            MorphogenesisProfile::AstralCrystalline,
            11,
            7,
            scale,
        )
        .unwrap()
    }

    fn generated(adapter: &WorldContinuumAdapter, coord: MacroTileCoord) -> GeneratedMacroTile {
        adapter
            .ticket(adapter.identity(), coord)
            .unwrap()
            .generate()
            .unwrap()
    }

    fn address(coord: MacroTileCoord, x: usize, z: usize) -> MacroCellAddress {
        MacroCellAddress::new(coord, x, z).unwrap()
    }

    #[test]
    fn layout_and_work_budgets_are_compile_time_fixed() {
        let budget = WorldContinuumAdapter::fixed_budget();
        assert_eq!(size_of::<ConservativeMacroSummary>(), 44);
        assert_eq!(budget.fixed_slots, 8);
        assert_eq!(budget.summaries_per_slot, 1_344);
        assert!(budget.adapter_bytes <= budget.adapter_byte_cap);
        assert_eq!(budget.max_reduction_work_units_per_call, 5_120);
        assert_eq!(FAR_REDUCTION_WORK_UNITS, 2_624);
        assert_eq!(MID_REDUCTION_WORK_UNITS, 3_328);
        assert_eq!(NEAR_REDUCTION_WORK_UNITS, 5_120);
        assert_eq!(MAX_SLOT_PROBES_PER_SAMPLE, 1);
        assert_eq!(
            budget.max_ticket_validation_bytes,
            ContinuumTile::accounted_output_bytes() * 2
        );
    }

    #[test]
    fn far_is_published_first_and_near_falls_back_without_a_hole() {
        let identity = identity(256);
        let coord = MacroTileCoord::new(4, -9);
        let mut adapter = WorldContinuumAdapter::new(identity).unwrap();
        let tile = generated(&adapter, coord);

        assert_eq!(
            adapter.promote_mid(&tile),
            Err(ContinuumError::MissingFarParent(coord))
        );
        adapter.install_parent(&tile).unwrap();
        let far = adapter
            .sample_cell(identity, address(coord, 13, 22), ContinuumBand::Near)
            .unwrap();
        assert_eq!(far.served_band, ContinuumBand::Far);
        assert_eq!(far.summary.source_cell_count(), 16);
        assert_eq!(
            far.summary.surface_family_counts().iter().sum::<u16>(),
            far.summary.source_cell_count()
        );
        assert_eq!(
            far.summary.guild_counts().iter().sum::<u16>(),
            far.summary.source_cell_count()
        );

        assert_eq!(
            adapter.promote_near(&tile),
            Err(ContinuumError::MissingMidParent(coord))
        );
        adapter.promote_mid(&tile).unwrap();
        let mid = adapter
            .sample_cell(identity, address(coord, 13, 22), ContinuumBand::Near)
            .unwrap();
        assert_eq!(mid.served_band, ContinuumBand::Mid);
        assert_eq!(mid.summary.source_cell_count(), 4);
        assert_eq!(
            mid.summary.surface_family_counts().iter().sum::<u16>(),
            mid.summary.source_cell_count()
        );
        assert_eq!(
            mid.summary.guild_counts().iter().sum::<u16>(),
            mid.summary.source_cell_count()
        );

        adapter.promote_near(&tile).unwrap();
        let near = adapter
            .sample_cell(identity, address(coord, 13, 22), ContinuumBand::Near)
            .unwrap();
        assert_eq!(near.served_band, ContinuumBand::Near);
        assert_eq!(near.summary.source_cell_count(), 1);
        assert!(near.summary.elevation_min_m <= near.summary.elevation_mean_m);
        assert!(near.summary.elevation_mean_m <= near.summary.elevation_max_m);
        assert_eq!(near.summary.surface_family_counts().iter().sum::<u16>(), 1);
        assert_eq!(near.summary.guild_counts().iter().sum::<u16>(), 1);

        let far_after = adapter
            .sample_cell(identity, address(coord, 13, 22), ContinuumBand::Far)
            .unwrap();
        assert_eq!(far_after.summary, far.summary);
        assert_eq!(adapter.ready_counts(), (1, 1, 1));
    }

    #[test]
    fn all_bands_are_deterministic_reductions_of_one_ticket_seed() {
        let identity = identity(256);
        let coord = MacroTileCoord::new(-17, 23);
        let mut first = WorldContinuumAdapter::new(identity).unwrap();
        let mut second = WorldContinuumAdapter::new(identity).unwrap();
        let tile_a = generated(&first, coord);
        let tile_b = generated(&second, coord);
        assert_eq!(tile_a.fingerprint(), tile_b.fingerprint());

        for adapter in [&mut first, &mut second] {
            adapter.install_parent(&tile_a).unwrap();
            adapter.promote_mid(&tile_a).unwrap();
            adapter.promote_near(&tile_a).unwrap();
        }

        for band in [ContinuumBand::Near, ContinuumBand::Mid, ContinuumBand::Far] {
            for (x, z) in [(0, 0), (1, 31), (17, 9), (31, 31)] {
                let a = first
                    .sample_cell(identity, address(coord, x, z), band)
                    .unwrap();
                let b = second
                    .sample_cell(identity, address(coord, x, z), band)
                    .unwrap();
                assert_eq!(a, b);
            }
        }
    }

    #[test]
    fn stale_epoch_revision_and_same_epoch_reseed_fail_closed() {
        let identity = identity(256);
        let mut adapter = WorldContinuumAdapter::new(identity).unwrap();
        let stale_result = generated(&adapter, MacroTileCoord::new(0, 0));
        let next = identity.with_epoch_and_revision(11, 8).unwrap();
        assert_eq!(adapter.advance_identity(next), Ok(true));
        assert_eq!(
            adapter.install_parent(&stale_result),
            Err(ContinuumError::StaleRevision {
                expected: 8,
                found: 7
            })
        );

        let lower = identity.with_epoch_and_revision(10, 99).unwrap();
        assert_eq!(
            adapter.advance_identity(lower),
            Err(ContinuumError::StaleEpoch {
                expected: 11,
                found: 10
            })
        );

        let conflicting = WorldContinuumIdentity::new(
            TEST_WORLD_ID,
            TEST_SEED ^ 1,
            identity.profile(),
            11,
            9,
            256,
        )
        .unwrap();
        assert_eq!(
            adapter.advance_identity(conflicting),
            Err(ContinuumError::IdentityConflict)
        );
        assert!(adapter.telemetry().stale_rejections >= 2);
    }

    #[test]
    fn unsupported_versions_zero_scale_and_unidentified_features_fail_closed() {
        assert!(matches!(
            WorldContinuumIdentity::versioned(
                WORLD_CONTINUUM_SCHEMA_VERSION + 1,
                MORPHOGENESIS_GRAMMAR_VERSION,
                TEST_WORLD_ID,
                TEST_SEED,
                MorphogenesisProfile::AstralCrystalline,
                11,
                7,
                256,
            ),
            Err(ContinuumError::UnsupportedSchemaVersion { .. })
        ));
        assert!(matches!(
            WorldContinuumIdentity::versioned(
                WORLD_CONTINUUM_SCHEMA_VERSION,
                MORPHOGENESIS_GRAMMAR_VERSION + 1,
                TEST_WORLD_ID,
                TEST_SEED,
                MorphogenesisProfile::AstralCrystalline,
                11,
                7,
                256,
            ),
            Err(ContinuumError::UnsupportedGrammarVersion { .. })
        ));
        assert_eq!(
            WorldContinuumIdentity::new(
                TEST_WORLD_ID,
                TEST_SEED,
                MorphogenesisProfile::AstralCrystalline,
                11,
                7,
                0,
            ),
            Err(ContinuumError::InvalidVoxelScale(0))
        );

        let identity = identity(1);
        let coord = MacroTileCoord::new(0, 0);
        let mut adapter = WorldContinuumAdapter::new(identity).unwrap();
        let tile = generated(&adapter, coord);
        adapter.install_parent(&tile).unwrap();
        let invalid = ImplicitFeatureStamp {
            world_epoch: identity.world_epoch(),
            source_revision: identity.source_revision(),
            feature_id: 0,
            feature_revision: 1,
        };
        let sphere = SphereVolume::solid(Vec3d::ZERO, 1.0).unwrap();
        let bounds = Aabb3d::new(Vec3d::splat(-0.1), Vec3d::splat(0.1)).unwrap();
        assert_eq!(
            adapter.correlate_implicit(
                identity,
                invalid,
                invalid,
                WorldVoxel::new(0, 0, 0),
                bounds,
                BoundedImplicitVolumeRef::Sphere(&sphere),
                ContinuumBand::Far,
            ),
            Err(ContinuumError::InvalidImplicitFeatureId)
        );
    }

    #[test]
    fn direct_mapped_collisions_evict_whole_tiles_not_parents_alone() {
        let identity = identity(256);
        let identity_fingerprint = identity.fingerprint();
        let first_coord = MacroTileCoord::new(0, 0);
        let first_slot = slot_index(identity_fingerprint, first_coord);
        let second_coord = (1..1_000)
            .map(|x| MacroTileCoord::new(x, -x * 3))
            .find(|coord| slot_index(identity_fingerprint, *coord) == first_slot)
            .expect("eight direct slots must collide in a bounded search");
        let mut adapter = WorldContinuumAdapter::new(identity).unwrap();
        let first = generated(&adapter, first_coord);
        adapter.install_parent(&first).unwrap();
        adapter.promote_mid(&first).unwrap();
        adapter.promote_near(&first).unwrap();

        let second = generated(&adapter, second_coord);
        assert_eq!(
            adapter.install_parent(&second),
            Ok(InstallOutcome::Evicted(first_coord))
        );
        assert_eq!(adapter.resident_slots(), 1);
        assert_eq!(adapter.ready_counts(), (1, 0, 0));
        assert_eq!(
            adapter.sample_cell(identity, address(first_coord, 0, 0), ContinuumBand::Far),
            Err(ContinuumError::MissingFarParent(first_coord))
        );
        assert!(adapter
            .sample_cell(identity, address(second_coord, 0, 0), ContinuumBand::Near)
            .unwrap()
            .used_fallback());
    }

    #[test]
    fn negative_and_extreme_macro_coordinates_keep_integer_identity() {
        let identity = identity(256);
        let minimum_global = i128::from(i64::MIN) * MACRO_TILE_SIDE as i128 + 31;
        let maximum_global = i128::from(i64::MAX) * MACRO_TILE_SIDE as i128 + 31;
        let minimum =
            MacroCellAddress::from_global_macro_cell(minimum_global, minimum_global).unwrap();
        let maximum =
            MacroCellAddress::from_global_macro_cell(maximum_global, maximum_global).unwrap();
        assert_eq!(minimum.tile, MacroTileCoord::new(i64::MIN, i64::MIN));
        assert_eq!((minimum.local_x, minimum.local_z), (31, 31));
        assert_eq!(maximum.tile, MacroTileCoord::new(i64::MAX, i64::MAX));
        assert_eq!((maximum.local_x, maximum.local_z), (31, 31));
        assert!(matches!(
            MacroCellAddress::from_global_macro_cell(maximum_global + 1, 0),
            Err(ContinuumError::MacroCoordinateOutOfRange(_))
        ));

        let mut adapter = WorldContinuumAdapter::new(identity).unwrap();
        for address in [minimum, maximum] {
            let tile = generated(&adapter, address.tile);
            adapter.install_parent(&tile).unwrap();
            let sample = adapter
                .sample_cell(identity, address, ContinuumBand::Near)
                .unwrap();
            assert!(sample.summary.elevation_min_m.is_finite());
            assert!(sample.summary.elevation_max_m.is_finite());
        }
    }

    #[test]
    fn hierarchy_material_and_occupancy_remain_separate_and_stamp_checked() {
        let identity = identity(256);
        let coord = MacroTileCoord::new(-1, -1);
        let mut adapter = WorldContinuumAdapter::new(identity).unwrap();
        let tile = generated(&adapter, coord);
        adapter.install_parent(&tile).unwrap();
        let stamp = BrickStamp {
            epoch: identity.world_epoch(),
            source_version: identity.source_revision(),
            overlay_version: 3,
        };
        let observation = HierarchyObservation {
            position: WorldVoxel::new(-1, 77, -1),
            lod: 4,
            configured_max_lod: 12,
            stamp,
            summary: CellSummary::new(42, 1, 255),
        };
        let correlation = adapter
            .correlate_hierarchy(identity, stamp, observation, ContinuumBand::Near)
            .unwrap();
        assert_eq!(correlation.macro_sample.address.tile, coord);
        assert_eq!(correlation.observation.summary, observation.summary);
        assert_eq!(correlation.macro_sample.served_band, ContinuumBand::Far);

        let stale = HierarchyObservation {
            stamp: BrickStamp {
                overlay_version: 2,
                ..stamp
            },
            ..observation
        };
        assert!(matches!(
            adapter.correlate_hierarchy(identity, stamp, stale, ContinuumBand::Far),
            Err(ContinuumError::HierarchyStampMismatch { .. })
        ));
        let stale_source = BrickStamp {
            source_version: identity.source_revision() - 1,
            ..stamp
        };
        assert_eq!(
            adapter.correlate_hierarchy(identity, stale_source, observation, ContinuumBand::Far),
            Err(ContinuumError::StaleRevision {
                expected: identity.source_revision(),
                found: identity.source_revision() - 1,
            })
        );
        let invalid_lod = HierarchyObservation {
            lod: 13,
            configured_max_lod: 12,
            ..observation
        };
        assert_eq!(
            adapter.correlate_hierarchy(identity, stamp, invalid_lod, ContinuumBand::Far),
            Err(ContinuumError::HierarchyLodOutOfRange {
                requested: 13,
                configured: 12,
            })
        );
    }

    #[test]
    fn explicit_implicit_feature_is_classified_but_never_merged_into_macro_authority() {
        let identity = identity(1);
        let coord = MacroTileCoord::new(0, 0);
        let mut adapter = WorldContinuumAdapter::new(identity).unwrap();
        let tile = generated(&adapter, coord);
        adapter.install_parent(&tile).unwrap();
        let before = adapter
            .sample_cell(identity, address(coord, 0, 0), ContinuumBand::Far)
            .unwrap();
        let feature = ImplicitFeatureStamp {
            world_epoch: identity.world_epoch(),
            source_revision: identity.source_revision(),
            feature_id: 99,
            feature_revision: 4,
        };
        let sphere = SphereVolume::solid(Vec3d::ZERO, 2.0).unwrap();
        let bounds = Aabb3d::new(Vec3d::splat(-0.25), Vec3d::splat(0.25)).unwrap();
        let correlation = adapter
            .correlate_implicit(
                identity,
                feature,
                feature,
                WorldVoxel::new(0, 0, 0),
                bounds,
                BoundedImplicitVolumeRef::Sphere(&sphere),
                ContinuumBand::Far,
            )
            .unwrap();
        assert_eq!(correlation.classification, CellClassification::Inside);
        assert_eq!(correlation.macro_sample.summary, before.summary);

        let stale = ImplicitFeatureStamp {
            feature_revision: 3,
            ..feature
        };
        assert!(matches!(
            adapter.correlate_implicit(
                identity,
                feature,
                stale,
                WorldVoxel::new(0, 0, 0),
                bounds,
                BoundedImplicitVolumeRef::Sphere(&sphere),
                ContinuumBand::Far,
            ),
            Err(ContinuumError::ImplicitFeatureStampMismatch { .. })
        ));
        let after = adapter
            .sample_cell(identity, address(coord, 0, 0), ContinuumBand::Far)
            .unwrap();
        assert_eq!(before.summary, after.summary);
    }

    #[test]
    fn twenty_thousand_kilometres_keep_bytes_slots_and_per_call_work_constant() {
        let identity = identity(256);
        let mut adapter = WorldContinuumAdapter::new(identity).unwrap();
        let fixed_bytes = adapter.accounted_bytes();
        const SAMPLES: i64 = 65;
        const HALF_ROUTE_TILES: i64 = 1_250;
        for sample in 0..SAMPLES {
            let x = -HALF_ROUTE_TILES + (2 * HALF_ROUTE_TILES * sample).div_euclid(SAMPLES - 1);
            let coord = MacroTileCoord::new(x, x.div_euclid(3));
            let tile = generated(&adapter, coord);
            assert!(tile.tile().generation.work_units <= tile.tile().generation.work_unit_cap);
            adapter.install_parent(&tile).unwrap();
            assert_eq!(adapter.accounted_bytes(), fixed_bytes);
            assert!(adapter.resident_slots() <= CONTINUUM_SLOT_COUNT);
            assert_eq!(
                adapter.telemetry().last_reduction_work_units,
                FAR_REDUCTION_WORK_UNITS
            );
        }
        let route_km = 2.0 * HALF_ROUTE_TILES as f64 * 8.192;
        assert!(route_km >= 20_000.0);
        assert_eq!(adapter.telemetry().parent_installs, SAMPLES as u64);
        assert_eq!(
            adapter.telemetry().max_reduction_work_units,
            FAR_REDUCTION_WORK_UNITS
        );
        assert_eq!(
            fixed_bytes,
            WorldContinuumAdapter::fixed_budget().adapter_bytes
        );
    }

    #[test]
    #[ignore = "microbenchmark; run optimized with --ignored --nocapture --test-threads=1"]
    fn benchmark_world_continuum_distribution() {
        const SAMPLES: usize = 100;
        const SAMPLE_BATCH: usize = 2_000;
        let identity = identity(256);
        let seed_adapter = WorldContinuumAdapter::new(identity).unwrap();
        let tiles: Vec<_> = (0..SAMPLES)
            .map(|index| {
                generated(
                    &seed_adapter,
                    MacroTileCoord::new(index as i64 * 17 - 300, index as i64 * -11 + 90),
                )
            })
            .collect();
        let mut adapter = WorldContinuumAdapter::new(identity).unwrap();
        let mut far_times = Vec::with_capacity(SAMPLES);
        let mut mid_times = Vec::with_capacity(SAMPLES);
        let mut near_times = Vec::with_capacity(SAMPLES);
        let mut sample_times = Vec::with_capacity(SAMPLES);
        let mut checksum = 0_u64;

        for tile in &tiles {
            let start = Instant::now();
            black_box(adapter.install_parent(black_box(tile)).unwrap());
            far_times.push(start.elapsed());

            let start = Instant::now();
            black_box(adapter.promote_mid(black_box(tile)).unwrap());
            mid_times.push(start.elapsed());

            let start = Instant::now();
            black_box(adapter.promote_near(black_box(tile)).unwrap());
            near_times.push(start.elapsed());

            let query = address(tile.coord(), 13, 19);
            let start = Instant::now();
            for _ in 0..SAMPLE_BATCH {
                let result = adapter
                    .sample_cell(identity, black_box(query), ContinuumBand::Near)
                    .unwrap();
                checksum ^= u64::from(result.summary.elevation_mean_m.to_bits());
            }
            sample_times.push(start.elapsed() / SAMPLE_BATCH as u32);
        }
        black_box(checksum);

        let (far_p50, far_p95, far_p99) = distribution(&mut far_times);
        let (mid_p50, mid_p95, mid_p99) = distribution(&mut mid_times);
        let (near_p50, near_p95, near_p99) = distribution(&mut near_times);
        let (sample_p50, sample_p95, sample_p99) = distribution(&mut sample_times);
        println!(
            "world_continuum_benchmark samples={SAMPLES} far_us={:.3}/{:.3}/{:.3} mid_us={:.3}/{:.3}/{:.3} near_us={:.3}/{:.3}/{:.3} sample_ns={:.2}/{:.2}/{:.2}",
            micros(far_p50),
            micros(far_p95),
            micros(far_p99),
            micros(mid_p50),
            micros(mid_p95),
            micros(mid_p99),
            micros(near_p50),
            micros(near_p95),
            micros(near_p99),
            nanos(sample_p50),
            nanos(sample_p95),
            nanos(sample_p99),
        );
    }

    fn distribution(samples: &mut [Duration]) -> (Duration, Duration, Duration) {
        samples.sort_unstable();
        let p50 = samples[(samples.len() - 1) * 50 / 100];
        let p95 = samples[(samples.len() - 1) * 95 / 100];
        let p99 = samples[(samples.len() - 1) * 99 / 100];
        (p50, p95, p99)
    }

    fn micros(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1_000_000.0
    }

    fn nanos(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1_000_000_000.0
    }
}
