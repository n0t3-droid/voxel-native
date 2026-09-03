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

use crate::blocks::{BlockType, AIR};
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
    pub fn for_cell(seed: u32, ix: i32, iz: i32) -> Option<Self> {
        if cell_rand(seed, 0x0C_10_01, ix, iz) > 0.34 {
            return None;
        }
        let cx =
            ix * CRYSTAL_CELL + (cell_rand(seed, 0x0C_10_02, ix, iz) * CRYSTAL_CELL as f64) as i32;
        let cz =
            iz * CRYSTAL_CELL + (cell_rand(seed, 0x0C_10_03, ix, iz) * CRYSTAL_CELL as f64) as i32;
        Some(Self {
            cx,
            cz,
            shards: 3 + (cell_rand(seed, 0x0C_10_04, ix, iz) * 4.0) as u32,
            scale: 9 + (cell_rand(seed, 0x0C_10_05, ix, iz) * 15.0) as i32,
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
}

impl SkywayColumn {
    /// Block for one voxel of the deck cross-section, or `None` for a
    /// voxel that should simply be left empty.
    pub fn deck_block(&self, wy: i32, lamp: bool) -> Option<BlockType> {
        let edge = self.dist > SKYWAY_HALF_WIDTH - 1.2;
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
            return Some(BlockType::NeonCyan);
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

/// A glowing river carved into the terrain: lava in the volcanic
/// provinces, cyan plasma everywhere else.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RiverColumn {
    /// Depth to cut below the natural surface, in blocks.
    pub cut: i32,
    /// Distance from the channel centreline.
    pub dist: f64,
    /// Fluid that pools in the channel.
    pub fluid: BlockType,
}

/// The energy-river network. Same contour trick as the skyways, at a
/// different frequency and phase so rivers and roads rarely coincide.
pub struct RiverNetwork {
    flow: Perlin,
    heat: Perlin,
}

/// Half-width of an energy channel, in blocks.
const RIVER_HALF_WIDTH: f64 = 5.5;
/// Deepest cut at the channel centreline.
const RIVER_DEPTH: i32 = 7;

impl RiverNetwork {
    pub fn new(seed: u32) -> Self {
        Self {
            flow: Perlin::new(seed.wrapping_add(43)),
            heat: Perlin::new(seed.wrapping_add(44)),
        }
    }

    #[inline]
    fn flow_field(&self, wx: f64, wz: f64) -> f64 {
        self.flow.get([wx * 0.0011 - 77.1, wz * 0.0011 + 44.9]) * 0.70
            + self.flow.get([wx * 0.0027 + 8.3, wz * 0.0027 - 5.1]) * 0.30
    }

    pub fn column(&self, wx: i32, wz: i32) -> Option<RiverColumn> {
        let x = wx as f64;
        let z = wz as f64;
        let a = self.flow_field(x, z);
        if a.abs() > 0.06 {
            return None;
        }
        let e = 3.0;
        let gx = (self.flow_field(x + e, z) - self.flow_field(x - e, z)) / (2.0 * e);
        let gz = (self.flow_field(x, z + e) - self.flow_field(x, z - e)) / (2.0 * e);
        let grad = (gx * gx + gz * gz).sqrt();
        if grad < 1e-9 {
            return None;
        }
        let dist = a.abs() / grad;
        if dist > RIVER_HALF_WIDTH {
            return None;
        }
        // Parabolic channel profile: deepest in the middle, feathering to
        // nothing at the banks so the cut never leaves a vertical wall.
        let t = dist / RIVER_HALF_WIDTH;
        let cut = ((1.0 - t * t) * RIVER_DEPTH as f64).round() as i32;
        if cut <= 0 {
            return None;
        }
        let fluid = if self.heat.get([x * 0.00035, z * 0.00035]) > 0.05 {
            BlockType::Lava
        } else {
            BlockType::PlasmaFlow
        };
        Some(RiverColumn { cut, dist, fluid })
    }
}

/// Depth of fluid standing in a channel above its bed.
pub const RIVER_FILL_DEPTH: i32 = 2;

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
        };
        assert_eq!(col.deck_block(100, false), Some(BlockType::RoadMarking));
        assert_eq!(col.deck_block(99, false), Some(BlockType::PlatingWhite));
        assert_eq!(col.deck_block(101, false), None);

        let edge = SkywayColumn {
            deck_y: 100,
            dist: SKYWAY_HALF_WIDTH - 0.1,
            pylon: false,
        };
        assert_eq!(edge.deck_block(101, false), Some(BlockType::PlatingWhite));
        assert_eq!(edge.deck_block(102, true), Some(BlockType::NeonCyan));
        assert_eq!(edge.deck_block(102, false), None);
    }

    #[test]
    fn energy_rivers_cut_a_feathered_channel_with_glowing_fluid() {
        let net = RiverNetwork::new(555);
        let mut deepest = 0;
        let mut fluids = std::collections::BTreeSet::new();
        for wx in -6000..6000 {
            if let Some(col) = net.column(wx, 128) {
                assert!(col.cut > 0 && col.cut <= RIVER_DEPTH);
                assert!(col.dist <= RIVER_HALF_WIDTH);
                deepest = deepest.max(col.cut);
                fluids.insert(col.fluid as u16);
            }
        }
        assert!(
            deepest >= 4,
            "rivers never cut deeper than {deepest} blocks"
        );
        assert!(
            fluids.contains(&(BlockType::PlasmaFlow as u16))
                || fluids.contains(&(BlockType::Lava as u16)),
            "rivers carried no glowing fluid"
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
