//! Chunk data and world-scale addressing.
//!
//! Ported from `lib/voxel/world.ts`. 16×16×16 chunk size (same as R93G and
//! the de-facto Minecraft standard). A dense flat array beats any fancier
//! storage for the workload we care about (meshing + neighbour sampling).

use std::sync::Arc;

use crate::blocks::{Voxel, AIR};

pub const CHUNK_SIZE: usize = 16;
pub const CHUNK_SIZE_I: i32 = CHUNK_SIZE as i32;
pub const CHUNK_VOLUME: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE;

/// Shared, immutable chunk voxel storage. Cheap to clone (ref-count bump)
/// so background mesher tasks can snapshot center + neighbours in O(1).
pub type SharedVoxels = Arc<[Voxel; CHUNK_VOLUME]>;

/// Chunk position in chunk-space. World position of a voxel =
/// `ChunkPos * CHUNK_SIZE + local (x, y, z)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// Set by the mesher/terrain; used to skip re-meshing untouched chunks.
    pub dirty: bool,
    /// True if every voxel in the chunk is AIR. Empty chunks never need
    /// meshing and are filtered out of the mesh queue.
    pub is_empty: bool,
    /// True if the chunk is entirely one opaque solid block type. These
    /// are fully hidden by neighbours and also skipped by the mesher.
    pub is_uniform_solid: bool,
}

impl Chunk {
    pub fn new(pos: ChunkPos) -> Self {
        Self {
            pos,
            voxels: Arc::new([AIR; CHUNK_VOLUME]),
            dirty: true,
            is_empty: true,
            is_uniform_solid: false,
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

    /// Mutably edit the chunk's voxels. Uses copy-on-write via
    /// `Arc::make_mut` so in-flight snapshots aren't affected.
    pub fn set(&mut self, x: usize, y: usize, z: usize, v: Voxel) {
        let i = Self::index(x, y, z);
        if self.voxels[i] != v {
            Arc::make_mut(&mut self.voxels)[i] = v;
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
    }

    /// Cheap ref-count clone of the voxel storage — used by background
    /// meshing tasks to sample neighbours without copying ~4 KB each.
    pub fn voxels_shared(&self) -> SharedVoxels {
        self.voxels.clone()
    }

    /// Replace the chunk's voxel storage with a pre-computed buffer (used
    /// when a background terrain-gen task finishes).
    pub fn install_voxels(&mut self, voxels: SharedVoxels) {
        self.voxels = voxels;
        self.dirty = true;
        self.finalize_uniform_flags();
    }
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
