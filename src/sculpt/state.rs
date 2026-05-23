//! Resources and value types shared across all sculpt phases.
//!
//! These types are intentionally introduced in Phase 0 even though only
//! [`SculptState`] is registered with the app right now. Later phases will
//! consume the same definitions, so getting them right early avoids API
//! churn between Phase 1 (Push/Pull) and Phase 2 (Transform Gizmo).

#![allow(dead_code)]

use ahash::AHashMap;
use bevy::math::IVec3;
use bevy::prelude::Resource;

use crate::blocks::{MaterialId, Voxel};

/// Top-level interaction mode. Switched implicitly by what the cursor is
/// over (a face → Push/Pull, an existing selection → Transform, empty air
/// with paint hotkey held → Paint) — never by a separate tool slot. That
/// implicit dispatch is the whole point of "direct manipulation".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SculptMode {
    /// Cursor free, no active gesture. Hover over a flat face to enter
    /// `PushPull`; left-drag from empty space to start a marquee selection.
    #[default]
    Idle,
    /// Hovering or actively dragging a contiguous coplanar face.
    PushPull,
    /// A selection is locked and an in-world gizmo is being manipulated.
    Transform,
    /// Knife is out; two on-screen points define a cutting plane.
    Slice,
    /// Material radial picker is committed; LMB-hold paints voxels along
    /// the cursor's 3D path.
    Paint,
}

/// Per-axis snap behaviour. Default is `Voxel`; toggled with **G**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnapMode {
    Off,
    #[default]
    Voxel,
    HalfVoxel,
}

/// Modifier-key flags rolled up into a single struct so input systems
/// don't have to re-query [`bevy::input::keyboard::KeyCode`] in five places.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModifierFlags {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Result of the current frame's hover raycast. `None` if the camera is
/// not looking at any solid voxel within reach.
#[derive(Debug, Clone, Copy)]
pub struct HoverHit {
    /// Solid voxel the ray entered.
    pub voxel: IVec3,
    /// Cell the ray was in immediately before entering `voxel`. The face
    /// normal is `prev - voxel` (exactly one axis is ±1).
    pub prev: IVec3,
}

impl HoverHit {
    /// Outward face normal at the hit, encoded as a unit-length [`IVec3`]
    /// (one component is ±1, others are 0).
    #[inline]
    pub fn normal(self) -> IVec3 {
        self.prev - self.voxel
    }
}

/// What the user currently has selected. Phase 1 only ever sets `None`
/// (Push/Pull doesn't lock a selection). Phase 2's marquee produces
/// `Mask`; Phase 3's slice keeps producing `Mask` because slicing of an
/// AABB generally yields a non-cuboid result.
#[derive(Debug, Clone, Default)]
pub enum SculptSelection {
    #[default]
    None,
    /// Inclusive AABB. Cheap; used for cuboid drag-select.
    Aabb { min: IVec3, max: IVec3 },
    /// Explicit per-voxel mask. `bits[idx(x,y,z)]` set ⇔ voxel selected.
    /// Index layout matches [`VoxelBlob`]: `x + y*size.x + z*size.x*size.y`.
    Mask {
        min: IVec3,
        size: IVec3,
        /// One bit per cell in the size×size×size box. `Vec<u64>` for
        /// cheap copies; helpers in Phase 2 will wrap the bit math.
        bits: Vec<u64>,
    },
}

impl SculptSelection {
    /// Inclusive (min, max) of the selection's bounding box, if any.
    pub fn aabb(&self) -> Option<(IVec3, IVec3)> {
        match self {
            SculptSelection::None => None,
            SculptSelection::Aabb { min, max } => Some((*min, *max)),
            SculptSelection::Mask { min, size, .. } => Some((*min, *min + *size - IVec3::ONE)),
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, SculptSelection::None)
    }
}

/// Mask-aware clipboard / blob. Supersedes [`crate::builder::BuilderClipboard`]
/// for the new system; non-cuboid pieces (e.g. one half of a sliced object)
/// keep their identity through copy / rotate / paste cycles via `mask`.
///
/// Index layout: `x + y*size.x + z*size.x*size.y`. Chosen to match the
/// natural row-major iteration order in Phase 2's rotation kernels.
#[derive(Debug, Clone)]
pub struct VoxelBlob {
    pub size: IVec3,
    pub voxels: Vec<Voxel>,
    pub materials: Vec<MaterialId>,
    /// `bits[i] == true` ⇔ cell `i` is part of the blob (vs ambient air
    /// padding from a non-cuboid source). Always present — a cuboid blob
    /// just has every bit set.
    pub mask: Vec<bool>,
}

impl VoxelBlob {
    #[inline]
    pub fn idx(size: IVec3, x: i32, y: i32, z: i32) -> usize {
        (x + y * size.x + z * size.x * size.y) as usize
    }

    pub fn empty() -> Self {
        Self {
            size: IVec3::ZERO,
            voxels: Vec::new(),
            materials: Vec::new(),
            mask: Vec::new(),
        }
    }
}

/// Root resource for the sculpt subsystem. Lives even when the player is
/// not actively sculpting; that way the marquee / paint state survives
/// camera moves, pause, and editor open/close cycles.
#[derive(Resource, Default)]
pub struct SculptState {
    pub mode: SculptMode,
    pub hover: Option<HoverHit>,
    pub selection: SculptSelection,
    pub clipboard: Option<VoxelBlob>,
    pub snap: SnapMode,
    pub modifiers: ModifierFlags,
    /// Last status line for the tiny corner readout in the HUD.
    pub status: String,
    /// Per-chunk transient preview state. Populated during a Push/Pull
    /// drag so the live preview can be rolled back & reapplied each frame
    /// as the drag distance changes. Phase 1 fills this in; Phase 0 keeps
    /// the field reserved so adding it later doesn't churn this struct.
    pub preview_before: AHashMap<IVec3, Voxel>,
}
