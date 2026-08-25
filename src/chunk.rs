//! Chunk data and world-scale addressing.
//!
//! Voxel Native uses 16×16×16 chunks. A dense flat array fits the authoritative
//! near-field workload: meshing, editing, and neighbour sampling.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::blocks::{MaterialId, Voxel, AIR, DEFAULT_MATERIAL};

pub const CHUNK_SIZE: usize = 16;
pub const CHUNK_SIZE_I: i32 = CHUNK_SIZE as i32;
pub const CHUNK_VOLUME: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE;

/// Shared, immutable chunk voxel storage. Cheap to clone (ref-count bump)
/// so background mesher tasks can snapshot center + neighbours in O(1).
pub type SharedVoxels = Arc<[Voxel; CHUNK_VOLUME]>;
pub type SharedMaterials = Arc<[MaterialId; CHUNK_VOLUME]>;

/// Chunk position in chunk-space. World position of a voxel =
/// `ChunkPos * CHUNK_SIZE + local (x, y, z)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl ChunkPos {
    pub fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// World coordinates of this chunk's (0,0,0) corner.
    #[inline]
    pub fn origin(self) -> (i32, i32, i32) {
        (
            self.x * CHUNK_SIZE_I,
            self.y * CHUNK_SIZE_I,
            self.z * CHUNK_SIZE_I,
        )
    }
}

pub struct Chunk {
    pub pos: ChunkPos,
    voxels: SharedVoxels,
    materials: SharedMaterials,
    /// Set by the mesher/terrain; used to skip re-meshing untouched chunks.
    pub dirty: bool,
    /// True if every voxel in the chunk is AIR. Empty chunks never need
    /// meshing and are filtered out of the mesh queue.
    pub is_empty: bool,
    /// True if the chunk is entirely one opaque solid block type. These
    /// are fully hidden by neighbours and also skipped by the mesher.
    pub is_uniform_solid: bool,
    /// When `is_uniform_solid` or `is_empty` are true, this is the voxel
    /// value that fills the chunk (so the mesher can check "is my
    /// neighbour the SAME voxel type?" — if yes, no face needs drawing).
    pub uniform_voxel: Voxel,
}

impl Chunk {
    pub fn new(pos: ChunkPos) -> Self {
        Self {
            pos,
            voxels: Arc::new([AIR; CHUNK_VOLUME]),
            materials: Arc::new([DEFAULT_MATERIAL; CHUNK_VOLUME]),
            dirty: true,
            is_empty: true,
            is_uniform_solid: false,
            uniform_voxel: AIR,
        }
    }

    /// Index layout: `x + z*16 + y*256` (X is contiguous → best for mesher).
    #[inline]
    pub fn index(x: usize, y: usize, z: usize) -> usize {
        debug_assert!(x < CHUNK_SIZE && y < CHUNK_SIZE && z < CHUNK_SIZE);
        x + z * CHUNK_SIZE + y * CHUNK_SIZE * CHUNK_SIZE
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> Voxel {
        self.voxels[Self::index(x, y, z)]
    }

    #[inline]
    #[allow(dead_code)]
    pub fn get_material(&self, x: usize, y: usize, z: usize) -> MaterialId {
        self.materials[Self::index(x, y, z)]
    }

    /// Mutably edit the chunk's voxels. Uses copy-on-write via
    /// `Arc::make_mut` so in-flight snapshots aren't affected.
    pub fn set(&mut self, x: usize, y: usize, z: usize, v: Voxel) {
        let i = Self::index(x, y, z);
        if self.voxels[i] != v {
            Arc::make_mut(&mut self.voxels)[i] = v;
            self.dirty = true;
        }
        if v == AIR {
            self.set_material(x, y, z, DEFAULT_MATERIAL);
        }
    }

    pub fn set_material(&mut self, x: usize, y: usize, z: usize, material: MaterialId) {
        let i = Self::index(x, y, z);
        if self.materials[i] != material {
            Arc::make_mut(&mut self.materials)[i] = material;
            self.dirty = true;
        }
    }

    #[allow(dead_code)]
    pub fn set_cell(&mut self, x: usize, y: usize, z: usize, v: Voxel, material: MaterialId) {
        let i = Self::index(x, y, z);
        let material = if v == AIR { DEFAULT_MATERIAL } else { material };
        let mut changed = false;
        if self.voxels[i] != v {
            Arc::make_mut(&mut self.voxels)[i] = v;
            changed = true;
        }
        if self.materials[i] != material {
            Arc::make_mut(&mut self.materials)[i] = material;
            changed = true;
        }
        if changed {
            self.dirty = true;
        }
    }

    /// Recompute `is_empty` / `is_uniform_solid`. Call once after bulk
    /// terrain generation — doing this on every `set` during generation
    /// would be O(n²).
    pub fn finalize_uniform_flags(&mut self) {
        let first = self.voxels[0];
        let mut uniform = true;
        for v in self.voxels.iter() {
            if *v != first {
                uniform = false;
                break;
            }
        }
        self.is_empty = uniform && first == AIR;
        self.is_uniform_solid = uniform && first != AIR;
        self.uniform_voxel = if uniform { first } else { AIR };
    }

    /// Cheap ref-count clone of the voxel storage — used by background
    /// meshing tasks to sample neighbours without copying ~4 KB each.
    pub fn voxels_shared(&self) -> SharedVoxels {
        self.voxels.clone()
    }

    pub fn materials_shared(&self) -> SharedMaterials {
        self.materials.clone()
    }

    pub fn voxels_vec(&self) -> Vec<Voxel> {
        self.voxels.iter().copied().collect()
    }

    pub fn materials_vec(&self) -> Vec<MaterialId> {
        self.materials.iter().copied().collect()
    }

    /// Replace the chunk's voxel storage with a pre-computed buffer (used
    /// when a background terrain-gen task finishes).
    pub fn install_voxels(&mut self, voxels: SharedVoxels) {
        self.voxels = voxels;
        self.materials = Arc::new([DEFAULT_MATERIAL; CHUNK_VOLUME]);
        self.dirty = true;
        self.finalize_uniform_flags();
    }

    pub fn install_voxels_and_materials(
        &mut self,
        voxels: SharedVoxels,
        materials: SharedMaterials,
    ) {
        self.voxels = voxels;
        self.materials = materials;
        self.dirty = true;
        self.finalize_uniform_flags();
    }
}

/// Sanitised `f32 -> i32` conversion. Plain `x as i32` saturates on
/// `±inf` (producing `i32::MAX/MIN`) and returns `0` on `NaN`, which
/// silently turns physics glitches (a NaN velocity, a freshly-broken
/// transform) into wild coordinates that poke into random chunks.
/// This helper clamps to the safe integer range f32 can represent
/// exactly (`±2^24`) and maps NaN to 0 — the player stays near the
/// origin instead of tunnelling to the i32 edge.
#[inline]
pub fn to_i32_safe(x: f32) -> i32 {
    if x.is_nan() {
        return 0;
    }
    const MAX_EXACT: f32 = 16_777_216.0; // 2^24
    x.clamp(-MAX_EXACT, MAX_EXACT) as i32
}

/// `floor()` variant of [`to_i32_safe`] — same sanitisation, used at
/// every block-coordinate sampling site.
#[inline]
pub fn floor_to_i32_safe(x: f32) -> i32 {
    if x.is_nan() {
        return 0;
    }
    const MAX_EXACT: f32 = 16_777_216.0;
    x.clamp(-MAX_EXACT, MAX_EXACT).floor() as i32
}

/// Convert a world-space block coordinate to (chunk, local) coordinates.
/// Uses floor-division so negative coordinates work correctly.
#[inline]
pub fn world_to_chunk(wx: i32, wy: i32, wz: i32) -> (ChunkPos, usize, usize, usize) {
    let cx = wx.div_euclid(CHUNK_SIZE_I);
    let cy = wy.div_euclid(CHUNK_SIZE_I);
    let cz = wz.div_euclid(CHUNK_SIZE_I);
    let lx = wx.rem_euclid(CHUNK_SIZE_I) as usize;
    let ly = wy.rem_euclid(CHUNK_SIZE_I) as usize;
    let lz = wz.rem_euclid(CHUNK_SIZE_I) as usize;
    (ChunkPos::new(cx, cy, cz), lx, ly, lz)
}
