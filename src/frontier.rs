//! Aether Frontier overlay.
//!
//! Deterministic, seed-driven scenery that pushes default worlds toward the
//! engine's sci-fi goal image without converting Earth-like provinces into
//! the old neon-showcase biomes. Every feature is column-local so it streams
//! through the existing chunk pipeline:
//!
//! - floating sky islands (grass decks, stone keels, bloom-crystal undersides)
//! - plasma channels along mesa / canyon floors
//! - skyway decks spanning neighbouring islands
//! - rare orbital station prefabs on hero islands
//!
//! Density stays sparse: a few islands per square kilometre, short plasma
//! filaments, and at most two outbound skyways per island.

use noise::{NoiseFn, Perlin};

use crate::blocks::{
    BlockType, AIR, VOXEL_CRYSTAL_MAGENTA, VOXEL_CRYSTAL_VERDANT, VOXEL_PLASMA_FLOW,
    VOXEL_SKYWAY_DECK,
};
use crate::chunk::{Chunk, CHUNK_SIZE, CHUNK_SIZE_I};
use crate::terrain::{Biome, WATER_LEVEL};

/// Island lattice size in blocks. Large enough that neighbouring islands
/// read as a scattered archipelago, small enough that a skyway between
/// adjacent occupied cells stays in the 40–110 block span budget.
pub const ISLAND_CELL: i32 = 96;

/// Extra vertical headroom, in blocks, reserved above an island deck for
/// the orbital station mast and skyway rails.
pub const STATION_HEADROOM: i32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IslandSpec {
    pub cx: i32,
    pub cz: i32,
    pub radius_x: i32,
    pub radius_z: i32,
    pub deck_y: i32,
    pub keel_depth: i32,
    pub has_station: bool,
    pub crystal: BlockType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IslandColumn {
    pub top_y: i32,
    pub bottom_y: i32,
    /// 0 at the rim, 1000 at the centre. Used to pick grass vs moss.
    pub dist_norm: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkywaySpan {
    pub ax: i32,
    pub az: i32,
    pub ay: i32,
    pub bx: i32,
    pub bz: i32,
    pub by: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkywayColumn {
    pub deck_y: i32,
    pub is_rail: bool,
    pub is_pylon: bool,
}

/// Apply the overlay to a freshly generated chunk. Safe to call twice —
/// writes are deterministic and land in air (islands / skyways) or replace
/// a thin canyon-floor band (plasma).
pub fn decorate_chunk(
    chunk: &mut Chunk,
    seed: u32,
    surface_at: impl Fn(i32, i32) -> i32,
    biome_at: impl Fn(i32, i32) -> Biome,
) {
    let (origin_x, origin_y, origin_z) = chunk.pos.origin();
    let plasma_noise = Perlin::new(seed.wrapping_add(0xA37E_F10A));
    let cell_x0 = origin_x.div_euclid(ISLAND_CELL);
    let cell_z0 = origin_z.div_euclid(ISLAND_CELL);

    let mut islands = [None; 9];
    let mut n_islands = 0usize;
    for dz in -1..=1 {
        for dx in -1..=1 {
            if let Some(spec) =
                island_in_cell(seed, cell_x0 + dx, cell_z0 + dz, &surface_at, &biome_at)
            {
                islands[n_islands] = Some(spec);
                n_islands += 1;
            }
        }
    }

    for lz in 0..CHUNK_SIZE {
        for lx in 0..CHUNK_SIZE {
            let wx = origin_x + lx as i32;
            let wz = origin_z + lz as i32;
            let surface = surface_at(wx, wz);
            let biome = biome_at(wx, wz);

            if let Some(col) = column_in_any_island(wx, wz, &islands[..n_islands]) {
                fill_island_column(
                    chunk,
                    seed,
                    wx,
                    wz,
                    origin_y,
                    col,
                    biome,
                    islands_crystal(wx, wz, &islands[..n_islands]),
                );
            }

            if let Some((lo, hi)) = plasma_band(seed, wx, wz, surface, biome, &plasma_noise) {
                for y in lo..=hi {
                    set_in_chunk(
                        chunk,
                        wx,
                        y,
                        wz,
                        origin_y,
                        BlockType::from_voxel(VOXEL_PLASMA_FLOW),
                        true,
                    );
                }
            }

            if let Some(sky) =
                skyway_column_near(seed, wx, wz, &islands[..n_islands], &surface_at, &biome_at)
            {
                fill_skyway_column(chunk, wx, wz, origin_y, surface, sky);
            }
        }
    }

    for spec in islands.iter().flatten() {
        if spec.has_station {
            stamp_station_into_chunk(chunk, spec.cx, spec.deck_y + 1, spec.cz);
        }
    }
}

/// Highest overlay voxel in a column, used by the streamer so island decks
/// and station masts are not clipped by the surface-only column ceiling.
pub fn overlay_column_top(
    seed: u32,
    wx: i32,
    wz: i32,
    surface_at: impl Fn(i32, i32) -> i32,
    biome_at: impl Fn(i32, i32) -> Biome,
) -> i32 {
    let mut top = surface_at(wx, wz);
    let cell_x = wx.div_euclid(ISLAND_CELL);
    let cell_z = wz.div_euclid(ISLAND_CELL);
    for dz in -1..=1 {
        for dx in -1..=1 {
            let Some(spec) = island_in_cell(seed, cell_x + dx, cell_z + dz, &surface_at, &biome_at)
            else {
                continue;
            };
            if let Some(col) = column_in_island(wx, wz, spec) {
                top = top.max(col.top_y);
                if spec.has_station {
                    top = top.max(spec.deck_y + 1 + STATION_HEADROOM);
                }
            }
            for span in outbound_spans_lookup(seed, spec, &surface_at, &biome_at)
                .into_iter()
                .flatten()
            {
                if let Some(sky) = skyway_column(wx, wz, span) {
                    top = top.max(sky.deck_y + 2);
                }
            }
        }
    }
    top
}

/// Search occupied island cells around `origin` and return the closest deck.
pub fn find_nearest_island(
    seed: u32,
    origin_x: i32,
    origin_z: i32,
    max_radius: i32,
    surface_at: impl Fn(i32, i32) -> i32,
    biome_at: impl Fn(i32, i32) -> Biome,
) -> Option<IslandSpec> {
    let cell_x0 = origin_x.div_euclid(ISLAND_CELL);
    let cell_z0 = origin_z.div_euclid(ISLAND_CELL);
    let cell_range = (max_radius / ISLAND_CELL).max(1) + 1;
    let mut best: Option<(i64, IslandSpec)> = None;
    for ring in 0..=cell_range {
        for dz in -ring..=ring {
            for dx in -ring..=ring {
                if dx.abs() != ring && dz.abs() != ring && ring > 0 {
                    continue;
                }
                let Some(spec) =
                    island_in_cell(seed, cell_x0 + dx, cell_z0 + dz, &surface_at, &biome_at)
                else {
                    continue;
                };
                let ddx = spec.cx - origin_x;
                let ddz = spec.cz - origin_z;
                let dist2 = (ddx as i64) * (ddx as i64) + (ddz as i64) * (ddz as i64);
                if dist2 > (max_radius as i64) * (max_radius as i64) {
                    continue;
                }
                if best.is_none_or(|(best_d, _)| dist2 < best_d) {
                    best = Some((dist2, spec));
                }
            }
        }
        if best.is_some() && ring >= 1 {
            break;
        }
    }
    best.map(|(_, spec)| spec)
}

pub fn island_in_cell(
    seed: u32,
    cell_x: i32,
    cell_z: i32,
    surface_at: &impl Fn(i32, i32) -> i32,
    biome_at: &impl Fn(i32, i32) -> Biome,
) -> Option<IslandSpec> {
    let base_x = cell_x * ISLAND_CELL;
    let base_z = cell_z * ISLAND_CELL;
    let ox = 16 + (hash01(seed, cell_x, cell_z, 1) * (ISLAND_CELL - 32) as f64) as i32;
    let oz = 16 + (hash01(seed, cell_x, cell_z, 2) * (ISLAND_CELL - 32) as f64) as i32;
    let cx = base_x + ox;
    let cz = base_z + oz;
    let biome = biome_at(cx, cz);
    if hash01(seed, cell_x, cell_z, 0) >= island_chance(biome) {
        return None;
    }
    let surface = surface_at(cx, cz);
    island_from_anchor(seed, cell_x, cell_z, cx, cz, surface, biome)
}

fn island_from_anchor(
    seed: u32,
    cell_x: i32,
    cell_z: i32,
    cx: i32,
    cz: i32,
    surface: i32,
    biome: Biome,
) -> Option<IslandSpec> {
    if matches!(biome, Biome::Ocean | Biome::Beach) {
        return None;
    }
    if surface <= WATER_LEVEL + 4 {
        return None;
    }
    // Default streaming loads cy 0..8 (y 0–127). Keep decks + station masts
    // inside that band so islands are not generated into never-meshed air.
    if surface > 96 {
        return None;
    }
    let lift = 24 + (hash01(seed, cell_x, cell_z, 3) * 16.0) as i32;
    let deck_y = (surface + lift).min(118);
    let radius_x = 8 + (hash01(seed, cell_x, cell_z, 4) * 8.0) as i32;
    let radius_z = 7 + (hash01(seed, cell_x, cell_z, 5) * 7.0) as i32;
    let keel_depth = 6 + (hash01(seed, cell_x, cell_z, 6) * 6.0) as i32;
    let has_station = hash01(seed, cell_x, cell_z, 7) < 0.12;
    let crystal = if hash01(seed, cell_x, cell_z, 8) < 0.5 {
        BlockType::from_voxel(VOXEL_CRYSTAL_MAGENTA)
    } else {
        BlockType::from_voxel(VOXEL_CRYSTAL_VERDANT)
    };
    Some(IslandSpec {
        cx,
        cz,
        radius_x,
        radius_z,
        deck_y,
        keel_depth,
        has_station,
        crystal,
    })
}

pub fn column_in_island(wx: i32, wz: i32, spec: IslandSpec) -> Option<IslandColumn> {
    let dx = (wx - spec.cx) as f64;
    let dz = (wz - spec.cz) as f64;
    let nx = dx / spec.radius_x.max(1) as f64;
    let nz = dz / spec.radius_z.max(1) as f64;
    let d2 = nx * nx + nz * nz;
    if d2 > 1.0 {
        return None;
    }
    let t = (1.0 - d2.sqrt()).clamp(0.0, 1.0);
    let thickness = (spec.keel_depth as f64 * (0.35 + 0.65 * t.powf(0.65)))
        .round()
        .max(3.0) as i32;
    let dome = (t * 2.0).round() as i32;
    Some(IslandColumn {
        top_y: spec.deck_y + dome,
        bottom_y: spec.deck_y - thickness,
        dist_norm: (t * 1000.0) as u16,
    })
}

fn column_in_any_island(wx: i32, wz: i32, islands: &[Option<IslandSpec>]) -> Option<IslandColumn> {
    islands
        .iter()
        .flatten()
        .find_map(|spec| column_in_island(wx, wz, *spec))
}

fn islands_crystal(wx: i32, wz: i32, islands: &[Option<IslandSpec>]) -> BlockType {
    islands
        .iter()
        .flatten()
        .find(|spec| column_in_island(wx, wz, **spec).is_some())
        .map(|spec| spec.crystal)
        .unwrap_or(BlockType::from_voxel(VOXEL_CRYSTAL_MAGENTA))
}

fn island_chance(biome: Biome) -> f64 {
    match biome {
        Biome::Mesa | Biome::Mountains | Biome::Karst | Biome::SnowyMountains => 0.34,
        Biome::CrystalSpires | Biome::AlienReef => 0.22,
        Biome::Plains | Biome::Forest | Biome::Savanna | Biome::Desert => 0.11,
        Biome::Jungle | Biome::Tundra | Biome::GlacierShards | Biome::VolcanicWaste => 0.07,
        Biome::Ocean | Biome::Beach => 0.0,
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_island_column(
    chunk: &mut Chunk,
    seed: u32,
    wx: i32,
    wz: i32,
    origin_y: i32,
    col: IslandColumn,
    biome: Biome,
    crystal: BlockType,
) {
    let core = match biome {
        Biome::Mesa => BlockType::RedStone,
        Biome::Karst => BlockType::Limestone,
        Biome::SnowyMountains | Biome::GlacierShards => BlockType::Stone,
        _ => BlockType::Stone,
    };
    let rim = col.dist_norm < 220;
    for y in col.bottom_y..=col.top_y {
        let block = if y == col.top_y {
            if rim {
                BlockType::MossStone
            } else {
                BlockType::Grass
            }
        } else if y >= col.top_y - 2 {
            BlockType::Dirt
        } else if y <= col.bottom_y + 1 {
            crystal
        } else {
            core
        };
        set_in_chunk(chunk, wx, y, wz, origin_y, block, true);
    }
    let hang = 2 + (hash01(seed, wx, wz, 11) * 4.0) as i32;
    for dy in 1..=hang {
        set_in_chunk(chunk, wx, col.bottom_y - dy, wz, origin_y, crystal, false);
    }
}

/// Plasma coolant band for a column, or `None` if this is not a channel.
///
/// The mask is a ridged low-frequency Perlin contour (the same trick as
/// real meandering rivers: the zero-crossing of a smooth field). Only
/// mesa / mountain / karst floors just above sea level qualify, so the
/// overlay cannot flood plains or oceans.
pub fn plasma_band(
    seed: u32,
    wx: i32,
    wz: i32,
    surface: i32,
    biome: Biome,
    noise: &Perlin,
) -> Option<(i32, i32)> {
    let _ = seed;
    if !matches!(biome, Biome::Mesa | Biome::Mountains | Biome::Karst) {
        return None;
    }
    if surface <= WATER_LEVEL + 2 || surface > WATER_LEVEL + 30 {
        return None;
    }
    let n = noise.get([wx as f64 * 0.0074, wz as f64 * 0.0074]);
    let ridge = 1.0 - n.abs();
    let meander = noise.get([wx as f64 * 0.019 + 17.0, wz as f64 * 0.019 - 9.0]);
    let score = ridge + meander.abs() * 0.06;
    if score < 0.875 {
        return None;
    }
    Some((surface - 1, surface + 1))
}

#[cfg(test)]
pub fn plasma_band_at(
    seed: u32,
    wx: i32,
    wz: i32,
    surface: i32,
    biome: Biome,
) -> Option<(i32, i32)> {
    let noise = Perlin::new(seed.wrapping_add(0xA37E_F10A));
    plasma_band(seed, wx, wz, surface, biome, &noise)
}

fn outbound_spans_with(
    spec: IslandSpec,
    partner: impl Fn(i32, i32) -> Option<IslandSpec>,
) -> [Option<SkywaySpan>; 2] {
    let cell_x = spec.cx.div_euclid(ISLAND_CELL);
    let cell_z = spec.cz.div_euclid(ISLAND_CELL);
    let east = partner(cell_x + 1, cell_z).and_then(|b| span_between(spec, b));
    let south = partner(cell_x, cell_z + 1).and_then(|b| span_between(spec, b));
    [east, south]
}

fn outbound_spans_lookup(
    seed: u32,
    spec: IslandSpec,
    surface_at: &impl Fn(i32, i32) -> i32,
    biome_at: &impl Fn(i32, i32) -> Biome,
) -> [Option<SkywaySpan>; 2] {
    outbound_spans_with(spec, |cell_x, cell_z| {
        island_in_cell(seed, cell_x, cell_z, surface_at, biome_at)
    })
}

pub fn span_between(a: IslandSpec, b: IslandSpec) -> Option<SkywaySpan> {
    let dx = (b.cx - a.cx) as f64;
    let dz = (b.cz - a.cz) as f64;
    let len = (dx * dx + dz * dz).sqrt();
    if !(40.0..=118.0).contains(&len) {
        return None;
    }
    Some(SkywaySpan {
        ax: a.cx,
        az: a.cz,
        ay: a.deck_y + 1,
        bx: b.cx,
        bz: b.cz,
        by: b.deck_y + 1,
    })
}

pub fn skyway_column(wx: i32, wz: i32, span: SkywaySpan) -> Option<SkywayColumn> {
    let (dist, t, signed) = point_segment_xz(wx, wz, span.ax, span.az, span.bx, span.bz);
    if dist > 1.55 {
        return None;
    }
    let deck_y = lerp(span.ay as f64, span.by as f64, smoothstep(t)).round() as i32;
    let along = (t * span_length(span)).round() as i32;
    Some(SkywayColumn {
        deck_y,
        is_rail: signed.abs() >= 0.55,
        is_pylon: dist < 0.55 && along % 8 == 0 && (0.04..=0.96).contains(&t),
    })
}

fn skyway_column_near(
    seed: u32,
    wx: i32,
    wz: i32,
    islands: &[Option<IslandSpec>],
    surface_at: &impl Fn(i32, i32) -> i32,
    biome_at: &impl Fn(i32, i32) -> Biome,
) -> Option<SkywayColumn> {
    for spec in islands.iter().flatten() {
        for span in outbound_spans_lookup(seed, *spec, surface_at, biome_at)
            .into_iter()
            .flatten()
        {
            if let Some(col) = skyway_column(wx, wz, span) {
                return Some(col);
            }
        }
    }
    None
}

fn span_length(span: SkywaySpan) -> f64 {
    let dx = (span.bx - span.ax) as f64;
    let dz = (span.bz - span.az) as f64;
    (dx * dx + dz * dz).sqrt()
}

fn fill_skyway_column(
    chunk: &mut Chunk,
    wx: i32,
    wz: i32,
    origin_y: i32,
    surface: i32,
    sky: SkywayColumn,
) {
    let deck = if sky.is_rail {
        BlockType::ShipHullAlloy
    } else {
        BlockType::from_voxel(VOXEL_SKYWAY_DECK)
    };
    set_in_chunk(chunk, wx, sky.deck_y, wz, origin_y, deck, true);
    if sky.is_rail {
        set_in_chunk(
            chunk,
            wx,
            sky.deck_y + 1,
            wz,
            origin_y,
            BlockType::NeonCyan,
            false,
        );
    }
    if sky.is_pylon {
        let top = (sky.deck_y - 1).max(surface + 1);
        for y in (surface + 1)..=top {
            set_in_chunk(chunk, wx, y, wz, origin_y, BlockType::ShipHullDark, false);
        }
    }
}

/// Walk every voxel of the orbital-station prefab. Origin is the pad centre
/// (deck block). Used by worldgen and by the player-facing stamp command.
pub fn visit_orbital_station(
    origin_x: i32,
    origin_y: i32,
    origin_z: i32,
    mut visit: impl FnMut(i32, i32, i32, BlockType),
) {
    for dy in 0..=STATION_HEADROOM {
        for dz in -5..=5 {
            for dx in -5..=5 {
                if let Some(block) = station_block(dx, dy, dz) {
                    visit(origin_x + dx, origin_y + dy, origin_z + dz, block);
                }
            }
        }
    }
}

pub fn station_block(dx: i32, dy: i32, dz: i32) -> Option<BlockType> {
    let adx = dx.abs();
    let adz = dz.abs();
    if dy == 0 {
        if adx <= 4 && adz <= 4 && !(adx == 4 && adz == 4) {
            if adx == 0 && adz == 0 {
                return Some(BlockType::EngineCore);
            }
            if adx == 4 || adz == 4 {
                return Some(BlockType::NeonCyan);
            }
            return Some(BlockType::from_voxel(VOXEL_SKYWAY_DECK));
        }
        return None;
    }
    if dy == 1 && (adx == 4 || adz == 4) && adx <= 4 && adz <= 4 && adx != adz {
        return Some(BlockType::ShipHullAlloy);
    }
    if dx == 0 && dz == 0 && (1..=5).contains(&dy) {
        return Some(BlockType::ShipHullDark);
    }
    if dy == 6 && adx <= 1 && adz <= 1 {
        return Some(BlockType::NeonAmber);
    }
    if dy == 7 && dx == 0 && dz == 0 {
        return Some(BlockType::NeonAmber);
    }
    if dz == 0 && dy == 2 && (1..=4).contains(&dx) {
        return Some(BlockType::ShipHullAlloy);
    }
    if dy == 2 && dx == 5 && dz == 0 {
        return Some(BlockType::NeonCyan);
    }
    if dy == 1 && adx == 3 && adz == 3 {
        return Some(BlockType::EngineCore);
    }
    if (2..=3).contains(&dy) && adx <= 1 && adz <= 1 && !(dx == 0 && dz == 0) {
        return Some(BlockType::CockpitGlass);
    }
    None
}

fn stamp_station_into_chunk(chunk: &mut Chunk, ox: i32, oy: i32, oz: i32) {
    let origin_y = chunk.pos.origin().1;
    visit_orbital_station(ox, oy, oz, |x, y, z, block| {
        set_in_chunk(chunk, x, y, z, origin_y, block, true);
    });
}

/// Compact XZ occupancy map for tests and visual dumps.
/// `I` island, `C` crystal keel, `P` plasma, `S` skyway, `O` station, `.` empty.
#[cfg(test)]
pub fn ascii_overlay_map(
    seed: u32,
    x0: i32,
    z0: i32,
    size: i32,
    surface_at: impl Fn(i32, i32) -> i32,
    biome_at: impl Fn(i32, i32) -> Biome,
) -> String {
    let noise = Perlin::new(seed.wrapping_add(0xA37E_F10A));
    let mut out = String::with_capacity((size * (size + 1)) as usize);
    for dz in 0..size {
        for dx in 0..size {
            let wx = x0 + dx;
            let wz = z0 + dz;
            let surface = surface_at(wx, wz);
            let biome = biome_at(wx, wz);
            let ch = overlay_glyph(seed, wx, wz, surface, biome, &noise, &surface_at, &biome_at);
            out.push(ch);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn overlay_glyph(
    seed: u32,
    wx: i32,
    wz: i32,
    surface: i32,
    biome: Biome,
    noise: &Perlin,
    surface_at: &impl Fn(i32, i32) -> i32,
    biome_at: &impl Fn(i32, i32) -> Biome,
) -> char {
    let cell_x = wx.div_euclid(ISLAND_CELL);
    let cell_z = wz.div_euclid(ISLAND_CELL);
    let mut islands = [None; 9];
    let mut n = 0usize;
    for iz in -1..=1 {
        for ix in -1..=1 {
            if let Some(spec) = island_in_cell(seed, cell_x + ix, cell_z + iz, surface_at, biome_at)
            {
                islands[n] = Some(spec);
                n += 1;
            }
        }
    }
    for spec in islands.iter().flatten() {
        if spec.has_station && (wx - spec.cx).abs() <= 4 && (wz - spec.cz).abs() <= 4 {
            return 'O';
        }
    }
    if skyway_column_near(seed, wx, wz, &islands[..n], surface_at, biome_at).is_some() {
        return 'S';
    }
    if let Some(col) = column_in_any_island(wx, wz, &islands[..n]) {
        if col.dist_norm < 280 {
            return 'C';
        }
        return 'I';
    }
    if plasma_band(seed, wx, wz, surface, biome, noise).is_some() {
        return 'P';
    }
    '.'
}

fn set_in_chunk(
    chunk: &mut Chunk,
    wx: i32,
    wy: i32,
    wz: i32,
    origin_y: i32,
    block: BlockType,
    overwrite: bool,
) {
    let (origin_x, _, origin_z) = chunk.pos.origin();
    let lx = wx - origin_x;
    let ly = wy - origin_y;
    let lz = wz - origin_z;
    if lx < 0 || ly < 0 || lz < 0 || lx >= CHUNK_SIZE_I || ly >= CHUNK_SIZE_I || lz >= CHUNK_SIZE_I
    {
        return;
    }
    let lx = lx as usize;
    let ly = ly as usize;
    let lz = lz as usize;
    if !overwrite && chunk.get(lx, ly, lz) != AIR {
        return;
    }
    if wy <= 2 {
        return;
    }
    chunk.set(lx, ly, lz, block.into());
}

fn point_segment_xz(px: i32, pz: i32, ax: i32, az: i32, bx: i32, bz: i32) -> (f64, f64, f64) {
    let vx = (bx - ax) as f64;
    let vz = (bz - az) as f64;
    let wx = (px - ax) as f64;
    let wz = (pz - az) as f64;
    let len2 = vx * vx + vz * vz;
    if len2 < 1e-6 {
        let dist = (wx * wx + wz * wz).sqrt();
        return (dist, 0.0, 0.0);
    }
    let t = ((wx * vx + wz * vz) / len2).clamp(0.0, 1.0);
    let dx = wx - vx * t;
    let dz = wz - vz * t;
    let dist = (dx * dx + dz * dz).sqrt();
    let signed = (vx * wz - vz * wx) / len2.sqrt();
    (dist, t, signed)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn hash01(seed: u32, x: i32, z: i32, salt: u32) -> f64 {
    let mut h = seed as u64 ^ (salt as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h = h.rotate_left(27).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= (z as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    h = h.rotate_left(31).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    ((h >> 11) as f64) * (1.0 / ((1u64 << 53) as f64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::Voxel;
    use crate::chunk::ChunkPos;
    use crate::terrain::TerrainGenerator;

    fn gen() -> TerrainGenerator {
        TerrainGenerator::new(12345)
    }

    #[test]
    fn island_placement_is_seed_deterministic() {
        let g = gen();
        let a = find_nearest_island(
            12345,
            0,
            0,
            6000,
            |x, z| g.surface_height_at(x, z),
            |x, z| g.biome_at(x, z),
        );
        let b = find_nearest_island(
            12345,
            0,
            0,
            6000,
            |x, z| g.surface_height_at(x, z),
            |x, z| g.biome_at(x, z),
        );
        let c = find_nearest_island(
            99999,
            0,
            0,
            6000,
            |x, z| TerrainGenerator::new(99999).surface_height_at(x, z),
            |x, z| TerrainGenerator::new(99999).biome_at(x, z),
        );
        let a = a.expect("seed 12345 should host at least one island near origin");
        assert_eq!(Some(a), b);
        assert_ne!(Some(a), c);
        assert!(a.radius_x >= 8 && a.radius_x <= 16);
        assert!(a.keel_depth >= 6);
        assert!(a.deck_y > WATER_LEVEL + 16);
    }

    #[test]
    fn island_columns_are_filled_ellipses_with_grass_and_crystal() {
        let g = gen();
        let spec = find_nearest_island(
            12345,
            0,
            0,
            8000,
            |x, z| g.surface_height_at(x, z),
            |x, z| g.biome_at(x, z),
        )
        .expect("island");
        let mut grass = 0usize;
        let mut crystal = 0usize;
        let mut body = 0usize;
        let cx0 = spec.cx.div_euclid(CHUNK_SIZE_I);
        let cz0 = spec.cz.div_euclid(CHUNK_SIZE_I);
        let cy0 = spec.deck_y.div_euclid(CHUNK_SIZE_I);
        for cz in (cz0 - 1)..=(cz0 + 1) {
            for cx in (cx0 - 1)..=(cx0 + 1) {
                for cy in (cy0 - 2)..=(cy0 + 1) {
                    let mut chunk = Chunk::new(ChunkPos::new(cx, cy, cz));
                    decorate_chunk(
                        &mut chunk,
                        12345,
                        |x, z| g.surface_height_at(x, z),
                        |x, z| g.biome_at(x, z),
                    );
                    for lz in 0..CHUNK_SIZE {
                        for ly in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                let v = chunk.get(lx, ly, lz);
                                if v == Voxel::from(BlockType::Grass)
                                    || v == Voxel::from(BlockType::MossStone)
                                {
                                    grass += 1;
                                }
                                if v == VOXEL_CRYSTAL_MAGENTA || v == VOXEL_CRYSTAL_VERDANT {
                                    crystal += 1;
                                }
                                if v == Voxel::from(BlockType::Stone)
                                    || v == Voxel::from(BlockType::RedStone)
                                    || v == Voxel::from(BlockType::Dirt)
                                    || v == Voxel::from(BlockType::Limestone)
                                {
                                    body += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(grass > 20, "island deck should carry grass, got {grass}");
        assert!(
            crystal > 10,
            "island keel should carry bloom crystals, got {crystal}"
        );
        assert!(body > 30, "island body should be rocky, got {body}");
        let centre = column_in_island(spec.cx, spec.cz, spec).expect("centre column");
        assert!(centre.top_y > centre.bottom_y + 3);
        assert!(column_in_island(spec.cx + spec.radius_x + 3, spec.cz, spec).is_none());
    }

    #[test]
    fn plasma_channels_are_sparse_canyon_filaments() {
        let g = gen();
        let mut mesa = 0usize;
        let mut plasma = 0usize;
        for z in (-8_000..=8_000).step_by(48) {
            for x in (-8_000..=8_000).step_by(48) {
                let biome = g.biome_at(x, z);
                if biome != Biome::Mesa && biome != Biome::Mountains && biome != Biome::Karst {
                    continue;
                }
                mesa += 1;
                let surface = g.surface_height_at(x, z);
                if plasma_band_at(12345, x, z, surface, biome).is_some() {
                    plasma += 1;
                }
            }
        }
        assert!(mesa > 20, "need canyon samples, got {mesa}");
        assert!(plasma > 0, "plasma should appear in canyon floors");
        assert!(
            plasma * 4 < mesa,
            "plasma should stay a filament, not a flood ({plasma}/{mesa})"
        );
        let artifact_dir = std::path::Path::new("/opt/cursor/artifacts");
        if artifact_dir.is_dir() {
            let mut sample = None;
            'scan: for z in (-8_000..=8_000).step_by(16) {
                for x in (-8_000..=8_000).step_by(16) {
                    let biome = g.biome_at(x, z);
                    if biome != Biome::Mesa && biome != Biome::Mountains && biome != Biome::Karst {
                        continue;
                    }
                    let surface = g.surface_height_at(x, z);
                    if plasma_band_at(12345, x, z, surface, biome).is_some() {
                        sample = Some((x, z, surface, biome));
                        break 'scan;
                    }
                }
            }
            if let Some((px, pz, surface, biome)) = sample {
                let map = ascii_overlay_map(
                    12345,
                    px - 24,
                    pz - 24,
                    48,
                    |x, z| g.surface_height_at(x, z),
                    |x, z| g.biome_at(x, z),
                );
                let _ = std::fs::write(artifact_dir.join("aether_plasma_xz_map.txt"), &map);
                let _ = std::fs::write(
                    artifact_dir.join("aether_plasma_sample.txt"),
                    format!(
                        "seed=12345\nsample_xz=({}, {})\nsurface_y={}\nbiome={:?}\ncanyon_samples={}\nplasma_hits={}\nfilament_ratio={:.4}\nlegend=P plasma  I island  C crystal  S skyway  O station  . empty\n",
                        px, pz, surface, biome, mesa, plasma, plasma as f64 / mesa as f64
                    ),
                );
            }
        }
    }

    #[test]
    fn skyway_span_math_rejects_short_and_long_hops() {
        let a = IslandSpec {
            cx: 0,
            cz: 0,
            radius_x: 10,
            radius_z: 9,
            deck_y: 90,
            keel_depth: 8,
            has_station: false,
            crystal: BlockType::CrystalMagenta,
        };
        let too_close = IslandSpec { cx: 20, cz: 0, ..a };
        let too_far = IslandSpec {
            cx: 200,
            cz: 0,
            ..a
        };
        let ok = IslandSpec {
            cx: 72,
            cz: 10,
            ..a
        };
        assert!(span_between(a, too_close).is_none());
        assert!(span_between(a, too_far).is_none());
        let span = span_between(a, ok).expect("mid-range hop");
        let mid = skyway_column(36, 5, span).expect("deck at midpoint");
        assert!((mid.deck_y - 91).abs() <= 1);
        assert!(skyway_column(36, 20, span).is_none());
        let artifact_dir = std::path::Path::new("/opt/cursor/artifacts");
        if artifact_dir.is_dir() {
            let dx = (span.bx - span.ax) as f64;
            let dz = (span.bz - span.az) as f64;
            let _ = std::fs::write(
                artifact_dir.join("aether_skyway_span.txt"),
                format!(
                    "span_a=({}, {}, {})\nspan_b=({}, {}, {})\nlength={:.3}\nmid_deck_y={}\nreject_short=20\nreject_long=200\naccept=72,10\n",
                    span.ax, span.ay, span.az, span.bx, span.by, span.bz, (dx * dx + dz * dz).sqrt(), mid.deck_y
                ),
            );
        }
    }

    #[test]
    fn orbital_station_prefab_has_pad_mast_dish_and_docking_arm() {
        let mut pad = 0usize;
        let mut lights = 0usize;
        let mut dish = 0usize;
        let mut arm = 0usize;
        visit_orbital_station(0, 80, 0, |x, y, z, block| {
            match block {
                BlockType::SkywayDeck => pad += 1,
                BlockType::NeonCyan => {
                    lights += 1;
                    if x == 5 && y == 82 && z == 0 {
                        arm += 1;
                    }
                }
                BlockType::NeonAmber => dish += 1,
                _ => {}
            }
            assert!((x).abs() <= 5);
            assert!((z).abs() <= 5);
            assert!((80..=88).contains(&y));
        });
        assert!(pad >= 40, "station pad too small ({pad})");
        assert!(lights >= 8);
        assert!(dish >= 5, "antenna dish missing");
        assert_eq!(arm, 1, "docking-arm tip");
        assert_eq!(station_block(0, 0, 0), Some(BlockType::EngineCore));
        assert_eq!(station_block(0, 7, 0), Some(BlockType::NeonAmber));
        assert!(station_block(5, 5, 5).is_none());
        let artifact_dir = std::path::Path::new("/opt/cursor/artifacts");
        if artifact_dir.is_dir() {
            let _ = std::fs::write(
                artifact_dir.join("aether_orbital_station.txt"),
                format!(
                    "origin=(0, 80, 0)\npad_skyway_deck={pad}\nneon_cyan_lights={lights}\namber_dish={dish}\ndocking_arm_tip={arm}\nmast_core=EngineCore at (0,80,0)\ndish_tip=NeonAmber at (0,87,0)\nbounds_xz=[-5,5]\nbounds_y=[80,88]\n"
                ),
            );
        }
    }

    #[test]
    fn overlay_does_not_scatter_legacy_showcase_blocks() {
        let g = gen();
        let showcase: [Voxel; 6] = [
            BlockType::Crystal.into(),
            BlockType::LuminiteCrystal.into(),
            BlockType::AlienMoss.into(),
            BlockType::GlowSand.into(),
            BlockType::Lava.into(),
            BlockType::IridiumVein.into(),
        ];
        let spec = find_nearest_island(
            12345,
            0,
            0,
            8000,
            |x, z| g.surface_height_at(x, z),
            |x, z| g.biome_at(x, z),
        )
        .expect("island");
        let cx = spec.cx.div_euclid(CHUNK_SIZE_I);
        let cz = spec.cz.div_euclid(CHUNK_SIZE_I);
        let cy = spec.deck_y.div_euclid(CHUNK_SIZE_I);
        let mut chunk = Chunk::new(ChunkPos::new(cx, cy, cz));
        decorate_chunk(
            &mut chunk,
            12345,
            |x, z| g.surface_height_at(x, z),
            |x, z| g.biome_at(x, z),
        );
        for lz in 0..CHUNK_SIZE {
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let v = chunk.get(lx, ly, lz);
                    assert!(
                        !showcase.contains(&v),
                        "frontier overlay wrote legacy showcase voxel {v}"
                    );
                }
            }
        }
    }

    #[test]
    fn ascii_overlay_map_is_stable_and_nonempty() {
        let g = gen();
        let spec = find_nearest_island(
            12345,
            0,
            0,
            8000,
            |x, z| g.surface_height_at(x, z),
            |x, z| g.biome_at(x, z),
        )
        .expect("island");
        let map = ascii_overlay_map(
            12345,
            spec.cx - 24,
            spec.cz - 24,
            48,
            |x, z| g.surface_height_at(x, z),
            |x, z| g.biome_at(x, z),
        );
        let map2 = ascii_overlay_map(
            12345,
            spec.cx - 24,
            spec.cz - 24,
            48,
            |x, z| g.surface_height_at(x, z),
            |x, z| g.biome_at(x, z),
        );
        assert_eq!(map, map2);
        assert!(map.contains('I'), "map should show island body:\n{map}");
        assert!(
            map.chars().filter(|c| *c == 'I' || *c == 'C').count() > 20,
            "island footprint too small:\n{map}"
        );
        let artifact_dir = std::path::Path::new("/opt/cursor/artifacts");
        if artifact_dir.is_dir() {
            let _ = std::fs::write(artifact_dir.join("aether_island_xz_map.txt"), &map);
            let spans =
                outbound_spans_lookup(12345, spec, &|x, z| g.surface_height_at(x, z), &|x, z| {
                    g.biome_at(x, z)
                });
            let col_top = overlay_column_top(
                12345,
                spec.cx,
                spec.cz,
                |x, z| g.surface_height_at(x, z),
                |x, z| g.biome_at(x, z),
            );
            let _ = std::fs::write(
                artifact_dir.join("aether_island_spec.txt"),
                format!(
                    "seed=12345\ncenter_xz=({}, {})\ndeck_y={}\nradius_xz=({}, {})\nkeel_depth={}\nhas_station={}\ncrystal={:?}\ncolumn_top={}\neast_skyway={:?}\nsouth_skyway={:?}\nlegend=I island body  C crystal rim/keel  S skyway  O station  P plasma  . empty\n",
                    spec.cx,
                    spec.cz,
                    spec.deck_y,
                    spec.radius_x,
                    spec.radius_z,
                    spec.keel_depth,
                    spec.has_station,
                    spec.crystal,
                    col_top,
                    spans[0],
                    spans[1]
                ),
            );
        }
    }

    #[test]
    fn overlay_column_top_covers_island_deck_and_station_mast() {
        let g = gen();
        let spec = find_nearest_island(
            12345,
            0,
            0,
            8000,
            |x, z| g.surface_height_at(x, z),
            |x, z| g.biome_at(x, z),
        )
        .expect("island");
        let top = overlay_column_top(
            12345,
            spec.cx,
            spec.cz,
            |x, z| g.surface_height_at(x, z),
            |x, z| g.biome_at(x, z),
        );
        let surface = g.surface_height_at(spec.cx, spec.cz);
        assert!(top > surface + 10);
        let centre = column_in_island(spec.cx, spec.cz, spec).unwrap();
        assert!(top >= centre.top_y);
        if spec.has_station {
            assert!(top >= spec.deck_y + STATION_HEADROOM);
        }
    }

    #[test]
    fn terrain_generate_installs_frontier_voxels_in_air() {
        let g = gen();
        let spec = find_nearest_island(
            12345,
            0,
            0,
            8000,
            |x, z| g.surface_height_at(x, z),
            |x, z| g.biome_at(x, z),
        )
        .expect("island");
        let cx = spec.cx.div_euclid(CHUNK_SIZE_I);
        let cz = spec.cz.div_euclid(CHUNK_SIZE_I);
        let cy = spec.deck_y.div_euclid(CHUNK_SIZE_I);
        let mut chunk = Chunk::new(ChunkPos::new(cx, cy, cz));
        g.generate(&mut chunk);
        let mut overlay = 0usize;
        for lz in 0..CHUNK_SIZE {
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let v = chunk.get(lx, ly, lz);
                    if v == Voxel::from(BlockType::Grass)
                        || v == VOXEL_CRYSTAL_MAGENTA
                        || v == VOXEL_CRYSTAL_VERDANT
                        || v == VOXEL_SKYWAY_DECK
                    {
                        overlay += 1;
                    }
                }
            }
        }
        assert!(
            overlay > 0,
            "generated chunk at island deck should contain overlay voxels"
        );
    }
}
