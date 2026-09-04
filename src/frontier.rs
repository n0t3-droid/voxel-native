//! Cinematic frontier features — the parts of the key-art planet that a
//! height-field alone can never produce.
//!
//! `terrain.rs` builds the ground: strata, canyons, spires, biomes. This
//! module adds the four silhouettes that make the reference image read as
//! a *place* rather than a landscape:
//!
//!   1. **Sky islands** — grass-capped rock slabs hanging in mid-air with
//!      a long crystal root growing down out of the underside.
//!   2. **Skyways** — winding elevated carriageways on pylons that bridge
//!      the canyons and cut through the mesas.
//!   3. **Sky stations** — hovering docking platforms with masts, holo
//!      windows and neon rim lights.
//!   4. **Crystal clusters** — the tilted magenta / cyan / emerald shard
//!      groups that erupt out of cliff shoulders.
//!
//! Everything here is a **pure function of world coordinates**. Features
//! are anchored to a coarse deterministic lattice, so a feature that
//! straddles a chunk boundary produces exactly the same voxels no matter
//! which chunk is generated first, in which order, or on which thread.
//! That is the whole reason these live outside the per-chunk decoration
//! passes in `terrain.rs`, which can only place chunk-local props.

use noise::{NoiseFn, Perlin};

use crate::blocks::{voxel_is_emissive, BlockType, AIR};
use crate::chunk::{Chunk, ChunkPos, CHUNK_SIZE, CHUNK_SIZE_I};

/// Sky islands are anchored one per this many blocks of world grid.
/// Roughly one island every 13 chunks keeps them a landmark rather than
/// a ceiling.
pub const SKY_ISLAND_CELL: i32 = 208;
/// Docking stations are much rarer than islands — they are the "capital
/// ship on the horizon" beat.
pub const STATION_CELL: i32 = 512;
/// Crystal clusters are common: they are ground detail, not landmarks.
pub const CRYSTAL_CELL: i32 = 96;

/// Postcard composition parked in front of the default look direction
/// (camera forward is −Z at yaw 0). Kept within ~200 blocks of origin
/// so the first 10 seconds of a new world match the key art instead of
/// an empty mesa.
pub const HERO_CRYSTAL_X: i32 = 72;
pub const HERO_CRYSTAL_Z: i32 = -96;
pub const HERO_RIVER_X0: i32 = 36;
pub const HERO_RIVER_X1: i32 = 200;
pub const HERO_RIVER_Z: i32 = -72;
pub const HERO_SKYWAY_X0: i32 = -36;
pub const HERO_SKYWAY_X1: i32 = 188;
pub const HERO_SKYWAY_Z: i32 = -48;

/// Inclusive AABB of the spawn postcard. Terrain in this box is forced
/// to banded mesa country so the first look is the key-art ground, not
/// whatever plains the seed happened to put at the origin.
pub fn in_hero_postcard(wx: i32, wz: i32) -> bool {
    wx >= -48 && wx <= 220 && wz >= -180 && wz <= 28
}

/// Altitude band for sky islands.
///
/// Airborne landmarks are only worth generating inside the vertical slab
/// the streamer actually loads (`settings.vertical_chunks` × 16 blocks,
/// measured from y = 0). An island above that ceiling is invisible no
/// matter how good it looks, so the band is clamped to fit the default
/// streaming budget with the whole root and cap inside it.
pub const SKY_ISLAND_MIN_Y: i32 = 86;
pub const SKY_ISLAND_MAX_Y: i32 = 138;
/// Altitude band for docking stations — above the islands, still under
/// the default ceiling once the mast is accounted for.
pub const STATION_MIN_Y: i32 = 112;
pub const STATION_MAX_Y: i32 = 132;

/// Half-width of a skyway carriageway, in blocks. Nine blocks wide plus
/// guardrails reads as a real multi-lane highway at flight speed.
const SKYWAY_HALF_WIDTH: f64 = 4.5;
/// Blocks of headroom kept clear above a deck so the road is drivable
/// even where it cuts straight through a mesa shoulder.
const SKYWAY_CLEARANCE: i32 = 6;
/// Pylon lattice pitch. Pylons drop wherever the route crosses one.
const SKYWAY_PYLON_PITCH: i32 = 24;

/// Cheap deterministic hash → float in `[0, 1)` keyed by
/// `(seed, salt, x, z)`. Same construction as `terrain::column_rand`,
/// with an extra salt so one lattice cell can produce many independent
/// rolls without correlating between features.
#[inline]
pub fn cell_rand(seed: u32, salt: u32, x: i32, z: i32) -> f64 {
    let mut h = (seed as u64) ^ ((salt as u64).wrapping_mul(0x9E37_79B1_85EB_CA87));
    h ^= (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h = h.rotate_left(27).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= (z as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    h = h.rotate_left(31).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    ((h >> 11) as f64) * (1.0 / (1u64 << 53) as f64)
}

/// Write `block` at world position `(wx, wy, wz)` if that position falls
/// inside `chunk` and the slot is currently air. Returns `true` when the
/// write landed, so callers can cheaply detect "nothing of this feature
/// is in this chunk".
#[inline]
fn place(chunk: &mut Chunk, origin: (i32, i32, i32), wx: i32, wy: i32, wz: i32, block: BlockType) {
    let lx = wx - origin.0;
    let ly = wy - origin.1;
    let lz = wz - origin.2;
    if lx < 0 || lx >= CHUNK_SIZE_I || ly < 0 || ly >= CHUNK_SIZE_I || lz < 0 || lz >= CHUNK_SIZE_I
    {
        return;
    }
    let (lx, ly, lz) = (lx as usize, ly as usize, lz as usize);
    if chunk.get(lx, ly, lz) == AIR {
        chunk.set(lx, ly, lz, block.into());
    }
}

/// Same as [`place`], but overwrites whatever is already there. Used for
/// structural surfaces (deck slabs, platform floors) that must win against
/// terrain rather than being swallowed by it.
#[inline]
fn place_over(
    chunk: &mut Chunk,
    origin: (i32, i32, i32),
    wx: i32,
    wy: i32,
    wz: i32,
    block: BlockType,
) {
    let lx = wx - origin.0;
    let ly = wy - origin.1;
    let lz = wz - origin.2;
    if lx < 0 || lx >= CHUNK_SIZE_I || ly < 0 || ly >= CHUNK_SIZE_I || lz < 0 || lz >= CHUNK_SIZE_I
    {
        return;
    }
    chunk.set(lx as usize, ly as usize, lz as usize, block.into());
}

/// Overwrite terrain with air so a terrace can bite into a cliff.
/// Leaves emissive fluids/crystals alone so rivers and shards survive.
#[inline]
fn carve(chunk: &mut Chunk, origin: (i32, i32, i32), wx: i32, wy: i32, wz: i32) {
    let lx = wx - origin.0;
    let ly = wy - origin.1;
    let lz = wz - origin.2;
    if lx < 0 || lx >= CHUNK_SIZE_I || ly < 0 || ly >= CHUNK_SIZE_I || lz < 0 || lz >= CHUNK_SIZE_I
    {
        return;
    }
    let (lx, ly, lz) = (lx as usize, ly as usize, lz as usize);
    let v = chunk.get(lx, ly, lz);
    if v != AIR && !voxel_is_emissive(v) {
        chunk.set(lx, ly, lz, AIR);
    }
}

/// Structural overwrite that will not bury lava, plasma, or crystal.
#[inline]
fn place_over_unless_glow(
    chunk: &mut Chunk,
    origin: (i32, i32, i32),
    wx: i32,
    wy: i32,
    wz: i32,
    block: BlockType,
) {
    let lx = wx - origin.0;
    let ly = wy - origin.1;
    let lz = wz - origin.2;
    if lx < 0 || lx >= CHUNK_SIZE_I || ly < 0 || ly >= CHUNK_SIZE_I || lz < 0 || lz >= CHUNK_SIZE_I
    {
        return;
    }
    let (lx, ly, lz) = (lx as usize, ly as usize, lz as usize);
    if voxel_is_emissive(chunk.get(lx, ly, lz)) {
        return;
    }
    chunk.set(lx, ly, lz, block.into());
}

// ---------------------------------------------------------------------------
// Sky islands ---------------------------------------------------------------
// ---------------------------------------------------------------------------

/// One floating island: a shallow dome of rock with a green cap and a
/// tapering crystal root hanging beneath it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyIsland {
    /// Island centre column.
    pub cx: i32,
    pub cz: i32,
    /// Altitude of the island's "waterline" — the widest slice.
    pub cy: i32,
    /// Nominal horizontal radius in blocks.
    pub radius: i32,
    /// Phase used to wobble the outline so islands are not discs.
    pub phase: f32,
}

impl SkyIsland {
    /// Roll the island anchored in lattice cell `(ix, iz)`, if that cell
    /// has one. `ground` is the *smooth macro* elevation, not the real
    /// surface: islands should ride the broad shape of the land rather
    /// than pop up and down with every ridge they pass over.
    pub fn for_cell(
        seed: u32,
        ix: i32,
        iz: i32,
        ground: impl Fn(i32, i32) -> i32,
    ) -> Option<SkyIsland> {
        // Not every cell gets an island; a sky full of them stops reading
        // as remarkable and boxes the player in from above.
        if cell_rand(seed, 0x15_1A_11, ix, iz) > 0.62 {
            return None;
        }
        let jitter_x = (cell_rand(seed, 0x15_1A_12, ix, iz) * SKY_ISLAND_CELL as f64) as i32;
        let jitter_z = (cell_rand(seed, 0x15_1A_13, ix, iz) * SKY_ISLAND_CELL as f64) as i32;
        let cx = ix * SKY_ISLAND_CELL + jitter_x;
        let cz = iz * SKY_ISLAND_CELL + jitter_z;
        let radius = 13 + (cell_rand(seed, 0x15_1A_14, ix, iz) * 12.0) as i32;

        // Lift scales with radius: a wide island grows a proportionally
        // longer root, and a fixed lift would plant that root in the dirt.
        let lift = 30 + radius + (cell_rand(seed, 0x15_1A_15, ix, iz) * 28.0) as i32;
        let cy = (ground(cx, cz) + lift).clamp(SKY_ISLAND_MIN_Y, SKY_ISLAND_MAX_Y);

        Some(SkyIsland {
            cx,
            cz,
            cy,
            radius,
            phase: (cell_rand(seed, 0x15_1A_16, ix, iz) * 6.283) as f32,
        })
    }

    /// Every island whose bounding box can reach the chunk at `(cx, cz)`.
    pub fn near(seed: u32, cx: i32, cz: i32, ground: impl Fn(i32, i32) -> i32 + Copy) -> Vec<Self> {
        let ix = cx.div_euclid(SKY_ISLAND_CELL);
        let iz = cz.div_euclid(SKY_ISLAND_CELL);
        let mut out = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                if let Some(island) = Self::for_cell(seed, ix + dx, iz + dz, ground) {
                    out.push(island);
                }
            }
        }
        out
    }

    /// Vertical span `(bottom, top)` of island material in this column,
    /// or `None` when the column misses the island.
    pub fn column(&self, wx: i32, wz: i32) -> Option<(i32, i32)> {
        let dx = (wx - self.cx) as f32;
        let dz = (wz - self.cz) as f32;
        let d = (dx * dx + dz * dz).sqrt();
        // Wobble the outline with two harmonics of the bearing so the
        // silhouette is a weathered slab, not a cylinder.
        let angle = dz.atan2(dx);
        let wobble = 1.0
            + 0.16 * (angle * 3.0 + self.phase).sin()
            + 0.09 * (angle * 5.0 - self.phase * 1.7).sin();
        let radius = self.radius as f32 * wobble;
        if radius <= 1.0 || d >= radius {
            return None;
        }
        let t = d / radius;

        // Top: a gentle dome. Bottom: a long root that tapers to a point
        // under the centre — the classic floating-island read.
        let top = self.cy + ((1.0 - t * t) * self.radius as f32 * 0.30).round() as i32;
        let depth = (1.0 - t).powf(1.6) * self.radius as f32 * 1.55;
        let bottom = self.cy - depth.round() as i32;
        if bottom > top {
            return None;
        }
        Some((bottom, top))
    }

    /// Material for one voxel of the island body.
    pub fn block_at(
        &self,
        seed: u32,
        wx: i32,
        wy: i32,
        wz: i32,
        bottom: i32,
        top: i32,
    ) -> BlockType {
        let from_bottom = wy - bottom;
        let from_top = top - wy;
        // The lowest few blocks of the root are the glowing crystal tip
        // that lights the underside in the key art.
        if from_bottom <= 2 {
            return if cell_rand(seed, 0x15_1A_21, wx, wz) < 0.45 {
                BlockType::LuminiteCrystal
            } else {
                BlockType::Crystal
            };
        }
        if from_bottom <= 5 {
            return BlockType::Crystal;
        }
        if from_top == 0 {
            return BlockType::Grass;
        }
        if from_top <= 2 {
            return BlockType::Dirt;
        }
        strata_block(wy)
    }

    /// Stamp the island into `chunk`. Cheap no-op when they do not overlap.
    pub fn stamp(&self, seed: u32, chunk: &mut Chunk) {
        let origin = chunk.pos.origin();
        let (ox, oy, oz) = origin;
        // Bail out early on the vertical axis: most chunks are nowhere
        // near the island's altitude band.
        let lowest = self.cy - (self.radius as f32 * 1.6).round() as i32;
        let highest = self.cy + (self.radius as f32 * 0.35).round() as i32 + 6;
        if oy > highest || oy + CHUNK_SIZE_I <= lowest {
            return;
        }
        if (ox + CHUNK_SIZE_I <= self.cx - self.radius - 2)
            || (ox > self.cx + self.radius + 2)
            || (oz + CHUNK_SIZE_I <= self.cz - self.radius - 2)
            || (oz > self.cz + self.radius + 2)
        {
            return;
        }

        for lz in 0..CHUNK_SIZE_I {
            for lx in 0..CHUNK_SIZE_I {
                let wx = ox + lx;
                let wz = oz + lz;
                let Some((bottom, top)) = self.column(wx, wz) else {
                    continue;
                };
                let lo = bottom.max(oy);
                let hi = top.min(oy + CHUNK_SIZE_I - 1);
                for wy in lo..=hi {
                    let block = self.block_at(seed, wx, wy, wz, bottom, top);
                    place_over(chunk, origin, wx, wy, wz, block);
                }
                // A scatter of crystal spikes and shrubs on the green cap
                // so the island silhouette is not a bare table.
                if top >= oy && top < oy + CHUNK_SIZE_I - 4 {
                    let r = cell_rand(seed, 0x15_1A_31, wx, wz);
                    if r < 0.020 {
                        let h = 2 + (r * 400.0) as i32 % 4;
                        for k in 1..=h {
                            place(
                                chunk,
                                origin,
                                wx,
                                top + k,
                                wz,
                                crystal_tint(seed, wx, wz, k),
                            );
                        }
                    } else if r < 0.055 {
                        place(chunk, origin, wx, top + 1, wz, BlockType::Leaves);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Strata --------------------------------------------------------------------
// ---------------------------------------------------------------------------

/// Sedimentary band for world height `wy`.
///
/// A pure function of Y, so adjacent columns always line up into the
/// continuous horizontal stripes that define the cliff faces in the key
/// art: violet at the base, ochre and brick through the middle, buff
/// cap-rock on top, with a thin luminous vein every cycle.
pub fn strata_block(wy: i32) -> BlockType {
    match wy.rem_euclid(34) / 4 {
        0 => BlockType::VioletStone,
        1 => BlockType::RedStone,
        2 => BlockType::AmberStone,
        3 => BlockType::MesaClay,
        4 => BlockType::VioletStone,
        5 => BlockType::AmberStone,
        6 => BlockType::RedSand,
        _ => BlockType::MesaClay,
    }
}

/// Pick a crystal colour for a shard voxel. Clusters lean toward one hue
/// but always carry a second colour so they read as gem clusters rather
/// than monochrome spikes.
pub fn crystal_tint(seed: u32, wx: i32, wz: i32, k: i32) -> BlockType {
    let hue = cell_rand(seed, 0x0C_17_5A, wx.div_euclid(8), wz.div_euclid(8));
    let mix = cell_rand(seed, 0x0C_17_5B, wx, wz + k * 31);
    let secondary = mix < 0.22;
    if hue < 0.34 {
        if secondary {
            BlockType::CrystalMagenta
        } else {
            BlockType::Crystal
        }
    } else if hue < 0.62 {
        if secondary {
            BlockType::Crystal
        } else {
            BlockType::CrystalMagenta
        }
    } else if hue < 0.84 {
        if secondary {
            BlockType::LuminiteCrystal
        } else {
            BlockType::CrystalGreen
        }
    } else if secondary {
        BlockType::CrystalMagenta
    } else {
        BlockType::IridiumVein
    }
}

// ---------------------------------------------------------------------------
// Crystal clusters ----------------------------------------------------------
// ---------------------------------------------------------------------------

/// A group of tilted crystal shards erupting from the ground.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrystalCluster {
    pub cx: i32,
    pub cz: i32,
    pub shards: u32,
    pub scale: i32,
}

impl CrystalCluster {
    /// Guaranteed shard group for the spawn postcard. Always generated,
    /// independent of the lattice roll.
    pub fn hero() -> Self {
        Self {
            cx: HERO_CRYSTAL_X,
            cz: HERO_CRYSTAL_Z,
            shards: 7,
            scale: 28,
        }
    }

    /// Second shard group further into the postcard look so crystals
    /// stay a landmark once the camera has flown to ~x=90.
    pub fn hero_b() -> Self {
        Self {
            cx: 142,
            cz: -78,
            shards: 5,
            scale: 22,
        }
    }

    pub fn for_cell(seed: u32, ix: i32, iz: i32) -> Option<Self> {
        if cell_rand(seed, 0x0C_10_01, ix, iz) > 0.42 {
            return None;
        }
        let cx =
            ix * CRYSTAL_CELL + (cell_rand(seed, 0x0C_10_02, ix, iz) * CRYSTAL_CELL as f64) as i32;
        let cz =
            iz * CRYSTAL_CELL + (cell_rand(seed, 0x0C_10_03, ix, iz) * CRYSTAL_CELL as f64) as i32;
        Some(Self {
            cx,
            cz,
            shards: 3 + (cell_rand(seed, 0x0C_10_04, ix, iz) * 5.0) as u32,
            scale: 10 + (cell_rand(seed, 0x0C_10_05, ix, iz) * 20.0) as i32,
        })
    }

    pub fn near(seed: u32, cx: i32, cz: i32) -> Vec<Self> {
        let ix = cx.div_euclid(CRYSTAL_CELL);
        let iz = cz.div_euclid(CRYSTAL_CELL);
        let mut out = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                if let Some(cluster) = Self::for_cell(seed, ix + dx, iz + dz) {
                    out.push(cluster);
                }
            }
        }
        out
    }

    /// Stamp every shard into `chunk`. `ground` gives the terrain surface
    /// height for a column so shards are rooted, not floating.
    pub fn stamp(&self, seed: u32, chunk: &mut Chunk, ground: impl Fn(i32, i32) -> i32) {
        let origin = chunk.pos.origin();
        let (ox, _, oz) = origin;
        // Widest possible footprint: shard offsets plus lean plus radius.
        let reach = 12 + self.scale / 3;
        if (ox + CHUNK_SIZE_I <= self.cx - reach)
            || (ox > self.cx + reach)
            || (oz + CHUNK_SIZE_I <= self.cz - reach)
            || (oz > self.cz + reach)
        {
            return;
        }

        for s in 0..self.shards {
            let salt = 0x0C_20_00 + s * 7919;
            let bx = self.cx + ((cell_rand(seed, salt, self.cx, self.cz) - 0.5) * 14.0) as i32;
            let bz = self.cz + ((cell_rand(seed, salt + 1, self.cx, self.cz) - 0.5) * 14.0) as i32;
            let height =
                (self.scale as f64 * (0.55 + cell_rand(seed, salt + 2, bx, bz) * 0.85)) as i32;
            if height < 4 {
                continue;
            }
            let base_radius = 1 + (height / 9).min(3);
            // Lean, expressed as horizontal drift per block of height.
            let lean_x = (cell_rand(seed, salt + 3, bx, bz) - 0.5) * 0.5;
            let lean_z = (cell_rand(seed, salt + 4, bx, bz) - 0.5) * 0.5;
            let base_y = ground(bx, bz);

            for k in 0..height {
                let wy = base_y + k;
                let taper = 1.0 - (k as f64 / height as f64);
                let radius = (base_radius as f64 * taper).round() as i32;
                let cx = bx + (lean_x * k as f64).round() as i32;
                let cz = bz + (lean_z * k as f64).round() as i32;
                for dz in -radius..=radius {
                    for dx in -radius..=radius {
                        // Diamond cross-section — voxel crystals need
                        // chamfered corners to read as faceted gems.
                        if dx.abs() + dz.abs() > radius {
                            continue;
                        }
                        place(
                            chunk,
                            origin,
                            cx + dx,
                            wy,
                            cz + dz,
                            crystal_tint(seed, bx, bz, k),
                        );
                    }
                }
            }
            // Bright tip so every shard ends in a highlight.
            place(
                chunk,
                origin,
                bx + (lean_x * height as f64).round() as i32,
                base_y + height,
                bz + (lean_z * height as f64).round() as i32,
                BlockType::LuminiteCrystal,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Sky stations --------------------------------------------------------------
// ---------------------------------------------------------------------------

/// A hovering docking platform: disc, core tower, docking arms, mast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkyStation {
    pub cx: i32,
    pub cz: i32,
    /// Altitude of the platform deck.
    pub cy: i32,
    pub radius: i32,
    pub tower: i32,
}

impl SkyStation {
    pub fn for_cell(
        seed: u32,
        ix: i32,
        iz: i32,
        ground: impl Fn(i32, i32) -> i32,
    ) -> Option<SkyStation> {
        if cell_rand(seed, 0x57_A7_01, ix, iz) > 0.55 {
            return None;
        }
        let cx =
            ix * STATION_CELL + (cell_rand(seed, 0x57_A7_02, ix, iz) * STATION_CELL as f64) as i32;
        let cz =
            iz * STATION_CELL + (cell_rand(seed, 0x57_A7_03, ix, iz) * STATION_CELL as f64) as i32;
        let radius = 8 + (cell_rand(seed, 0x57_A7_04, ix, iz) * 5.0) as i32;
        let lift = 62 + (cell_rand(seed, 0x57_A7_05, ix, iz) * 40.0) as i32;
        let cy = (ground(cx, cz) + lift).clamp(STATION_MIN_Y, STATION_MAX_Y);
        Some(SkyStation {
            cx,
            cz,
            cy,
            radius,
            tower: 7 + (cell_rand(seed, 0x57_A7_06, ix, iz) * 6.0) as i32,
        })
    }

    pub fn near(seed: u32, cx: i32, cz: i32, ground: impl Fn(i32, i32) -> i32 + Copy) -> Vec<Self> {
        let ix = cx.div_euclid(STATION_CELL);
        let iz = cz.div_euclid(STATION_CELL);
        let mut out = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                if let Some(station) = Self::for_cell(seed, ix + dx, iz + dz, ground) {
                    out.push(station);
                }
            }
        }
        out
    }

    pub fn stamp(&self, chunk: &mut Chunk) {
        let origin = chunk.pos.origin();
        let (ox, oy, oz) = origin;
        let reach = self.radius + 7;
        if (ox + CHUNK_SIZE_I <= self.cx - reach)
            || (ox > self.cx + reach)
            || (oz + CHUNK_SIZE_I <= self.cz - reach)
            || (oz > self.cz + reach)
            || (oy > self.cy + self.tower + 8)
            || (oy + CHUNK_SIZE_I <= self.cy - 4)
        {
            return;
        }

        let r = self.radius;
        // ---- Disc: deck, rim, tapered underside -----------------------
        for dz in -r..=r {
            for dx in -r..=r {
                let d2 = dx * dx + dz * dz;
                if d2 > r * r {
                    continue;
                }
                let wx = self.cx + dx;
                let wz = self.cz + dz;
                let rim = d2 > (r - 1) * (r - 1);
                place_over(
                    chunk,
                    origin,
                    wx,
                    self.cy,
                    wz,
                    if rim {
                        BlockType::PlatingTeal
                    } else {
                        BlockType::PlatingWhite
                    },
                );
                // Underside cone — three shrinking rings so the platform
                // reads as a hull, not a floating pancake.
                let under = ((r * r - d2) as f32).sqrt() as i32 / 3;
                for k in 1..=under {
                    place_over(
                        chunk,
                        origin,
                        wx,
                        self.cy - k,
                        wz,
                        if k >= under {
                            BlockType::EngineCore
                        } else {
                            BlockType::PlatingTeal
                        },
                    );
                }
                // Rim running lights.
                if rim && (dx + dz).rem_euclid(4) == 0 {
                    place(chunk, origin, wx, self.cy + 1, wz, BlockType::NeonCyan);
                }
            }
        }

        // ---- Core tower with holo windows -----------------------------
        let core = (r / 3).max(2);
        for k in 1..=self.tower {
            for dz in -core..=core {
                for dx in -core..=core {
                    if dx * dx + dz * dz > core * core {
                        continue;
                    }
                    let edge = dx.abs() == core || dz.abs() == core;
                    let block = if edge && k.rem_euclid(3) == 2 {
                        BlockType::HoloPanel
                    } else if edge {
                        BlockType::ShipHullAlloy
                    } else {
                        BlockType::PlatingWhite
                    };
                    place_over(
                        chunk,
                        origin,
                        self.cx + dx,
                        self.cy + k,
                        self.cz + dz,
                        block,
                    );
                }
            }
        }

        // ---- Docking arms ---------------------------------------------
        for (sx, sz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            for k in r..(r + 6) {
                let wx = self.cx + sx * k;
                let wz = self.cz + sz * k;
                place_over(chunk, origin, wx, self.cy, wz, BlockType::ShipHullAlloy);
                if k == r + 5 {
                    place(chunk, origin, wx, self.cy + 1, wz, BlockType::NeonAmber);
                }
            }
        }

        // ---- Mast ------------------------------------------------------
        let mast_top = self.cy + self.tower + 6;
        for wy in (self.cy + self.tower + 1)..=mast_top {
            place_over(
                chunk,
                origin,
                self.cx,
                wy,
                self.cz,
                BlockType::ShipHullAlloy,
            );
        }
        place_over(
            chunk,
            origin,
            self.cx,
            mast_top + 1,
            self.cz,
            BlockType::NeonMagenta,
        );
    }
}

// ---------------------------------------------------------------------------
// Skyways -------------------------------------------------------------------
// ---------------------------------------------------------------------------

/// What a skyway does to one world column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkywayColumn {
    /// Y of the carriageway surface.
    pub deck_y: i32,
    /// Distance from the route centreline, in blocks.
    pub dist: f64,
    /// True when this column sits on the pylon lattice, so a support
    /// should be dropped from the deck down to the ground.
    pub pylon: bool,
    /// Half-width of this ribbon. Postcard rails are narrower than the
    /// main carriageway so Fast-mode meshing stays a thin extra strip.
    pub half: f64,
}

impl SkywayColumn {
    /// Block for one voxel of the deck cross-section, or `None` for a
    /// voxel that should simply be left empty.
    pub fn deck_block(&self, wy: i32, lamp: bool) -> Option<BlockType> {
        let edge = self.dist > self.half - 1.2;
        if wy == self.deck_y {
            return Some(if self.dist < 0.75 {
                BlockType::RoadMarking
            } else if edge {
                BlockType::PlatingWhite
            } else {
                BlockType::RoadDeck
            });
        }
        if wy == self.deck_y - 1 {
            return Some(if edge {
                BlockType::PlatingTeal
            } else {
                BlockType::PlatingWhite
            });
        }
        if wy == self.deck_y + 1 && edge {
            return Some(BlockType::PlatingWhite);
        }
        if wy == self.deck_y + 2 && edge && lamp {
            // Warm streetlamps — cyan was blowing the Cinematic bloom
            // buffer and wiping the mesa around every skyway.
            return Some(BlockType::NeonAmber);
        }
        None
    }
}

/// The elevated road network. Routes are the zero-contours of a very
/// low-frequency noise field, which yields endlessly winding curves that
/// never dead-end and never need cross-chunk bookkeeping.
pub struct SkywayNetwork {
    route: Perlin,
    deck: Perlin,
}

impl SkywayNetwork {
    pub fn new(seed: u32) -> Self {
        Self {
            route: Perlin::new(seed.wrapping_add(41)),
            deck: Perlin::new(seed.wrapping_add(42)),
        }
    }

    #[inline]
    fn route_field(&self, wx: f64, wz: f64) -> f64 {
        // Two octaves only: more would add wiggle at a scale smaller than
        // the road is wide, which just looks like noise.
        self.route.get([wx * 0.00085, wz * 0.00085]) * 0.72
            + self.route.get([wx * 0.0021 + 31.7, wz * 0.0021 - 12.3]) * 0.28
    }

    /// Evaluate the network at a column. `macro_h` is the *smooth* base
    /// elevation from the terrain generator (no ridges, no hills), which
    /// is what keeps a deck level while the ground under it heaves.
    pub fn column(&self, wx: i32, wz: i32, macro_h: f64) -> Option<SkywayColumn> {
        if let Some(hero) = hero_skyway_column(wx, wz, macro_h) {
            return Some(hero);
        }
        if let Some(spur) = hero_skyway_spur(wx, wz, macro_h) {
            return Some(spur);
        }
        if let Some(rail) = hero_mesa_rail(wx, wz, macro_h) {
            return Some(rail);
        }
        if let Some(walk) = hero_cliff_walk(wx, wz, macro_h) {
            return Some(walk);
        }
        if let Some(spur) = hero_terrace_spur(wx, wz, macro_h) {
            return Some(spur);
        }
        if let Some(face) = hero_face_rail(wx, wz, macro_h) {
            return Some(face);
        }
        if let Some(west) = hero_west_face_rail(wx, wz, macro_h) {
            return Some(west);
        }
        if let Some(look_west) = hero_look_west_face_rail(wx, wz, macro_h) {
            return Some(look_west);
        }
        let x = wx as f64;
        let z = wz as f64;
        let a = self.route_field(x, z);
        // The field's gradient is on the order of 1e-3 per block, so a
        // road half-width of ~5 blocks lives inside |a| < 0.01. Bailing
        // at 0.05 keeps 19 out of 20 columns down to a single noise
        // sample while never clipping a real road edge.
        if a.abs() > 0.05 {
            return None;
        }
        let e = 3.0;
        let gx = (self.route_field(x + e, z) - self.route_field(x - e, z)) / (2.0 * e);
        let gz = (self.route_field(x, z + e) - self.route_field(x, z - e)) / (2.0 * e);
        let grad = (gx * gx + gz * gz).sqrt();
        if grad < 1e-9 {
            return None;
        }
        // |value| / |gradient| is the first-order distance to the
        // contour, which is what makes the carriageway a constant width
        // instead of ballooning wherever the field flattens out.
        let dist = a.abs() / grad;
        if dist > SKYWAY_HALF_WIDTH {
            return None;
        }

        let wobble = self.deck.get([x * 0.0013, z * 0.0013]) * 9.0;
        let deck_y = (macro_h + 27.0 + wobble).round() as i32;
        let pylon = wx.rem_euclid(SKYWAY_PYLON_PITCH) < 3
            && wz.rem_euclid(SKYWAY_PYLON_PITCH) < 3
            && dist < SKYWAY_HALF_WIDTH - 1.0;

        Some(SkywayColumn {
            deck_y,
            dist,
            pylon,
            half: SKYWAY_HALF_WIDTH,
        })
    }

    /// Headroom kept clear above a deck.
    pub const CLEARANCE: i32 = SKYWAY_CLEARANCE;
}

/// Should a guardrail post at this column carry a lamp?
#[inline]
pub fn skyway_lamp(wx: i32, wz: i32) -> bool {
    (wx + wz).rem_euclid(7) == 0
}

// ---------------------------------------------------------------------------
// Energy rivers -------------------------------------------------------------
// ---------------------------------------------------------------------------

/// A glowing river carved into the terrain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RiverColumn {
    /// Depth to cut below the natural surface, in blocks.
    pub cut: i32,
    /// Distance from the channel centreline.
    pub dist: f64,
    /// Fluid that pools in the channel.
    pub fluid: BlockType,
}

impl RiverColumn {
    /// Top of the standing fluid, given the carved bed height.
    ///
    /// The level is measured down from the *natural* surface rather than
    /// up from the bed, so the fluid surface is flat right across the
    /// channel. Filling a fixed depth above the bed instead would give a
    /// parabolic river that climbs its own banks.
    #[inline]
    pub fn fluid_top(&self, bed: i32) -> i32 {
        bed + self.cut - RIVER_BANK
    }
}

/// The energy-river network.
///
/// Two independent contour fields, not one: the key art has an orange
/// lava river and a blue plasma river threading the same canyon system,
/// and a single field with a hot/cold mask can only ever give one colour
/// per region.
pub struct RiverNetwork {
    lava: Perlin,
    plasma: Perlin,
}

/// Half-width of an energy channel, in blocks.
const RIVER_HALF_WIDTH: f64 = 6.0;
/// Deepest cut at the channel centreline.
const RIVER_DEPTH: i32 = 8;
/// How far the fluid surface sits below the natural ground line. Small
/// enough that the glow spills out over the banks and lights the canyon.
const RIVER_BANK: i32 = 2;

impl RiverNetwork {
    pub fn new(seed: u32) -> Self {
        Self {
            lava: Perlin::new(seed.wrapping_add(43)),
            plasma: Perlin::new(seed.wrapping_add(44)),
        }
    }

    #[inline]
    fn field(source: &Perlin, wx: f64, wz: f64, phase: f64) -> f64 {
        source.get([wx * 0.0011 + phase, wz * 0.0011 - phase]) * 0.70
            + source.get([wx * 0.0027 - phase, wz * 0.0027 + phase]) * 0.30
    }

    fn channel(source: &Perlin, wx: i32, wz: i32, phase: f64) -> Option<f64> {
        let x = wx as f64;
        let z = wz as f64;
        let a = Self::field(source, x, z, phase);
        if a.abs() > 0.06 {
            return None;
        }
        let e = 3.0;
        let gx = (Self::field(source, x + e, z, phase) - Self::field(source, x - e, z, phase))
            / (2.0 * e);
        let gz = (Self::field(source, x, z + e, phase) - Self::field(source, x, z - e, phase))
            / (2.0 * e);
        let grad = (gx * gx + gz * gz).sqrt();
        if grad < 1e-9 {
            return None;
        }
        let dist = a.abs() / grad;
        (dist <= RIVER_HALF_WIDTH).then_some(dist)
    }

    pub fn column(&self, wx: i32, wz: i32) -> Option<RiverColumn> {
        if let Some(hero) = hero_river_column(wx, wz) {
            return Some(hero);
        }
        // Where the two networks cross, the hotter one wins the bed.
        let lava = Self::channel(&self.lava, wx, wz, 77.1);
        let plasma = Self::channel(&self.plasma, wx, wz, -21.7);
        let (dist, fluid) = match (lava, plasma) {
            (Some(l), Some(p)) if l <= p => (l, BlockType::Lava),
            (Some(_), Some(p)) => (p, BlockType::PlasmaFlow),
            (Some(l), None) => (l, BlockType::Lava),
            (None, Some(p)) => (p, BlockType::PlasmaFlow),
            (None, None) => return None,
        };

        // Parabolic channel profile: deepest in the middle, feathering to
        // nothing at the banks so the cut never leaves a vertical wall.
        let t = dist / RIVER_HALF_WIDTH;
        let cut = ((1.0 - t * t) * RIVER_DEPTH as f64).round() as i32;
        if cut <= 0 {
            return None;
        }
        Some(RiverColumn { cut, dist, fluid })
    }
}

/// Forced energy-river spur for the spawn postcard. A sine-wobble
/// channel of lava/plasma along z ≈ HERO_RIVER_Z, starting east of
/// origin so `find_natural_spawn(0,0)` never lands in it.
fn hero_river_column(wx: i32, wz: i32) -> Option<RiverColumn> {
    if wx < HERO_RIVER_X0 || wx > HERO_RIVER_X1 {
        return None;
    }
    let centre_z = HERO_RIVER_Z + ((wx as f64 - 80.0) * 0.10 + (wx as f64 * 0.07).sin() * 6.0) as i32;
    let dist = (wz - centre_z).abs() as f64;
    if dist > RIVER_HALF_WIDTH {
        return None;
    }
    let t = dist / RIVER_HALF_WIDTH;
    let cut = ((1.0 - t * t) * RIVER_DEPTH as f64).round() as i32;
    if cut <= 0 {
        return None;
    }
    let fluid = if wx.rem_euclid(42) < 18 {
        BlockType::Lava
    } else {
        BlockType::PlasmaFlow
    };
    Some(RiverColumn { cut, dist, fluid })
}

/// Forced skyway span for the spawn postcard: a straight carriageway
/// along z = HERO_SKYWAY_Z so the first look has winding infrastructure.
fn hero_skyway_column(wx: i32, wz: i32, macro_h: f64) -> Option<SkywayColumn> {
    if wx < HERO_SKYWAY_X0 || wx > HERO_SKYWAY_X1 {
        return None;
    }
    let dist = (wz - HERO_SKYWAY_Z).abs() as f64;
    if dist > SKYWAY_HALF_WIDTH {
        return None;
    }
    let deck_y = (macro_h + 24.0).round() as i32;
    let pylon = wx.rem_euclid(SKYWAY_PYLON_PITCH) < 3 && dist < SKYWAY_HALF_WIDTH - 1.0;
    Some(SkywayColumn {
        deck_y,
        dist,
        pylon,
        half: SKYWAY_HALF_WIDTH,
    })
}

/// North–south spur so the postcard has a crossing instead of a single
/// lonely east–west deck.
fn hero_skyway_spur(wx: i32, wz: i32, macro_h: f64) -> Option<SkywayColumn> {
    const SPUR_X: i32 = 96;
    if wz < -120 || wz > -18 {
        return None;
    }
    let dist = (wx - SPUR_X).abs() as f64;
    if dist > SKYWAY_HALF_WIDTH {
        return None;
    }
    let deck_y = (macro_h + 24.0).round() as i32;
    let pylon = wz.rem_euclid(SKYWAY_PYLON_PITCH) < 3 && dist < SKYWAY_HALF_WIDTH - 1.0;
    Some(SkywayColumn {
        deck_y,
        dist,
        pylon,
        half: SKYWAY_HALF_WIDTH,
    })
}

/// Low mesa-edge rail: a narrower ribbon sitting closer to the cliff
/// so the postcard reads as a colony with stacked transit, not one
/// lonely skyway. Postcard AABB only.
fn hero_mesa_rail(wx: i32, wz: i32, macro_h: f64) -> Option<SkywayColumn> {
    const RAIL_Z: i32 = -88;
    const HALF: f64 = 2.4;
    if wx < 16 || wx > 172 {
        return None;
    }
    let dist = (wz - RAIL_Z).abs() as f64;
    if dist > HALF {
        return None;
    }
    let deck_y = (macro_h + 10.0).round() as i32;
    let pylon = wx.rem_euclid(SKYWAY_PYLON_PITCH) < 2 && dist < HALF - 0.6;
    Some(SkywayColumn {
        deck_y,
        dist,
        pylon,
        half: HALF,
    })
}

/// Lower cliff walk hugging the mesa rim, still postcard-AABB only.
fn hero_cliff_walk(wx: i32, wz: i32, macro_h: f64) -> Option<SkywayColumn> {
    const RAIL_Z: i32 = -112;
    const HALF: f64 = 2.0;
    if wx < 32 || wx > 156 {
        return None;
    }
    let dist = (wz - RAIL_Z).abs() as f64;
    if dist > HALF {
        return None;
    }
    let deck_y = (macro_h + 7.0).round() as i32;
    let pylon = wx.rem_euclid(SKYWAY_PYLON_PITCH) < 2 && dist < HALF - 0.5;
    Some(SkywayColumn {
        deck_y,
        dist,
        pylon,
        half: HALF,
    })
}

/// Short N–S terrace deck that ties the stacked habs together.
fn hero_terrace_spur(wx: i32, wz: i32, macro_h: f64) -> Option<SkywayColumn> {
    const SPUR_X: i32 = 118;
    const HALF: f64 = 2.0;
    if wz < -100 || wz > -48 {
        return None;
    }
    let dist = (wx - SPUR_X).abs() as f64;
    if dist > HALF {
        return None;
    }
    let deck_y = (macro_h + 14.0).round() as i32;
    let pylon = wz.rem_euclid(SKYWAY_PYLON_PITCH) < 2 && dist < HALF - 0.5;
    Some(SkywayColumn {
        deck_y,
        dist,
        pylon,
        half: HALF,
    })
}

/// Lower face rail stepping down the mesa wall so habs hanging on the
/// cliff stay connected to the mesa-top colony.
fn hero_face_rail(wx: i32, wz: i32, macro_h: f64) -> Option<SkywayColumn> {
    const RAIL_Z: i32 = -130;
    const HALF: f64 = 2.0;
    if wx < 48 || wx > 160 {
        return None;
    }
    let dist = (wz - RAIL_Z).abs() as f64;
    if dist > HALF {
        return None;
    }
    let deck_y = (macro_h - 2.0).round() as i32;
    let pylon = wx.rem_euclid(SKYWAY_PYLON_PITCH) < 2 && dist < HALF - 0.5;
    Some(SkywayColumn {
        deck_y,
        dist,
        pylon,
        half: HALF,
    })
}

/// Camera-facing west rail so terraced habs on the look-cone cliff stay
/// tied to the mesa-top colony. Postcard AABB only.
fn hero_west_face_rail(wx: i32, wz: i32, macro_h: f64) -> Option<SkywayColumn> {
    const RAIL_X: i32 = 32;
    const HALF: f64 = 2.0;
    if wz < -112 || wz > -48 {
        return None;
    }
    let dist = (wx - RAIL_X).abs() as f64;
    if dist > HALF {
        return None;
    }
    let deck_y = (macro_h - 6.0).round() as i32;
    let pylon = wz.rem_euclid(SKYWAY_PYLON_PITCH) < 2 && dist < HALF - 0.5;
    Some(SkywayColumn {
        deck_y,
        dist,
        pylon,
        half: HALF,
    })
}

/// West-face walk on the mesa the settled camera actually sees
/// (rest ~x=90 looking +X). Postcard AABB only.
fn hero_look_west_face_rail(wx: i32, wz: i32, macro_h: f64) -> Option<SkywayColumn> {
    const RAIL_X: i32 = 108;
    const HALF: f64 = 2.2;
    if wz < -128 || wz > -56 {
        return None;
    }
    let dist = (wx - RAIL_X).abs() as f64;
    if dist > HALF {
        return None;
    }
    let deck_y = (macro_h - 4.0).round() as i32;
    let pylon = wz.rem_euclid(SKYWAY_PYLON_PITCH) < 2 && dist < HALF - 0.5;
    Some(SkywayColumn {
        deck_y,
        dist,
        pylon,
        half: HALF,
    })
}

// ---------------------------------------------------------------------------
// Cliff colony (spawn postcard) ---------------------------------------------
// ---------------------------------------------------------------------------

/// A small stacked hab on the mesa: plated walls, holo windows, a neon
/// roof rim. Bounded to the spawn AABB so Fast-mode streaming is unchanged
/// outside the postcard.
#[derive(Debug, Clone, Copy)]
pub struct CliffHab {
    pub cx: i32,
    pub cz: i32,
    pub floors: i32,
    pub width: i32,
    pub depth: i32,
    /// Blocks below the height sample. 0 sits on the mesa; >0 hangs the
    /// hab on the cliff face.
    pub drop: i32,
    /// Extra blocks EAST of `cx` used as the height sample and as a
    /// westward deck so the hab sits on a ledge in front of the mesa
    /// instead of inside the cap. 0 = no ledge (mesa-top cluster).
    pub ledge: i32,
}

impl CliffHab {
    pub fn hero_cluster() -> [Self; 22] {
        [
            Self { cx: 38, cz: -58, floors: 5, width: 5, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 56, cz: -70, floors: 7, width: 4, depth: 5, drop: 0, ledge: 0 },
            Self { cx: 84, cz: -54, floors: 6, width: 6, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 22, cz: -82, floors: 6, width: 4, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 108, cz: -66, floors: 8, width: 5, depth: 3, drop: 0, ledge: 0 },
            Self { cx: 70, cz: -40, floors: 4, width: 7, depth: 5, drop: 0, ledge: 0 },
            Self { cx: 128, cz: -92, floors: 7, width: 5, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 146, cz: -74, floors: 9, width: 4, depth: 5, drop: 0, ledge: 0 },
            Self { cx: 96, cz: -108, floors: 6, width: 6, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 168, cz: -88, floors: 8, width: 5, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 118, cz: -58, floors: 5, width: 6, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 138, cz: -108, floors: 8, width: 4, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 158, cz: -64, floors: 6, width: 5, depth: 5, drop: 0, ledge: 0 },
            Self { cx: 112, cz: -84, floors: 10, width: 4, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 78, cz: -96, floors: 5, width: 7, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 48, cz: -98, floors: 7, width: 4, depth: 5, drop: 0, ledge: 0 },
            Self { cx: 64, cz: -52, floors: 3, width: 8, depth: 6, drop: 0, ledge: 0 },
            Self { cx: 100, cz: -48, floors: 4, width: 5, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 124, cz: -76, floors: 6, width: 5, depth: 3, drop: 0, ledge: 0 },
            Self { cx: 152, cz: -96, floors: 5, width: 6, depth: 4, drop: 0, ledge: 0 },
            Self { cx: 90, cz: -80, floors: 3, width: 9, depth: 5, drop: 0, ledge: 0 },
            Self { cx: 174, cz: -70, floors: 7, width: 4, depth: 4, drop: 0, ledge: 0 },
        ]
    }

    pub fn stamp(&self, chunk: &mut Chunk, ground: impl Fn(i32, i32) -> i32) {
        let origin = chunk.pos.origin();
        let (ox, oy, oz) = origin;
        let hw = self.width / 2;
        let hd = self.depth / 2;
        let pad = 2;
        let east = self.cx + self.ledge.max(hw) + pad + 1;
        let west = self.cx - hw - pad - 1;
        let north = self.cz - hd - pad - 1;
        let south = self.cz + hd + pad + 1;
        if (ox + CHUNK_SIZE_I <= west)
            || (ox > east)
            || (oz + CHUNK_SIZE_I <= north)
            || (oz > south)
        {
            return;
        }
        let sample_x = self.cx + self.ledge;
        let base = ground(sample_x, self.cz) - self.drop;
        let top = base + self.floors * 3 + 2;
        if oy > top || oy + CHUNK_SIZE_I <= base {
            return;
        }
        for dz in -hd..=hd {
            for dx in -hw..=hw {
                let wx = self.cx + dx;
                let wz = self.cz + dz;
                let edge = dx.abs() == hw || dz.abs() == hd;
                let corner = dx.abs() == hw && dz.abs() == hd;
                for floor in 0..self.floors {
                    let wy0 = base + floor * 3;
                    for ly in 0..3 {
                        let wy = wy0 + ly;
                        if !edge && ly > 0 {
                            continue;
                        }
                        let block = if ly == 1 && edge && !corner && (floor + dx + dz).rem_euclid(3) != 0
                        {
                            BlockType::HoloPanel
                        } else if ly == 2 && edge {
                            BlockType::PlatingTeal
                        } else if corner {
                            BlockType::PlatingWhite
                        } else {
                            BlockType::PlatingWhite
                        };
                        place_over(chunk, origin, wx, wy, wz, block);
                    }
                }
                let roof = base + self.floors * 3;
                place_over(
                    chunk,
                    origin,
                    wx,
                    roof,
                    wz,
                    if edge {
                        BlockType::NeonAmber
                    } else {
                        BlockType::PlatingTeal
                    },
                );
                if dx == 0 && dz == 0 {
                    place_over(chunk, origin, wx, roof + 1, wz, BlockType::HoloPanel);
                    place_over(chunk, origin, wx, roof + 2, wz, BlockType::PlatingWhite);
                }
            }
        }
        // One-floor plated terrace so towers read as stacked cliff
        // buildings, not floating boxes on bare mesa.
        let pad_w = hw + pad;
        let pad_d = hd + pad;
        for dz in -pad_d..=pad_d {
            for dx in -pad_w..=pad_w {
                if dx.abs() <= hw && dz.abs() <= hd {
                    continue;
                }
                let wx = self.cx + dx;
                let wz = self.cz + dz;
                let outer = dx.abs() == pad_w || dz.abs() == pad_d;
                place_over(chunk, origin, wx, base, wz, BlockType::PlatingTeal);
                place_over(
                    chunk,
                    origin,
                    wx,
                    base + 1,
                    wz,
                    if outer {
                        BlockType::PlatingWhite
                    } else {
                        BlockType::RoadDeck
                    },
                );
                if outer && (dx + dz).rem_euclid(3) == 0 {
                    place_over(chunk, origin, wx, base + 2, wz, BlockType::NeonAmber);
                }
            }
        }
        // Westward cliff deck so hanging habs sit in front of the mesa
        // face the postcard camera actually sees after the fly-in.
        if self.ledge > 0 {
            let deck_half = hd + 1;
            for dx in 1..=self.ledge {
                for dz in -deck_half..=deck_half {
                    let wx = self.cx + dx;
                    let wz = self.cz + dz;
                    let edge = dz.abs() == deck_half || dx == 1 || dx == self.ledge;
                    place_over(chunk, origin, wx, base, wz, BlockType::PlatingTeal);
                    place_over(
                        chunk,
                        origin,
                        wx,
                        base + 1,
                        wz,
                        if edge {
                            BlockType::PlatingWhite
                        } else {
                            BlockType::RoadDeck
                        },
                    );
                    if edge && (dx + dz).rem_euclid(2) == 0 {
                        place_over(chunk, origin, wx, base + 2, wz, BlockType::NeonAmber);
                    }
                }
            }
        }
    }
}

/// West-facing terraces cut *into* the mesa wall the settled postcard
/// actually sees. Camera rests at ~(90, 110, −44) looking +X/−Z, so the
/// lip sits around x=100–116 — a notch in the rock with a floor, lit
/// back-wall windows, a rail, and short stairs, not a floating box.
#[derive(Debug, Clone, Copy)]
pub struct CliffFace {
    pub face_x: i32,
    pub z0: i32,
    pub z1: i32,
    pub levels: i32,
    pub depth: i32,
    pub drop: i32,
    pub rise: i32,
    /// Blocks WEST of the lip to excavate so a flat mesa still presents
    /// a west-facing wall to the camera at ~x=90. 0 = no apron.
    pub apron: i32,
}

impl CliffFace {
    pub fn look_cone() -> [Self; 2] {
        [
            Self {
                face_x: 102,
                z0: -124,
                z1: -60,
                levels: 5,
                depth: 9,
                drop: 6,
                rise: 6,
                apron: 14,
            },
            Self {
                face_x: 118,
                z0: -108,
                z1: -72,
                levels: 3,
                depth: 6,
                drop: 18,
                rise: 5,
                apron: 0,
            },
        ]
    }

    pub fn stamp(&self, chunk: &mut Chunk, ground: impl Fn(i32, i32) -> i32) {
        let origin = chunk.pos.origin();
        let (ox, oy, oz) = origin;
        let west = self.face_x - self.apron.max(1);
        let east = self.face_x + self.depth + 3;
        if (ox + CHUNK_SIZE_I <= west) || (ox > east) {
            return;
        }
        if (oz + CHUNK_SIZE_I <= self.z0) || (oz > self.z1) {
            return;
        }
        let mut y_lo = i32::MAX;
        let mut y_hi = i32::MIN;
        for z in self.z0..=self.z1 {
            let rim = ground(self.face_x + self.depth, z);
            let bottom = rim - self.drop - (self.levels - 1) * self.rise - 4;
            y_lo = y_lo.min(bottom);
            y_hi = y_hi.max(rim + 1);
        }
        if oy > y_hi || oy + CHUNK_SIZE_I <= y_lo {
            return;
        }

        for z in self.z0..=self.z1 {
            let rim = ground(self.face_x + self.depth, z);
            let pit = rim - self.drop - (self.levels - 1) * self.rise - 3;
            for dx in 1..=self.apron {
                let wx = self.face_x - dx;
                for wy in pit..=rim {
                    carve(chunk, origin, wx, wy, z);
                }
            }
            let stair = (z - self.z0).rem_euclid(12) == 0;
            let dwelling = (z - self.z0).rem_euclid(10) == 4;
            for level in 0..self.levels {
                let floor = rim - self.drop - level * self.rise;
                let head = floor + self.rise - 1;
                let bite = if dwelling {
                    self.depth + 3
                } else {
                    self.depth
                };
                for dx in 0..bite {
                    let wx = self.face_x + dx;
                    place_over_unless_glow(
                        chunk,
                        origin,
                        wx,
                        floor,
                        z,
                        if dx <= 1 {
                            BlockType::PlatingTeal
                        } else {
                            BlockType::RoadDeck
                        },
                    );
                    for wy in (floor + 1)..=head {
                        carve(chunk, origin, wx, wy, z);
                    }
                }
                let back = self.face_x + bite;
                if (floor + 2 - z).rem_euclid(3) != 1 {
                    place_over_unless_glow(
                        chunk,
                        origin,
                        back,
                        floor + 2,
                        z,
                        BlockType::HoloPanel,
                    );
                    place_over_unless_glow(
                        chunk,
                        origin,
                        back,
                        floor + 1,
                        z,
                        BlockType::PlatingWhite,
                    );
                }
                if z.rem_euclid(2) == 0 {
                    place_over_unless_glow(
                        chunk,
                        origin,
                        self.face_x,
                        floor + 1,
                        z,
                        BlockType::NeonAmber,
                    );
                }
                if stair {
                    for step in 0..self.rise {
                        let wx = self.face_x + 1 + (step / 2).min(self.depth - 2);
                        place_over_unless_glow(
                            chunk,
                            origin,
                            wx,
                            floor + step,
                            z,
                            BlockType::RoadDeck,
                        );
                        carve(chunk, origin, wx, floor + step + 1, z);
                        carve(chunk, origin, wx, floor + step + 2, z);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------

/// Convenience bundle so `terrain.rs` builds the noise fields once.
pub struct FrontierPlanner {
    pub seed: u32,
    pub skyways: SkywayNetwork,
    pub rivers: RiverNetwork,
}

impl FrontierPlanner {
    pub fn new(seed: u32) -> Self {
        Self {
            seed,
            skyways: SkywayNetwork::new(seed),
            rivers: RiverNetwork::new(seed),
        }
    }

    /// Stamp every lattice-anchored feature that reaches this chunk.
    ///
    /// `ground` is the true surface height (crystal clusters must be
    /// rooted in it); `macro_ground` is the smooth base elevation, which
    /// is an order of magnitude cheaper to sample and is all the airborne
    /// features need. Both are only called for features that survive the
    /// altitude prune, so a chunk deep underground pays for neither.
    pub fn stamp_landmarks(
        &self,
        chunk: &mut Chunk,
        ground: impl Fn(i32, i32) -> i32 + Copy,
        macro_ground: impl Fn(i32, i32) -> i32 + Copy,
    ) {
        let ChunkPos { x: cx, z: cz, .. } = chunk.pos;
        let wx = cx * CHUNK_SIZE_I + CHUNK_SIZE_I / 2;
        let wz = cz * CHUNK_SIZE_I + CHUNK_SIZE_I / 2;
        let base_y = chunk.pos.y * CHUNK_SIZE_I;
        let top_y = base_y + CHUNK_SIZE_I - 1;

        for cluster in CrystalCluster::near(self.seed, wx, wz) {
            cluster.stamp(self.seed, chunk, ground);
        }
        CrystalCluster::hero().stamp(self.seed, chunk, ground);
        CrystalCluster::hero_b().stamp(self.seed, chunk, ground);
        for hab in CliffHab::hero_cluster() {
            hab.stamp(chunk, ground);
        }
        for face in CliffFace::look_cone() {
            face.stamp(chunk, ground);
        }
        // Widest island root reaches ~1.6 radii below the core.
        if top_y >= SKY_ISLAND_MIN_Y - 48 && base_y <= SKY_ISLAND_MAX_Y + 12 {
            for island in SkyIsland::near(self.seed, wx, wz, macro_ground) {
                island.stamp(self.seed, chunk);
            }
        }
        if top_y >= STATION_MIN_Y - 6 && base_y <= STATION_MAX_Y + 28 {
            for station in SkyStation::near(self.seed, wx, wz, macro_ground) {
                station.stamp(chunk);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat test ground so feature geometry is the only variable.
    fn flat(_x: i32, _z: i32) -> i32 {
        64
    }

    fn count_blocks(chunk: &Chunk, block: BlockType) -> usize {
        let target: crate::blocks::Voxel = block.into();
        let mut n = 0;
        for ly in 0..CHUNK_SIZE {
            for lz in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    if chunk.get(lx, ly, lz) == target {
                        n += 1;
                    }
                }
            }
        }
        n
    }

    #[test]
    fn strata_bands_are_horizontal_and_span_the_frontier_palette() {
        // Every column must agree on the band at a given Y, otherwise
        // cliff faces would dissolve into noise instead of stripes.
        for wy in -40..300 {
            assert_eq!(strata_block(wy), strata_block(wy));
        }
        let mut seen = std::collections::BTreeSet::new();
        for wy in 0..34 {
            seen.insert(strata_block(wy) as u16);
        }
        for expected in [
            BlockType::VioletStone,
            BlockType::AmberStone,
            BlockType::RedStone,
            BlockType::MesaClay,
            BlockType::RedSand,
        ] {
            assert!(
                seen.contains(&(expected as u16)),
                "strata cycle is missing {expected:?}"
            );
        }
    }

    #[test]
    fn sky_islands_float_clear_of_the_ground_below_them() {
        let mut found = 0;
        for ix in -6..6 {
            for iz in -6..6 {
                let Some(island) = SkyIsland::for_cell(4242, ix, iz, flat) else {
                    continue;
                };
                found += 1;
                let (bottom, top) = island
                    .column(island.cx, island.cz)
                    .expect("the centre column of an island always has material");
                assert!(top > bottom);
                assert!(
                    bottom > flat(island.cx, island.cz) + 12,
                    "radius-{} island root at {bottom} would fuse with ground at {}",
                    island.radius,
                    flat(island.cx, island.cz)
                );
                assert!(
                    (SKY_ISLAND_MIN_Y..=SKY_ISLAND_MAX_Y).contains(&island.cy),
                    "island core at {} escaped the streamed altitude band",
                    island.cy
                );
            }
        }
        assert!(found > 20, "only rolled {found} islands across 144 cells");
    }

    #[test]
    fn sky_island_outline_is_bounded_and_closed() {
        let island = SkyIsland {
            cx: 0,
            cz: 0,
            cy: 160,
            radius: 20,
            phase: 1.1,
        };
        // Nothing beyond the wobbled maximum radius.
        for d in 0..64 {
            let inside = island.column(d, 0).is_some();
            if d > (island.radius as f32 * 1.30) as i32 {
                assert!(
                    !inside,
                    "island material {d} blocks out from a radius-20 core"
                );
            }
        }
        assert!(island.column(0, 0).is_some());
    }

    #[test]
    fn sky_island_column_geometry_is_identical_from_either_neighbouring_chunk() {
        // The whole point of lattice anchoring: a straddling island must
        // not depend on which chunk asked for it.
        let ground = |_x: i32, _z: i32| 70;
        let a = SkyIsland::near(9001, 0, 0, ground);
        let b = SkyIsland::near(9001, CHUNK_SIZE_I, 0, ground);
        for island in &a {
            if let Some(same) = b.iter().find(|o| o.cx == island.cx && o.cz == island.cz) {
                assert_eq!(island, same);
            }
        }
    }

    #[test]
    fn skyway_decks_stay_level_while_the_ground_heaves() {
        let net = SkywayNetwork::new(77);
        let mut samples = 0;
        for wx in -3000..3000 {
            // A deck computed from the same macro height must not change
            // just because the local terrain does.
            if let Some(col) = net.column(wx, 0, 80.0) {
                let again = net.column(wx, 0, 80.0).unwrap();
                assert_eq!(col.deck_y, again.deck_y);
                assert!(col.dist <= SKYWAY_HALF_WIDTH);
                samples += 1;
            }
        }
        assert!(samples > 20, "route never crossed the sample line");
    }

    #[test]
    fn skyways_stay_a_thin_ribbon_across_the_whole_map() {
        // Gradient normalisation is what stops the contour from smearing
        // into a plaza wherever the route field flattens out. The tell is
        // area: a ribbon covers a percent or two of the world, a smear
        // covers a third of it.
        let net = SkywayNetwork::new(1234);
        let mut road = 0usize;
        let mut total = 0usize;
        for wz in (-2000..2000).step_by(7) {
            for wx in (-2000..2000).step_by(3) {
                total += 1;
                if net.column(wx, wz, 80.0).is_some() {
                    road += 1;
                }
            }
        }
        let coverage = road as f64 / total as f64;
        assert!(coverage > 0.0005, "no skyway anywhere in a 4 km square");
        assert!(
            coverage < 0.08,
            "skyways cover {:.1}% of the world — the gradient normalisation is not holding",
            coverage * 100.0
        );
    }

    #[test]
    fn skyway_cross_section_has_deck_rails_and_markings() {
        let col = SkywayColumn {
            deck_y: 100,
            dist: 0.0,
            pylon: false,
            half: SKYWAY_HALF_WIDTH,
        };
        assert_eq!(col.deck_block(100, false), Some(BlockType::RoadMarking));
        assert_eq!(col.deck_block(99, false), Some(BlockType::PlatingWhite));
        assert_eq!(col.deck_block(101, false), None);

        let edge = SkywayColumn {
            deck_y: 100,
            dist: SKYWAY_HALF_WIDTH - 0.1,
            pylon: false,
            half: SKYWAY_HALF_WIDTH,
        };
        assert_eq!(edge.deck_block(101, false), Some(BlockType::PlatingWhite));
        assert_eq!(edge.deck_block(102, true), Some(BlockType::NeonAmber));
        assert_eq!(edge.deck_block(102, false), None);
    }

    #[test]
    fn energy_rivers_cut_a_feathered_channel_with_glowing_fluid() {
        let net = RiverNetwork::new(555);
        let mut deepest = 0;
        let mut fluids = std::collections::BTreeSet::new();
        for wz in (-6000..6000).step_by(97) {
            for wx in (-6000..6000).step_by(3) {
                if let Some(col) = net.column(wx, wz) {
                    assert!(col.cut > 0 && col.cut <= RIVER_DEPTH);
                    assert!(col.dist <= RIVER_HALF_WIDTH);
                    deepest = deepest.max(col.cut);
                    fluids.insert(col.fluid as u16);
                }
            }
        }
        assert!(
            deepest >= 4,
            "rivers never cut deeper than {deepest} blocks"
        );
        // Both networks must show up: the key art has an orange river and
        // a blue one threading the same canyon country.
        assert!(
            fluids.contains(&(BlockType::PlasmaFlow as u16)),
            "no plasma river anywhere in a 12 km square"
        );
        assert!(
            fluids.contains(&(BlockType::Lava as u16)),
            "no lava river anywhere in a 12 km square"
        );
    }

    #[test]
    fn river_fluid_surface_is_flat_across_the_channel() {
        // Filling a fixed depth above the bed would give a parabolic
        // river climbing its own banks. The fluid top must instead be
        // level, and must stay below the natural ground line.
        let net = RiverNetwork::new(555);
        let natural = 90;
        let mut tops = std::collections::BTreeSet::new();
        let mut found = 0;
        for wx in -6000..6000 {
            let Some(col) = net.column(wx, 128) else {
                continue;
            };
            let bed = natural - col.cut;
            let top = col.fluid_top(bed);
            assert!(
                top < natural,
                "fluid at {top} overflows ground at {natural}"
            );
            if col.cut > RIVER_BANK {
                assert!(top > bed, "channel centre at {bed} holds no fluid");
                tops.insert(top);
                found += 1;
            }
        }
        assert!(found > 50, "only {found} deep river columns sampled");
        assert_eq!(
            tops.len(),
            1,
            "fluid surface is not level across the channel: {tops:?}"
        );
    }

    #[test]
    fn stations_build_a_deck_a_tower_and_a_lit_rim() {
        let station = SkyStation {
            cx: 8,
            cz: 8,
            cy: 160,
            radius: 9,
            tower: 8,
        };
        // Deck slice.
        let mut deck = Chunk::new(ChunkPos::new(0, 10, 0));
        station.stamp(&mut deck);
        assert!(count_blocks(&deck, BlockType::PlatingWhite) > 40);
        assert!(count_blocks(&deck, BlockType::PlatingTeal) > 8);
        assert!(count_blocks(&deck, BlockType::NeonCyan) > 0);
    }

    #[test]
    fn hero_postcard_is_always_near_origin() {
        let net = RiverNetwork::new(12345);
        let sky = SkywayNetwork::new(12345);
        let mut river = 0;
        let mut way = 0;
        for x in 40..180 {
            for z in -120..-20 {
                if net.column(x, z).is_some() {
                    river += 1;
                }
                if sky.column(x, z, 80.0).is_some() {
                    way += 1;
                }
            }
        }
        assert!(river > 40, "hero energy river missing near spawn ({river} columns)");
        assert!(way > 40, "hero skyway missing near spawn ({way} columns)");
        let hero = CrystalCluster::hero();
        assert_eq!(hero.cx, HERO_CRYSTAL_X);
        assert_eq!(hero.cz, HERO_CRYSTAL_Z);
        assert!(hero.shards >= 5);
    }

    #[test]
    fn cliff_colony_stamps_lit_windows_on_the_postcard() {
        let hab = CliffHab {
            cx: 8,
            cz: 8,
            floors: 4,
            width: 5,
            depth: 4,
            drop: 0,
            ledge: 0,
        };
        let mut chunk = Chunk::new(ChunkPos::new(0, 4, 0));
        hab.stamp(&mut chunk, |_, _| 64);
        assert!(
            count_blocks(&chunk, BlockType::HoloPanel) > 4,
            "cliff hab has no holo windows"
        );
        assert!(count_blocks(&chunk, BlockType::PlatingWhite) > 10);
        assert!(count_blocks(&chunk, BlockType::NeonAmber) > 0);
        assert_eq!(CliffHab::hero_cluster().len(), 22);
        for h in CliffHab::hero_cluster() {
            assert!(in_hero_postcard(h.cx, h.cz), "hab {},{} left the postcard", h.cx, h.cz);
            assert!(
                in_hero_postcard(h.cx + h.ledge, h.cz),
                "hab ledge {},{} left the postcard",
                h.cx + h.ledge,
                h.cz
            );
        }
        for face in CliffFace::look_cone() {
            assert!(
                in_hero_postcard(face.face_x - face.apron, face.z0)
                    && in_hero_postcard(face.face_x + face.depth, face.z1),
                "cliff face {},{}..{} left the postcard",
                face.face_x,
                face.z0,
                face.z1
            );
            assert!(face.levels >= 3, "need stacked terraces, got {}", face.levels);
            assert!(face.face_x >= 96 && face.face_x <= 120, "face_x {} is not in the look cone", face.face_x);
        }
        let mut terrace = Chunk::new(ChunkPos::new(6, 3, -6));
        for ly in 0..CHUNK_SIZE {
            for lz in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    terrace.set(lx, ly, lz, BlockType::RedStone.into());
                }
            }
        }
        CliffFace::look_cone()[0].stamp(&mut terrace, |_, _| 64);
        assert!(
            count_blocks(&terrace, BlockType::HoloPanel) > 4,
            "carved terrace has no lit windows"
        );
        assert!(
            count_blocks(&terrace, BlockType::NeonAmber) > 0,
            "carved terrace has no rail lights"
        );
        assert!(
            count_blocks(&terrace, BlockType::RoadDeck) > 8,
            "carved terrace has no floor"
        );
        assert!(in_hero_postcard(CrystalCluster::hero_b().cx, CrystalCluster::hero_b().cz));
        let sky = SkywayNetwork::new(1);
        let rail = sky.column(80, -88, 70.0).expect("mesa rail missing on postcard");
        assert!(rail.half < SKYWAY_HALF_WIDTH, "mesa rail should be a thin ribbon");
        let walk = sky.column(80, -112, 70.0).expect("cliff walk missing on postcard");
        assert!(walk.half < SKYWAY_HALF_WIDTH);
        let terrace = sky.column(118, -76, 70.0).expect("terrace spur missing on postcard");
        assert!(terrace.half < SKYWAY_HALF_WIDTH);
        let face = sky.column(80, -130, 70.0).expect("face rail missing on postcard");
        assert!(face.half < SKYWAY_HALF_WIDTH);
        assert!(face.deck_y < rail.deck_y, "face rail should sit below the mesa rail");
        let west = sky.column(32, -80, 70.0).expect("west face rail missing on postcard");
        assert!(west.half < SKYWAY_HALF_WIDTH);
        assert!(west.deck_y < rail.deck_y, "west rail should sit below the mesa rail");
        let look_west = sky
            .column(108, -100, 70.0)
            .expect("look-cone west face rail missing on postcard");
        assert!(look_west.half < SKYWAY_HALF_WIDTH);
        assert!(
            look_west.deck_y < rail.deck_y,
            "look-cone west rail should sit below the mesa rail"
        );
    }

    #[test]
    fn crystal_clusters_root_on_the_ground_and_mix_hues() {
        let mut hues = std::collections::BTreeSet::new();
        let mut clusters = 0;
        for ix in -8..8 {
            for iz in -8..8 {
                let Some(cluster) = CrystalCluster::for_cell(31337, ix, iz) else {
                    continue;
                };
                clusters += 1;
                for k in 0..6 {
                    hues.insert(crystal_tint(31337, cluster.cx, cluster.cz, k) as u16);
                }
            }
        }
        assert!(clusters > 40, "only {clusters} clusters over 256 cells");
        assert!(
            hues.len() >= 3,
            "clusters only produced {} crystal hues",
            hues.len()
        );
    }

    #[test]
    fn crystal_cluster_voxels_land_in_the_chunk_they_overlap() {
        let cluster = CrystalCluster {
            cx: 8,
            cz: 8,
            shards: 5,
            scale: 18,
        };
        let mut chunk = Chunk::new(ChunkPos::new(0, 4, 0));
        cluster.stamp(2024, &mut chunk, flat);
        let mut solid = 0;
        for ly in 0..CHUNK_SIZE {
            for lz in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    if chunk.get(lx, ly, lz) != AIR {
                        solid += 1;
                    }
                }
            }
        }
        assert!(
            solid > 20,
            "cluster only wrote {solid} voxels into its chunk"
        );
    }

    #[test]
    fn landmarks_never_write_outside_the_chunk_they_are_given() {
        // `place`/`place_over` are the only writers; if either forgot a
        // bound check this panics rather than silently corrupting a
        // neighbour.
        let planner = FrontierPlanner::new(4711);
        for cy in 0..14 {
            for cx in -2..3 {
                let mut chunk = Chunk::new(ChunkPos::new(cx, cy, 1));
                planner.stamp_landmarks(&mut chunk, flat, flat);
            }
        }
    }

    #[test]
    fn airborne_landmarks_fit_inside_the_default_streamed_slab() {
        // The streamer only loads chunk y in [0, vertical_chunks). An
        // island or station above that ceiling is generated, meshed and
        // never seen. Default is 10 vertical chunks = 160 blocks.
        const DEFAULT_CEILING: i32 = 10 * CHUNK_SIZE_I;
        for ix in -8..8 {
            for iz in -8..8 {
                if let Some(island) = SkyIsland::for_cell(2718, ix, iz, |_, _| 200) {
                    let top = island.cy + (island.radius as f32 * 0.30).round() as i32 + 6;
                    assert!(
                        top < DEFAULT_CEILING,
                        "island cap at {top} is above the ceiling"
                    );
                }
                if let Some(station) = SkyStation::for_cell(2718, ix, iz, |_, _| 200) {
                    let top = station.cy + station.tower + 7;
                    assert!(
                        top < DEFAULT_CEILING,
                        "station mast at {top} is above the ceiling"
                    );
                }
            }
        }
    }
}
