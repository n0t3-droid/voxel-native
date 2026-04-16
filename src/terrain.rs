//! Terrain generation.
//!
//! Ported from R93G's `lib/voxel/terrain.ts`. The stack is:
//!
//!   1. Continentalness + erosion FBM (low freq) → large-scale landmass shape.
//!   2. Domain-warped FBM (mid freq) → organic-looking hills.
//!   3. Ridged FBM → mountain ridges in high-continentalness areas.
//!   4. 3D narrow-band cave noise → hollows under the surface.
//!   5. Temperature + Moisture classifier → biome → surface block palette.
//!
//! Each noise layer is seeded deterministically off the world seed so two
//! worlds with the same seed produce byte-identical chunks.

use crate::blocks::BlockType;
use crate::chunk::{Chunk, ChunkPos, CHUNK_SIZE, CHUNK_SIZE_I};
use noise::{NoiseFn, Perlin};

pub const WATER_LEVEL: i32 = 48;
pub const BEDROCK_LEVEL: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Biome {
    Ocean,
    Beach,
    Plains,
    Forest,
    Jungle,
    Desert,
    Savanna,
    Tundra,
    SnowyMountains,
    Mountains,
}

pub struct TerrainGenerator {
    pub seed: u32,
    continent: Perlin,
    erosion: Perlin,
    hills_a: Perlin,
    hills_b: Perlin,
    warp_x: Perlin,
    warp_z: Perlin,
    ridges: Perlin,
    caves_a: Perlin,
    caves_b: Perlin,
    temperature: Perlin,
    moisture: Perlin,
}

impl TerrainGenerator {
    pub fn new(seed: u32) -> Self {
        // Derive per-layer seeds from the world seed so everything stays
        // deterministic but each layer has its own noise field.
        Self {
            seed,
            continent: Perlin::new(seed.wrapping_add(1)),
            erosion: Perlin::new(seed.wrapping_add(2)),
            hills_a: Perlin::new(seed.wrapping_add(3)),
            hills_b: Perlin::new(seed.wrapping_add(4)),
            warp_x: Perlin::new(seed.wrapping_add(5)),
            warp_z: Perlin::new(seed.wrapping_add(6)),
            ridges: Perlin::new(seed.wrapping_add(7)),
            caves_a: Perlin::new(seed.wrapping_add(8)),
            caves_b: Perlin::new(seed.wrapping_add(9)),
            temperature: Perlin::new(seed.wrapping_add(10)),
            moisture: Perlin::new(seed.wrapping_add(11)),
        }
    }

    /// Fractional Brownian Motion (stacked octaves of Perlin noise, in [-1,1]).
    fn fbm2(&self, n: &Perlin, x: f64, z: f64, octaves: u32, lacunarity: f64, gain: f64) -> f64 {
        let mut sum = 0.0;
        let mut amp = 1.0;
        let mut freq = 1.0;
        let mut norm = 0.0;
        for _ in 0..octaves {
            sum += amp * n.get([x * freq, z * freq]);
            norm += amp;
            amp *= gain;
            freq *= lacunarity;
        }
        sum / norm.max(1e-6)
    }

    fn fbm3(&self, n: &Perlin, x: f64, y: f64, z: f64, octaves: u32) -> f64 {
        let mut sum = 0.0;
        let mut amp = 1.0;
        let mut freq = 1.0;
        let mut norm = 0.0;
        for _ in 0..octaves {
            sum += amp * n.get([x * freq, y * freq, z * freq]);
            norm += amp;
            amp *= 0.5;
            freq *= 2.0;
        }
        sum / norm.max(1e-6)
    }

    /// Ridged FBM: `1 - |noise|` per octave, stacked. Gives mountain ridges.
    fn ridged_fbm(&self, n: &Perlin, x: f64, z: f64, octaves: u32) -> f64 {
        let mut sum = 0.0;
        let mut amp = 1.0;
        let mut freq = 1.0;
        let mut norm = 0.0;
        for _ in 0..octaves {
            let v = 1.0 - n.get([x * freq, z * freq]).abs();
            sum += amp * v * v;
            norm += amp;
            amp *= 0.5;
            freq *= 2.0;
        }
        sum / norm.max(1e-6)
    }

    /// Height of the terrain surface at world (x,z), in blocks.
    fn surface_height(&self, wx: f64, wz: f64) -> (i32, f64) {
        // 1. Continentalness — very low frequency, defines ocean vs land.
        let cont = self.fbm2(&self.continent, wx * 0.0008, wz * 0.0008, 4, 2.0, 0.5);

        // 2. Erosion — smooths out where it's high, carves where it's low.
        let erod = self.fbm2(&self.erosion, wx * 0.0015, wz * 0.0015, 3, 2.0, 0.5);

        // 3. Domain-warped hills — the "lumpy" medium-scale terrain.
        let warp_scale = 40.0;
        let dx = self.warp_x.get([wx * 0.004, wz * 0.004]) * warp_scale;
        let dz = self.warp_z.get([wx * 0.004, wz * 0.004]) * warp_scale;
        let hills = self.fbm2(
            &self.hills_a,
            (wx + dx) * 0.01,
            (wz + dz) * 0.01,
            5,
            2.0,
            0.5,
        );

        // 4. Ridged mountains — only "felt" where continentalness is high.
        let ridges = self.ridged_fbm(&self.ridges, wx * 0.006, wz * 0.006, 5);
        let mountain_mask = ((cont - 0.1).max(0.0) * 2.5).min(1.0);

        // 5. Fine detail — stops large flats from looking table-flat.
        let detail = self.hills_b.get([wx * 0.05, wz * 0.05]) * 0.5;

        // Combine. Ocean floor ~ 32, plains ~ 52, mountains up to ~ 110.
        let base = 48.0 + cont * 28.0 + (1.0 - erod.abs()) * 6.0;
        let h = base + hills * 14.0 + ridges * 42.0 * mountain_mask + detail;
        (h.round() as i32, cont)
    }

    /// 3D narrow-band cave noise. Returns `true` if this world cell is
    /// hollow (carved out by a cave).
    fn is_cave(&self, wx: f64, wy: f64, wz: f64) -> bool {
        // Two FBM fields; caves live where BOTH are close to zero (narrow
        // band), which produces tunnel-like geometry rather than big blobs.
        let a = self.fbm3(&self.caves_a, wx * 0.03, wy * 0.05, wz * 0.03, 3);
        let b = self.fbm3(&self.caves_b, wx * 0.03 + 13.7, wy * 0.05 + 7.1, wz * 0.03 - 5.3, 3);
        let band = 0.08;
        a.abs() < band && b.abs() < band
    }

    /// Pick a biome for this column based on temperature + moisture +
    /// continentalness (so beaches appear at coastlines, mountains at high
    /// continentalness, etc.).
    fn biome(&self, wx: f64, wz: f64, height: i32, cont: f64) -> Biome {
        if height <= WATER_LEVEL - 2 {
            return Biome::Ocean;
        }
        if height <= WATER_LEVEL + 1 {
            return Biome::Beach;
        }

        let temp = self.temperature.get([wx * 0.0025, wz * 0.0025]);
        let moist = self.moisture.get([wx * 0.0025, wz * 0.0025]);

        if height > 90 {
            return if temp < -0.1 {
                Biome::SnowyMountains
            } else {
                Biome::Mountains
            };
        }

        if cont > 0.55 && temp > 0.2 {
            return if temp > 0.4 { Biome::Desert } else { Biome::Savanna };
        }

        if temp < -0.3 {
            return Biome::Tundra;
        }

        if moist > 0.3 && temp > 0.1 {
            return Biome::Jungle;
        }

        if moist > 0.0 {
            return Biome::Forest;
        }

        Biome::Plains
    }

    /// Pick the surface / sub-surface / stone block for a biome.
    fn blocks_for(biome: Biome) -> (BlockType, BlockType, BlockType) {
        // (surface, sub-surface 3-blocks deep, everything below)
        match biome {
            Biome::Ocean | Biome::Beach => (BlockType::Sand, BlockType::Sand, BlockType::Stone),
            Biome::Plains => (BlockType::Grass, BlockType::Dirt, BlockType::Stone),
            Biome::Forest => (BlockType::Grass, BlockType::Dirt, BlockType::Stone),
            Biome::Jungle => (BlockType::Grass, BlockType::Dirt, BlockType::Stone),
            Biome::Desert => (BlockType::Sand, BlockType::Sand, BlockType::Stone),
            Biome::Savanna => (BlockType::SavannaGrass, BlockType::Dirt, BlockType::Stone),
            Biome::Tundra => (BlockType::TundraGrass, BlockType::Dirt, BlockType::Stone),
            Biome::Mountains => (BlockType::Stone, BlockType::Stone, BlockType::Stone),
            Biome::SnowyMountains => (BlockType::Snow, BlockType::Stone, BlockType::Stone),
        }
    }

    /// Fill a chunk with terrain. Deterministic for a given (seed, pos).
    pub fn generate(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = cx * CHUNK_SIZE_I + lx as i32;
                let wz = cz * CHUNK_SIZE_I + lz as i32;
                let (surface, cont) = self.surface_height(wx as f64, wz as f64);
                let biome = self.biome(wx as f64, wz as f64, surface, cont);
                let (top, sub, core) = Self::blocks_for(biome);

                for ly in 0..CHUNK_SIZE {
                    let wy = cy * CHUNK_SIZE_I + ly as i32;

                    // Bedrock at the bottom of the world.
                    if wy <= BEDROCK_LEVEL {
                        chunk.set(lx, ly, lz, BlockType::Bedrock.into());
                        continue;
                    }

                    // Above the surface: air or water.
                    if wy > surface {
                        if wy <= WATER_LEVEL {
                            chunk.set(lx, ly, lz, BlockType::Water.into());
                        }
                        // else: leave as AIR (default-initialised).
                        continue;
                    }

                    // Carve caves — never inside the top layer (preserves
                    // the surface skin) and never right at the water line
                    // so oceans don't drain through holes.
                    let cave_allowed = wy < surface - 3
                        && wy > BEDROCK_LEVEL + 2
                        && (wy < WATER_LEVEL - 1 || wy > WATER_LEVEL + 2);
                    if cave_allowed && self.is_cave(wx as f64, wy as f64, wz as f64) {
                        continue;
                    }

                    let depth = surface - wy;
                    let block = if depth == 0 {
                        top
                    } else if depth <= 3 {
                        sub
                    } else {
                        core
                    };
                    chunk.set(lx, ly, lz, block.into());
                }
            }
        }

        chunk.dirty = true;
    }

    /// Public biome lookup at a world (x, z) column — used by the HUD.
    pub fn biome_at(&self, wx: i32, wz: i32) -> Biome {
        let (h, cont) = self.surface_height(wx as f64, wz as f64);
        self.biome(wx as f64, wz as f64, h, cont)
    }
}

// Derive Copy/Clone only for lookup (biome blocks helper is `&self`-free).
impl Clone for TerrainGenerator {
    fn clone(&self) -> Self {
        Self::new(self.seed)
    }
}
