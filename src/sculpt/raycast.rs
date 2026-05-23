//! Shared voxel ray traversal — Amanatides–Woo DDA.
//!
//! Single source of truth for "what voxel is the camera looking at?".
//! Promoted out of [`crate::builder::live_raycast_voxel`] in Phase 0;
//! `weapons.rs` still has its own copy for animation-picker semantics
//! and is intentionally left alone (its callers want slightly different
//! behaviour around translucent blocks).
//!
//! Returns the first **solid** voxel hit and the immediately preceding
//! cell along the ray. The face normal is `previous - hit` — exactly one
//! axis is ±1, the other two are 0.

use bevy::math::{IVec3, Vec3};

use crate::blocks::voxel_is_solid;
use crate::world::VoxelWorld;

/// Maximum number of grid cells to step before giving up. The previous
/// implementation used 4096 unconditionally; we keep that to preserve
/// behaviour for long-distance shots from the existing builder live tool.
const MAX_DDA_STEPS: usize = 4_096;

/// Cast a ray through the voxel grid and return `Some((hit, prev))` for
/// the first solid voxel within `max_dist` blocks. `prev` is the cell the
/// ray was in immediately before entering `hit`; `prev - hit` gives the
/// outward face normal.
///
/// Returns `None` if no solid voxel is hit within the ray's reach.
#[inline]
pub fn dda_voxel(
    world: &VoxelWorld,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
) -> Option<(IVec3, IVec3)> {
    if dir.length_squared() < 1e-6 {
        return None;
    }

    let mut x = origin.x.floor() as i32;
    let mut y = origin.y.floor() as i32;
    let mut z = origin.z.floor() as i32;

    let step_x = dir.x.signum() as i32;
    let step_y = dir.y.signum() as i32;
    let step_z = dir.z.signum() as i32;

    let t_delta_x = if dir.x != 0.0 {
        (1.0 / dir.x).abs()
    } else {
        f32::INFINITY
    };
    let t_delta_y = if dir.y != 0.0 {
        (1.0 / dir.y).abs()
    } else {
        f32::INFINITY
    };
    let t_delta_z = if dir.z != 0.0 {
        (1.0 / dir.z).abs()
    } else {
        f32::INFINITY
    };

    let nb = |p: f32, s: i32| -> f32 {
        if s > 0 {
            p.floor() + 1.0 - p
        } else if s < 0 {
            p - p.floor()
        } else {
            f32::INFINITY
        }
    };

    let mut tmx = nb(origin.x, step_x) * t_delta_x;
    let mut tmy = nb(origin.y, step_y) * t_delta_y;
    let mut tmz = nb(origin.z, step_z) * t_delta_z;

    let mut prev: IVec3;
    for _ in 0..MAX_DDA_STEPS {
        let t = tmx.min(tmy).min(tmz);
        if t > max_dist {
            return None;
        }
        prev = IVec3::new(x, y, z);
        if tmx <= tmy && tmx <= tmz {
            x += step_x;
            tmx += t_delta_x;
        } else if tmy <= tmz {
            y += step_y;
            tmy += t_delta_y;
        } else {
            z += step_z;
            tmz += t_delta_z;
        }
        if voxel_is_solid(world.voxel_at(x, y, z)) {
            return Some((IVec3::new(x, y, z), prev));
        }
    }
    None
}
