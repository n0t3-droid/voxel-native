//! Chunk data and world-scale addressing.
//!
//! Ported from `lib/voxel/world.ts`. 16×16×16 chunk size (same as R93G and
//! the de-facto Minecraft standard). A dense flat array beats any fancier
//! storage for the workload we care about (meshing + neighbour sampling).

use crate::blocks::{Voxel, AIR};

pub const CHUNK_SIZE: usize = 16;
pub const CHUNK_SIZE_I: i32 = CHUNK_SIZE as i32;
pub const CHUNK_VOLUME: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE;

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
    voxels: Box<[Voxel; CHUNK_VOLUME]>,
    /// Set by the mesher/terrain; used to skip re-meshing untouched chunks.
    pub dirty: bool,
}

impl Chunk {
    pub fn new(pos: ChunkPos) -> Self {
        Self {
            pos,
            voxels: Box::new([AIR; CHUNK_VOLUME]),
            dirty: true,
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
    pub fn set(&mut self, x: usize, y: usize, z: usize, v: Voxel) {
        let i = Self::index(x, y, z);
        if self.voxels[i] != v {
            self.voxels[i] = v;
            self.dirty = true;
        }
    }

    pub fn voxels(&self) -> &[Voxel; CHUNK_VOLUME] {
        &self.voxels
    }

    /// Heap-clone the chunk's voxel storage. Used when handing a snapshot
    /// off to a background meshing task.
    pub fn clone_voxels(&self) -> Box<[Voxel; CHUNK_VOLUME]> {
        self.voxels.clone()
    }

    /// Consume the chunk's voxel storage (used by the async pipeline to
    /// hand finished terrain data back to the main thread without a copy).
    pub fn take_voxels(self) -> Box<[Voxel; CHUNK_VOLUME]> {
        self.voxels
    }

    /// Replace the chunk's voxel storage with a pre-computed box (used by
    /// the async pipeline when a background task finishes generation).
    pub fn install_voxels(&mut self, voxels: Box<[Voxel; CHUNK_VOLUME]>) {
        self.voxels = voxels;
        self.dirty = true;
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
