//! Chunk storage.
//!
//! Ported from `lib/voxel/world.ts`. We keep the same 16×16×16 chunk size
//! because all of R93G's terrain/mesher math already assumes it, and it is
//! the de-facto standard for Minecraft-like engines.

use crate::blocks::{Voxel, AIR};

pub const CHUNK_SIZE: usize = 16;
pub const CHUNK_VOLUME: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE;

/// Chunk position in chunk-space (not world-space).
/// World position of a voxel = `ChunkPos * CHUNK_SIZE + local (x,y,z)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// Flat-array chunk storage. Index layout matches R93G: `x + z*16 + y*256`
/// so rows along X are contiguous (best for the mesher's inner loop).
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
}
