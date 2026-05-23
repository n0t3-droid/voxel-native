//! Face flood-fill — given a hit voxel + outward normal, find the
//! contiguous coplanar same-block surface the user is looking at.
//!
//! "Same-block" here means same `Voxel` id (block type). Material id is
//! intentionally **not** part of the equality test: a stone wall painted
//! with two different stone textures is still one wall to the eye, and
//! Push/Pull should treat it as one face. If users later complain we can
//! relax this with a modifier (e.g. hold Alt to require material match).
//!
//! "Exposed" means `voxel_at(cell + normal)` is non-solid air — we never
//! flood across cells whose front-face is occluded, because those aren't
//! part of the visible face the player is clicking.

use ahash::AHashSet;
use bevy::math::IVec3;

use crate::blocks::{voxel_is_solid, Voxel};
use crate::world::VoxelWorld;

/// Maximum number of cells a single face can contain. Past this we stop
/// flooding (the user almost certainly didn't mean to extrude an entire
/// quarry-sized plane). Tuned so a 64×64 plaza is still a single face.
pub const FACE_CELL_CAP: usize = 16_384;

/// Result of a face flood-fill. Cells are world-space coordinates of the
/// **solid** voxels that compose the face; the air side is at
/// `cell + normal`.
#[derive(Debug, Clone)]
pub struct FaceRegion {
    pub cells: Vec<IVec3>,
    pub voxel: Voxel,
    /// Outward normal — exactly one component is ±1, the others 0.
    pub normal: IVec3,
    /// `true` if the flood was truncated by [`FACE_CELL_CAP`].
    pub clipped: bool,
}

/// Flood-fill the contiguous coplanar same-block face starting from
/// `(hit, normal)`. Returns `None` if the hit voxel itself is air, the
/// normal is malformed, or the front face isn't actually exposed (the
/// caller's raycast should already guarantee these, so `None` is a
/// programming error rather than a normal hover state).
pub fn collect_face(world: &VoxelWorld, hit: IVec3, normal: IVec3) -> Option<FaceRegion> {
    // Validate normal: exactly one component must be ±1, the others 0.
    let abs_sum = normal.x.abs() + normal.y.abs() + normal.z.abs();
    if abs_sum != 1 {
        return None;
    }

    let voxel = world.voxel_at(hit.x, hit.y, hit.z);
    if !voxel_is_solid(voxel) {
        return None;
    }

    // Front of the hit voxel must be air, otherwise the face isn't
    // exposed — bail out so the caller can fall back to "no face".
    let front = hit + normal;
    if voxel_is_solid(world.voxel_at(front.x, front.y, front.z)) {
        return None;
    }

    // Identify the two in-plane axes. For a normal along +X (1,0,0) the
    // in-plane axes are Y and Z, etc. We encode them as (in1, in2):
    let (in1, in2) = if normal.x != 0 {
        (IVec3::Y, IVec3::Z)
    } else if normal.y != 0 {
        (IVec3::X, IVec3::Z)
    } else {
        (IVec3::X, IVec3::Y)
    };

    let mut visited: AHashSet<IVec3> = AHashSet::with_capacity(64);
    let mut frontier: Vec<IVec3> = Vec::with_capacity(64);
    let mut cells: Vec<IVec3> = Vec::with_capacity(64);

    visited.insert(hit);
    frontier.push(hit);
    cells.push(hit);

    let mut clipped = false;

    while let Some(cur) = frontier.pop() {
        for delta in [in1, -in1, in2, -in2] {
            let n = cur + delta;
            if visited.contains(&n) {
                continue;
            }
            // Same block type?
            if world.voxel_at(n.x, n.y, n.z) != voxel {
                continue;
            }
            // Front face must be exposed (air).
            let nf = n + normal;
            if voxel_is_solid(world.voxel_at(nf.x, nf.y, nf.z)) {
                continue;
            }
            visited.insert(n);
            cells.push(n);
            if cells.len() >= FACE_CELL_CAP {
                clipped = true;
                frontier.clear();
                break;
            }
            frontier.push(n);
        }
    }

    Some(FaceRegion {
        cells,
        voxel,
        normal,
        clipped,
    })
}
