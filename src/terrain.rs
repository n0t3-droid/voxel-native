//! Terrain generation.
//!
//! Port target: the noise stack from R93G's `lib/voxel/terrain.ts`
//! (domain warping, ridged FBM, narrow-band caves, temperature/moisture
//! biomes). For the initial scaffold we only do a simple height-map + grass/
//! dirt/stone layering so the project builds and runs end-to-end; the full
//! R93G port will land in follow-up commits.

use crate::blocks::BlockType;
use crate::chunk::{Chunk, ChunkPos, CHUNK_SIZE};
use noise::{NoiseFn, Perlin};

pub struct TerrainGenerator {
    height: Perlin,
    pub seed: u32,
}

impl TerrainGenerator {
    pub fn new(seed: u32) -> Self {
        Self {
            height: Perlin::new(seed),
            seed,
        }
    }

    /// Fill a chunk with terrain. World y = 0 is bedrock, higher y = sky.
    pub fn generate(&self, chunk: &mut Chunk) {
        let ChunkPos { x: cx, y: cy, z: cz } = chunk.pos;

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = cx * CHUNK_SIZE as i32 + lx as i32;
                let wz = cz * CHUNK_SIZE as i32 + lz as i32;

                // Basic height map ~ 32..96. Real port will stack FBM + warp
                // + ridged noise here (see R93G terrain.ts).
                let n = self
                    .height
                    .get([wx as f64 * 0.01, wz as f64 * 0.01]);
                let surface = 48.0 + n * 16.0;
                let surface_i = surface as i32;

                for ly in 0..CHUNK_SIZE {
                    let wy = cy * CHUNK_SIZE as i32 + ly as i32;
                    let block = if wy > surface_i {
                        BlockType::Air
                    } else if wy == surface_i {
                        BlockType::Grass
                    } else if wy > surface_i - 4 {
                        BlockType::Dirt
                    } else {
                        BlockType::Stone
                    };
                    chunk.set(lx, ly, lz, block.into());
                }
            }
        }
        chunk.dirty = true;
    }
}
