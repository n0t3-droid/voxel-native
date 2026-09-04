//! Terrain generation.
//!
//! Ported from R93G's `lib/voxel/terrain.ts`. The stack is:
//!
//!   1. Continentalness + erosion FBM (low freq) â†’ large-scale landmass shape.
//!   2. Domain-warped FBM (mid freq) â†’ organic-looking hills.
//!   3. Ridged FBM â†’ mountain ridges in high-continentalness areas.
//!   4. 3D narrow-band cave noise â†’ hollows under the surface.
//!   5. Temperature + Moisture classifier â†’ biome â†’ surface block palette.
//!
//! Each noise layer is seeded deterministically off the world seed so two
//! worlds with the same seed produce byte-identical chunks.

use crate::blocks::{BlockType, Voxel, AIR};
use crate::chunk::{Chunk, ChunkPos, CHUNK_SIZE, CHUNK_SIZE_I};
use crate::frontier::{self, SkywayNetwork};
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
    /// Iconic American canyon: red sandstone mesas, bone-dry plateaus.
    Mesa,
    /// Chinese karst: tall green-mossy limestone pillars amid jungle.
    Karst,
    // ---- Alien planetary biomes (sniper-shooter playgrounds) ----
    /// Pandora-style towering cyan crystal spires over a glowing pale
    /// sand floor. Vertical sniper nests + long horizontal sightlines
    /// underneath the spire canopy.
    CrystalSpires,
    /// Mars/Io basalt plains laced with bright lava rivers.
    /// Wide-open kill corridors broken by impassable lava channels.
    VolcanicWaste,
    /// Hoth-style razor ice ridges and crevasses. Long-bowl shots
    /// between ridges; ridge-lines double as ambush cover.
    GlacierShards,
    /// Bioluminescent purple moss with bone-white pillar arches.
    /// Mid-range cover-and-move terrain.
    AlienReef,
}

impl Biome {
    #[inline]
    pub fn is_neon_showcase(self) -> bool {
        matches!(self, Biome::AlienReef | Biome::CrystalSpires)
    }

    #[inline]
    pub fn is_showcase_terrain(self) -> bool {
        matches!(
            self,
            Biome::AlienReef | Biome::CrystalSpires | Biome::GlacierShards | Biome::VolcanicWaste
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeonSpawnPoint {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub biome: Biome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NaturalSpawnPoint {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub biome: Biome,
}

/// Macro-region province. Returned by `region()` for any world (x,z).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Plains,
    Canyon,
    Plateau,
    Highland,
    Wetland,
    Karst,
    // ---- Alien planetary regions ----
    CrystalSpires,
    VolcanicWaste,
    GlacierShards,
    AlienReef,
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
    /// Long, sinuous "worm" tunnel layer that carves big horizontal
    /// tubes straight through mountains. Separate from the narrow
    /// cave band so tunnels feel distinct from cramped caves.
    tunnel_a: Perlin,
    tunnel_b: Perlin,
    /// Low-frequency noise used to decide tunnel elevation + flow
    /// direction so tunnels stay roughly horizontal over long spans.
    tunnel_path: Perlin,
    /// Huge spherical cavern rooms scattered underground â€” the
    /// dramatic "oh wow" chambers you stumble into from a tunnel.
    cavern: Perlin,
    temperature: Perlin,
    moisture: Perlin,
    /// Macro-region noise â€” extremely low frequency (~0.0002), defines
    /// vast geographic provinces hundreds of chunks across: highlands,
    /// canyon mesas, vast plateaus, lush wetlands. The single biggest
    /// "real-world variety" cue: walking 2 km in one direction takes
    /// you from European meadows to Grand-Canyon-style plateaus.
    region: Perlin,
    /// Secondary region channel, orthogonal to `region`, used to break
    /// up region boundaries so they don't all line up along one axis.
    region_b: Perlin,
    /// Skyway routes, energy rivers and the lattice-anchored landmarks
    /// (sky islands, docking stations, crystal clusters).
    frontier: crate::frontier::FrontierPlanner,
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
            tunnel_a: Perlin::new(seed.wrapping_add(21)),
            tunnel_b: Perlin::new(seed.wrapping_add(22)),
            tunnel_path: Perlin::new(seed.wrapping_add(23)),
            cavern: Perlin::new(seed.wrapping_add(24)),
            temperature: Perlin::new(seed.wrapping_add(10)),
            moisture: Perlin::new(seed.wrapping_add(11)),
            region: Perlin::new(seed.wrapping_add(12)),
            region_b: Perlin::new(seed.wrapping_add(13)),
            frontier: crate::frontier::FrontierPlanner::new(seed),
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

    /// Macro-region classification. Each world point is dominated by
    /// ONE province (canyon / plateau / highland / wetland / karst /
    /// normal). Province boundaries are smoothed but the dominant
    /// region's modifier is heavily weighted so cliffs from canyon
    /// regions don't leak into adjacent plains. Returns:
    /// (kind, strength in 0..1).
    fn region(&self, wx: f64, wz: f64) -> (Region, f64) {
        // Three orthogonal very-low-frequency channels in [-1, 1].
        let a = self.region.get([wx * 0.00018, wz * 0.00018]);
        let b = self.region_b.get([wx * 0.00021 + 17.3, wz * 0.00021 - 9.7]);
        // Third axis: separates karst out from the other 4 quadrants.
        let c = self.region.get([wx * 0.00013 - 41.7, wz * 0.00013 + 23.1]);

        // Region centers in (a, b, c) space.
        //
        // Every world is the same planet: a neon frontier of banded
        // canyon mesas, crystal spire fields, volcanic flats, glacier
        // ridges and bioluminescent reefs. Canyon is deliberately given
        // two centres because banded mesa country is the terrain the key
        // art is mostly made of — it should be what you fly over between
        // the rarer set-pieces, not one province in nine.
        let centers: [(f64, f64, f64, Region); 9] = [
            (-0.60, -0.55, -0.45, Region::Canyon),
            (0.58, 0.60, 0.42, Region::Canyon),
            (0.60, -0.58, -0.40, Region::CrystalSpires),
            (-0.58, 0.60, -0.42, Region::AlienReef),
            (0.55, 0.55, -0.48, Region::VolcanicWaste),
            (-0.55, -0.55, 0.50, Region::GlacierShards),
            (0.00, 0.00, 0.46, Region::Karst),
            (-0.62, 0.05, 0.44, Region::Highland),
            (0.10, -0.62, 0.40, Region::Plateau),
        ];

        // Find dominant region by closest center; strength is how much
        // it dominates the runner-up (so deep interiors get full effect
        // and only the thin boundary band feathers).
        let mut best = (Region::Plains, f64::INFINITY);
        let mut second = f64::INFINITY;
        for (cx, cy, cz, r) in &centers {
            let dx = a - cx;
            let dy = b - cy;
            let dz = c - cz;
            let d = dx * dx + dy * dy + dz * dz;
            if d < best.1 {
                second = best.1;
                best = (*r, d);
            } else if d < second {
                second = d;
            }
        }
        // Strength: 0 right at the boundary, ~1 deep inside the region.
        // Squared-distance ratio gives a soft falloff. Nine provinces sit
        // closer together than five did, so the ramp is steeper to keep
        // province interiors at full strength instead of leaving the
        // whole map in a permanent half-blended boundary state.
        let margin = (second - best.1).max(0.0);
        let strength = (margin * 7.0).min(1.0);
        // Below a threshold, treat as "normal" mixed terrain — the green
        // transitional country between the set-piece provinces.
        if strength < 0.10 {
            (Region::Plains, 0.0)
        } else {
            (best.0, strength)
        }
    }

    /// Smooth macro elevation — continentalness and erosion only, with
    /// no hills, ridges or province modifiers layered on.
    ///
    /// This is the surface the terrain *would* have if it were sanded
    /// flat, and it is what the skyway decks ride on: a deck offset from
    /// this stays level while the real ground heaves 80 blocks up into a
    /// mesa or drops away into a canyon underneath it.
    pub fn macro_height(&self, wx: f64, wz: f64) -> f64 {
        let cont = self.fbm2(&self.continent, wx * 0.0002, wz * 0.0002, 4, 2.0, 0.5);
        let erod = self.fbm2(&self.erosion, wx * 0.0005, wz * 0.0005, 3, 2.0, 0.5);
        50.0 + cont * 32.0 + (1.0 - erod.abs()) * 8.0
    }

    /// Height of the terrain surface at world (x,z), in blocks.
    fn surface_height(&self, wx: f64, wz: f64) -> (i32, f64) {
        // 1. Continentalness â€” very low frequency, defines ocean vs land.
        //    Halved frequency so continents stretch ~2Ã— wider â€” bigger
        //    plains, longer coastlines, gentler oceanâ†’land transitions.
        let cont = self.fbm2(&self.continent, wx * 0.0002, wz * 0.0002, 4, 2.0, 0.5);

        // 2. Erosion â€” smooths out where it's high, carves where it's low.
        let erod = self.fbm2(&self.erosion, wx * 0.0005, wz * 0.0005, 3, 2.0, 0.5);

        // 3. Domain-warped hills â€” the "lumpy" medium-scale terrain.
        //    Lower frequency + bigger warp = wider, more flowing hills
        //    instead of bumpy fields.
        let warp_scale = 120.0;
        let dx = self.warp_x.get([wx * 0.001, wz * 0.001]) * warp_scale;
        let dz = self.warp_z.get([wx * 0.001, wz * 0.001]) * warp_scale;
        let hills = self.fbm2(
            &self.hills_a,
            (wx + dx) * 0.003,
            (wz + dz) * 0.003,
            5,
            2.0,
            0.5,
        );

        // 4. Ridged mountains â€” only "felt" where continentalness is high.
        //    Lower frequency = wider mountain ranges with broad foothills
        //    rather than tightly-packed spires.
        let ridges = self.ridged_fbm(&self.ridges, wx * 0.0015, wz * 0.0015, 5);
        let mountain_mask = ((cont - 0.1).max(0.0) * 2.5).min(1.0);

        // Combine. Tuned for a more balanced world: lots of flat plains
        // and wide beaches, gentle rolling hills, and mountains that
        // ramp up gradually over hundreds of blocks. Hill amplitude
        // dropped from 18 â†’ 12 (calmer fields) and the quadratic peak
        // boost stretched (threshold 0.55â†’0.62, coefficient 110â†’90) so
        // mountains rise more slowly across a wider footprint.
        let peak_boost = (cont - 0.62).max(0.0);
        let peak_boost = peak_boost * peak_boost * 80.0;
        let base = 50.0 + cont * 32.0 + (1.0 - erod.abs()) * 8.0;
        let mut h = base + hills * 14.0 + ridges * 72.0 * mountain_mask + peak_boost;

        // ----------- Macro-region modifier -----------
        // Apply ONE geographic-province transform with high strength
        // inside the region's interior, smoothly fading to nothing at
        // the boundary. Winner-take-all so canyon banding NEVER leaks
        // into adjacent plains and vice versa.
        let (region, rs) = self.region(wx, wz);
        match region {
            Region::Canyon => {
                // Grand-Canyon style mesas: snap heights to ~16-block
                // plateaus separated by sharp drops. Only above water.
                if h > WATER_LEVEL as f64 + 4.0 {
                    let step = 24.0; // taller mesa steps
                    let banded = (h / step).round() * step;
                    let pull = rs * 0.92;
                    h = h * (1.0 - pull) + banded * pull;
                    // Boost overall canyon altitude so mesas tower
                    // dramatically above the canyon floor.
                    h += rs * 22.0;
                    // Carved canyon floors: where erosion is high, drop
                    // a deep slot. Creates river-cut canyons through
                    // the mesa fields.
                    let carve = (erod.abs() - 0.45).max(0.0) * 38.0;
                    h -= rs * carve;
                }
            }
            Region::Plateau => {
                // Vast tableland at h~88. Tibetan / Iberian high steppe.
                let plateau_h = 88.0;
                let pull = rs * 0.70;
                h = h * (1.0 - pull) + plateau_h * pull;
                h += rs * hills * 8.0;
            }
            Region::Highland => {
                // Alpine: strong enough for skyline silhouettes, capped
                // so normal worlds do not turn into vertical walls that
                // hitch low-end machines when approached.
                h += rs * ridges.abs() * 58.0;
            }
            Region::Wetland => {
                // Floodplain: pull to just above water level.
                let target = WATER_LEVEL as f64 + 1.5;
                let pull = rs * 0.6;
                h = h * (1.0 - pull) + target * pull;
                h += rs * hills * 4.5;
            }
            Region::Karst => {
                // Chinese karst: vertical pillars rising 30-60 blocks
                // out of a flat jungle floor. We compute a "pillar
                // mask" from cubed ridged noise (sharp and isolated)
                // and add it as additional height. The flat base also
                // gets pulled slightly upward so pillars rise from
                // verdant ground rather than ocean.
                let base_pull = rs * 0.4;
                let karst_floor = WATER_LEVEL as f64 + 6.0;
                h = h * (1.0 - base_pull) + karst_floor * base_pull;
                let pillar_n = self.ridged_fbm(&self.ridges, wx * 0.008, wz * 0.008, 4); // wider pillars
                                                                                         // Cube to make pillars sharp & isolated rather than
                                                                                         // continuous ridges. Threshold so only the strongest
                                                                                         // peaks become pillars.
                let pillar = (pillar_n - 0.62).max(0.0);
                let pillar = pillar * pillar * pillar * 520.0;
                h += rs * pillar;
            }
            Region::CrystalSpires => {
                // Towering hex-prism-feel pillars on a flat glow-sand
                // floor. The high threshold keeps the biome readable as
                // hero spires with flight corridors instead of a dense
                // translucent wall that overloads low-end GPUs up close.
                let base_pull = rs * 0.62;
                let floor = WATER_LEVEL as f64 + 10.0;
                h = h * (1.0 - base_pull) + floor * base_pull;
                let r1 = self.ridged_fbm(&self.ridges, wx * 0.0075, wz * 0.0075, 3);
                let r2 = self.ridged_fbm(&self.hills_b, wx * 0.0085 + 91.3, wz * 0.0085 - 47.5, 3);
                let spike = r1.min(r2);
                let spike = (spike - 0.49).max(0.0);
                let spike = spike * spike * spike * 2500.0;
                h += rs * spike;
            }
            Region::VolcanicWaste => {
                // Huge basalt plains and massive lava rivers for RPGs and vehicle passes.
                let plateau_h = 72.0;
                let pull = rs * 0.55;
                h = h * (1.0 - pull) + plateau_h * pull;
                h += rs * hills * 4.0;
                let river = self.ridged_fbm(&self.hills_a, wx * 0.003, wz * 0.003, 3); // wider rivers
                if river > 0.70 {
                    // easier threshold for huge canyons
                    let depth = ((river - 0.70) * 50.0).min(18.0);
                    h -= rs * depth;
                }
            }
            Region::GlacierShards => {
                // Razor ridge crevasses for huge bowls and high elevation sniper lookouts
                let ridge_sharp = ridges * ridges;
                h += rs * ridge_sharp * 180.0; // taller ridges
                let base_pull = rs * 0.25;
                let floor = WATER_LEVEL as f64 + 8.0;
                h = h * (1.0 - base_pull) + floor * base_pull;
            }
            Region::AlienReef => {
                // Huge moss hills and huge bone arches
                let base_pull = rs * 0.5;
                let reef_floor = WATER_LEVEL as f64 + 12.0;
                h = h * (1.0 - base_pull) + reef_floor * base_pull;
                h += rs * hills * 15.0; // taller hills
                let pillar_n =
                    self.ridged_fbm(&self.ridges, wx * 0.015 - 13.7, wz * 0.015 + 8.4, 3);
                let pillar = (pillar_n - 0.65).max(0.0);
                let pillar = pillar * pillar * 2400.0; // massive pillars
                h += rs * pillar;
            }
            Region::Plains => {
                // Mixed/normal terrain â€” no province modifier.
            }
        }

        // ----------- Skyline compression -----------
        // Crystal spikes and reef pillars can stack to 260+ blocks, but
        // the streamer only loads chunk y in [0, vertical_chunks). Any
        // terrain above that ceiling is not "tall", it is decapitated:
        // the player sees a spire sheared off into a flat table. Rather
        // than clamping (which produces exactly that), compress
        // everything above the knee so the relief below is untouched and
        // the tallest hero silhouettes taper into the sky instead.
        const SKYLINE_KNEE: f64 = 118.0;
        const SKYLINE_COMPRESSION: f64 = 0.24;
        if h > SKYLINE_KNEE {
            h = SKYLINE_KNEE + (h - SKYLINE_KNEE) * SKYLINE_COMPRESSION;
        }

        // Coastal smoothing: heights close to the water line create
        // pointy "teeth" shorelines because rounding flips neighbouring
        // columns between y=48 and y=49. Pull heights in the narrow
        // band [WATER_LEVEL-1.5, WATER_LEVEL+2.5] toward a two-level
        // shore curve (sub-water ocean floor vs firm beach at
        // WATER_LEVEL+1) so the transition is stable rather than
        // stochastic.
        let wl = WATER_LEVEL as f64;
        let delta = h - wl;
        if delta > -1.5 && delta < 2.5 {
            // Smooth-step from "just barely submerged" to "firm beach".
            // Ensures shore columns snap to WATER_LEVEL-1 or WATER_LEVEL+1.
            if delta < 0.5 {
                h = wl - 1.0; // submerged shore â†’ ocean floor
            } else {
                h = wl + 1.0; // exposed shore â†’ beach
            }
        }

        // ----------- Energy river channels -----------
        // Carved here rather than in `generate()` so every consumer of
        // the height field — spawn search, bot siting, ship landing,
        // collision — agrees the channel is there. `generate()` re-reads
        // the same channel to decide what fluid pools in it.
        let mut h = h.round() as i32;
        if h > WATER_LEVEL + 4 {
            if let Some(river) = self
                .frontier
                .rivers
                .column(wx.round() as i32, wz.round() as i32)
            {
                h -= river.cut;
            }
        }

        (h, cont)
    }

    /// 3D narrow-band cave noise. Returns `true` if this world cell is
    /// hollow (carved out by a cave).
    fn is_cave(&self, wx: f64, wy: f64, wz: f64) -> bool {
        // Perlin noise is identically zero at the origin for every seed,
        // so without a seed-dependent offset the cave condition
        // (|a| < band && |b| < band) is ALWAYS true at (0, 0, 0) â€”
        // which is right under spawn. That's why every freshly-generated
        // world showed the same cluster of surface holes at spawn no
        // matter what seed was used. Offsetting the sample coords by a
        // seed-derived vector moves that degenerate point somewhere else.
        let s = self.seed as f64;
        let ox = (s * 0.12345).sin() * 10_000.0 + 100_000.0;
        let oy = (s * 0.54321).cos() * 10_000.0 + 100_000.0;
        let oz = (s * 0.98765).sin() * 10_000.0 + 100_000.0;
        // Two FBM fields; caves live where BOTH are close to zero (narrow
        // band), which produces tunnel-like geometry rather than big blobs.
        let a = self.fbm3(
            &self.caves_a,
            (wx + ox) * 0.02, // lower frequency for massive caves
            (wy + oy) * 0.04,
            (wz + oz) * 0.02,
            3,
        );
        let b = self.fbm3(
            &self.caves_b,
            (wx + ox) * 0.02 + 13.7,
            (wy + oy) * 0.04 + 7.1,
            (wz + oz) * 0.02 - 5.3,
            3,
        );
        let band = 0.045; // much wider cave systems
        a.abs() < band && b.abs() < band
    }

    /// Big horizontal "worm" tunnel â€” cuts straight through mountain
    /// sides and hillsides so the player can walk/drive/fly through
    /// them. Distinct from `is_cave` (which is narrow winding caves).
    ///
    /// Strategy: two orthogonal low-frequency 3D fields; the
    /// intersection of their near-zero bands traces a sinuous line in
    /// 3D space â€” a tunnel. The Y coordinate is compressed so the
    /// tunnel stays roughly horizontal (flatter in Y than in XZ).
    fn is_tunnel(&self, wx: f64, wy: f64, wz: f64) -> bool {
        let s = self.seed as f64;
        let ox = (s * 0.37121).sin() * 10_000.0 + 200_000.0;
        let oy = (s * 0.81723).cos() * 10_000.0 + 200_000.0;
        let oz = (s * 0.41287).sin() * 10_000.0 + 200_000.0;

        // Path noise gently bends the tunnel vertically so long
        // tunnels rise and fall like a real subway line.
        let bend = self
            .tunnel_path
            .get([(wx + ox) * 0.0015, (wz + oz) * 0.0015])
            * 12.0;
        let wy_adj = wy - bend;

        // Strong Y compression (Ã—0.18) â†’ tunnels are much longer in
        // XZ than in Y â†’ you get long horizontal tubes, not vertical
        // shafts.
        let a = self.fbm3(
            &self.tunnel_a,
            (wx + ox) * 0.010,
            (wy_adj + oy) * 0.018,
            (wz + oz) * 0.010,
            3,
        );
        let b = self.fbm3(
            &self.tunnel_b,
            (wx + ox) * 0.010 + 31.3,
            (wy_adj + oy) * 0.018 + 17.1,
            (wz + oz) * 0.010 - 12.7,
            3,
        );
        // Wider band than normal caves â†’ 3-6 block tall corridors.
        let band = 0.055;
        a.abs() < band && b.abs() < band
    }

    /// Massive spherical cavern rooms, sparse and deep. The player
    /// pops out of a tunnel into one of these once in a while.
    fn is_cavern(&self, wx: f64, wy: f64, wz: f64) -> bool {
        let s = self.seed as f64;
        let ox = (s * 0.61987).sin() * 10_000.0 + 300_000.0;
        let oy = (s * 0.23918).cos() * 10_000.0 + 300_000.0;
        let oz = (s * 0.72831).sin() * 10_000.0 + 300_000.0;
        let v = self.fbm3(
            &self.cavern,
            (wx + ox) * 0.006,
            (wy + oy) * 0.010,
            (wz + oz) * 0.006,
            2,
        );
        // Very sparse â€” only the strongest noise peaks.
        v > 0.58
    }

    /// Pick a biome for this column based on temperature + moisture +
    /// continentalness (so beaches appear at coastlines, mountains at high
    /// continentalness, etc.).
    fn biome(&self, wx: f64, wz: f64, height: i32, cont: f64) -> Biome {
        if height <= WATER_LEVEL - 2 {
            return Biome::Ocean;
        }
        if crate::frontier::in_hero_postcard(wx as i32, wz as i32) && height > WATER_LEVEL + 2 {
            return Biome::Mesa;
        }
        // Region overrides (above water): alien & special regions
        // dominate even at weak strength so the player sees them
        // often. Classic canyons / karst need a bit more authority.
        let (region, rs) = self.region(wx, wz);
        if rs > 0.08 && height > WATER_LEVEL + 2 {
            match region {
                Region::Canyon => {
                    // Banded mesa country is the frontier's default
                    // ground, so it asserts itself well before the
                    // province interior rather than only at full
                    // strength like the old earth-like canyon province.
                    if rs > 0.12 {
                        return Biome::Mesa;
                    }
                }
                Region::Karst => {
                    if rs > 0.18 {
                        return Biome::Karst;
                    }
                }
                Region::CrystalSpires => return Biome::CrystalSpires,
                Region::VolcanicWaste => return Biome::VolcanicWaste,
                Region::GlacierShards => return Biome::GlacierShards,
                Region::AlienReef => return Biome::AlienReef,
                _ => {}
            }
        }
        // Wider beach band (up to +3 above water) gives shores actual
        // depth instead of a 1-block sand stripe. The exact extent is
        // perturbed by a low-frequency noise so beaches feather into
        // grass with organic in-and-out fingers â€” the single biggest
        // visual upgrade for coastlines, no extra geometry needed.
        let beach_wobble = self.moisture.get([wx * 0.008, wz * 0.008]) * 3.5; // wider, softer beaches
        let beach_top = WATER_LEVEL + 4 + beach_wobble as i32; // even higher beach transitions
        if height <= beach_top {
            return Biome::Beach;
        }

        let temp = self.temperature.get([wx * 0.0015, wz * 0.0015]); // vast temperature bands
        let moist = self.moisture.get([wx * 0.0015, wz * 0.0015]);

        // Altitude-driven snow line, perturbed by temperature so the line
        // isn't a ruler-straight horizontal cut. Cold latitudes have snow
        // starting ~15 blocks lower, warm latitudes push it ~15 higher.
        // A second low-freq noise wobbles the line column-by-column Â±6
        // blocks so the grass-rock-snow transition fingers organically
        // up and down the mountainside instead of running as a clean
        // horizontal stripe.
        let line_wobble = self.moisture.get([wx * 0.008, wz * 0.008]) * 6.0
            + self.erosion.get([wx * 0.02, wz * 0.02]) * 3.0;
        let snow_line = 138 + (temp * -15.0) as i32 + line_wobble as i32;
        let rock_line = snow_line - 20 + (line_wobble * 0.6) as i32;
        if height > snow_line {
            return Biome::SnowyMountains;
        }
        if height > rock_line {
            return Biome::Mountains;
        }

        if cont > 0.55 && temp > 0.2 {
            return if temp > 0.4 {
                Biome::Desert
            } else {
                Biome::Savanna
            };
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
            // Mesa: rust-red dust on top, sandstone underneath. The
            // generate() loop overrides `core` per-Y so cliff faces
            // stripe between RedStone / MesaClay / RedSand bands.
            Biome::Mesa => (BlockType::RedSand, BlockType::RedStone, BlockType::RedStone),
            // Karst pillars: dark mossy limestone with a brighter core.
            Biome::Karst => (
                BlockType::MossStone,
                BlockType::Limestone,
                BlockType::Limestone,
            ),
            // Alien crystal spires: glow-sand floor, crystal cores.
            // The generate() loop overrides `top` for tall columns so
            // the spire shafts read as solid crystal rather than sand.
            Biome::CrystalSpires => (BlockType::GlowSand, BlockType::Crystal, BlockType::Crystal),
            // Volcanic basalt waste â€” lava channel handling is special-
            // cased in generate() (see VolcanicWaste branch below).
            Biome::VolcanicWaste => (BlockType::Basalt, BlockType::Basalt, BlockType::Basalt),
            // Glacier ridges: snow cap, ice body, stone deep base.
            Biome::GlacierShards => (BlockType::Snow, BlockType::Ice, BlockType::Stone),
            // Alien reef: magenta moss surface, bone rock for pillars.
            Biome::AlienReef => (
                BlockType::AlienMoss,
                BlockType::BoneRock,
                BlockType::BoneRock,
            ),
        }
    }

    /// Y-banded sub-surface block for Mesa biome â€” produces the
    /// horizontal red/buff/dark stripes that define real-world
    /// canyon cliff faces. Pure function of world Y so adjacent
    /// columns line up perfectly into continuous bands.
    fn mesa_band(wy: i32) -> BlockType {
        frontier::strata_block(wy)
    }

    /// Deep body block for a column.
    ///
    /// Most of the frontier's rock is banded: violet, brick, ochre and
    /// buff stripes running dead level across every cliff, canyon wall
    /// and cave roof, exactly as in the key art. Only the biomes with
    /// their own strong material identity (crystal, basalt, ice, bone)
    /// keep a solid core, so their silhouettes stay readable.
    fn core_block(biome: Biome, core: BlockType, wy: i32) -> BlockType {
        match biome {
            Biome::CrystalSpires
            | Biome::VolcanicWaste
            | Biome::GlacierShards
            | Biome::AlienReef
            | Biome::Ocean => core,
            _ => frontier::strata_block(wy),
        }
    }

    fn surface_detail_block(
        &self,
        biome: Biome,
        current: BlockType,
        slope: i32,
        wx: i32,
        wz: i32,
    ) -> BlockType {
        let r = column_rand(self.seed ^ 0xA17E_577, wx, wz);
        let grain = self
            .hills_b
            .get([wx as f64 * 0.033 + 19.0, wz as f64 * 0.033 - 31.0]);

        match biome {
            Biome::Plains | Biome::Forest => {
                if slope <= 1 && r < 0.045 {
                    BlockType::Dirt
                } else if grain > 0.46 && r < 0.080 {
                    BlockType::MossStone
                } else {
                    current
                }
            }
            Biome::Jungle => {
                if slope <= 1 && r < 0.090 {
                    BlockType::MossStone
                } else {
                    current
                }
            }
            Biome::Beach | Biome::Desert | Biome::Savanna => current,
            Biome::Tundra => {
                if r < 0.10 {
                    BlockType::Snow
                } else if r < 0.16 {
                    BlockType::Gravel
                } else {
                    current
                }
            }
            Biome::Mountains | Biome::SnowyMountains => {
                if r < 0.18 {
                    BlockType::Gravel
                } else if matches!(biome, Biome::SnowyMountains) && r < 0.34 {
                    BlockType::Snow
                } else {
                    current
                }
            }
            Biome::Mesa => {
                // Mesa tables in the key art are not bare rock: their
                // flat tops carry a vivid green skin that stops dead at
                // the cliff edge. Slope gates it, so the banded cliff
                // faces stay bare while every plateau reads as living
                // ground you would want to land a shuttle on.
                if slope <= 1 && grain > 0.10 {
                    if r < 0.10 {
                        BlockType::MossStone
                    } else {
                        BlockType::Grass
                    }
                } else if r < 0.10 {
                    BlockType::MesaClay
                } else if grain > 0.50 && r < 0.20 {
                    BlockType::AmberStone
                } else {
                    current
                }
            }
            Biome::Karst => {
                if grain > 0.36 && r < 0.18 {
                    BlockType::Limestone
                } else {
                    current
                }
            }
            Biome::CrystalSpires => {
                if r < 0.16 {
                    BlockType::LuminiteCrystal
                } else if r < 0.26 {
                    BlockType::Crystal
                } else {
                    current
                }
            }
            Biome::VolcanicWaste => {
                if grain > 0.58 && r < 0.18 {
                    BlockType::Lava
                } else {
                    current
                }
            }
            Biome::GlacierShards => {
                if r < 0.20 {
                    BlockType::Ice
                } else {
                    current
                }
            }
            Biome::AlienReef => {
                if r < 0.14 {
                    BlockType::IridiumVein
                } else if r < 0.24 {
                    BlockType::BoneRock
                } else {
                    current
                }
            }
            Biome::Ocean => current,
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
                let (mut surface, cont) = self.surface_height(wx as f64, wz as f64);

                // --------- Ocean isolation cleanup ---------
                // A single column sticking 1-2 blocks out of the water
                // with all 4 cardinal neighbours submerged is a floating
                // sand pebble â€” jarring and unrealistic. Pull it back
                // beneath the water line so oceans look clean. We only
                // fix the "just-barely-above-water" band; real islands
                // rise > 3 blocks above water and have at least one
                // land neighbour.
                if surface <= WATER_LEVEL + 2 && surface >= WATER_LEVEL {
                    let (hn, _) = self.surface_height(wx as f64, (wz - 1) as f64);
                    let (hs, _) = self.surface_height(wx as f64, (wz + 1) as f64);
                    let (he, _) = self.surface_height((wx + 1) as f64, wz as f64);
                    let (hw, _) = self.surface_height((wx - 1) as f64, wz as f64);
                    let land_neighbours = (hn >= WATER_LEVEL) as i32
                        + (hs >= WATER_LEVEL) as i32
                        + (he >= WATER_LEVEL) as i32
                        + (hw >= WATER_LEVEL) as i32;
                    if land_neighbours == 0 {
                        // Floating island: submerge it.
                        surface = WATER_LEVEL - 1;
                    }
                }

                // --------- Slope analysis ---------
                // Compute the local gradient from 4 cardinal height
                // samples. The slope (max Î”height across neighbours) is
                // what makes the difference between "grass on a shallow
                // hill" and "cliff face of exposed rock". Steep columns
                // drop grass â†’ stone for the surface block, producing
                // believable cliffs, scree lines on mountainsides, and
                // dirt streaks where slopes transition.
                let (hn, _) = self.surface_height(wx as f64, (wz - 1) as f64);
                let (hs, _) = self.surface_height(wx as f64, (wz + 1) as f64);
                let (he, _) = self.surface_height((wx + 1) as f64, wz as f64);
                let (hw, _) = self.surface_height((wx - 1) as f64, wz as f64);
                let slope = (surface - hn)
                    .abs()
                    .max((surface - hs).abs())
                    .max((surface - he).abs())
                    .max((surface - hw).abs());

                let biome = self.biome(wx as f64, wz as f64, surface, cont);
                let (mut top, sub, core) = Self::blocks_for(biome);

                // --------- Energy river ---------
                // `surface_height` already cut the channel; here we work
                // out what pools in it. The gate has to be on the height
                // BEFORE the cut, exactly as in `surface_height`, or a
                // channel cut near the sea line would be left as a dry
                // trench because the carve pushed it under the threshold.
                let river = self
                    .frontier
                    .rivers
                    .column(wx, wz)
                    .filter(|r| surface + r.cut > WATER_LEVEL + 4);
                let river_fill_top = river.map(|r| r.fluid_top(surface));

                // --------- Skyway ---------
                // Decks ride the smooth macro elevation, so a single
                // route stays level while the ground below it drops into
                // a canyon (bridge) or heaves into a mesa (cutting).
                let skyway =
                    self.frontier
                        .skyways
                        .column(wx, wz, self.macro_height(wx as f64, wz as f64));
                let skyway_lamp = frontier::skyway_lamp(wx, wz);

                // Crystal Spires: tall columns ARE the spires, so their
                // top block must be Crystal (not the GlowSand floor).
                // Threshold = floor + 6 blocks: anything above that is
                // a pillar shaft.
                if biome == Biome::CrystalSpires && surface > WATER_LEVEL + 16 {
                    top = BlockType::Crystal;
                }

                // Volcanic Waste: per-column lava-fill level. Lowered
                // to 52 (just above water level) so lava fills only
                // deep channels, not the whole basin \u2014 keeps the
                // biome walkable and doesn't blind the player with a
                // sea of emissive blocks.
                let volcanic_lava_level: i32 = 52;
                let in_volcanic = biome == Biome::VolcanicWaste;

                // Slope override â€” only applies to biomes where it makes
                // visual sense (don't turn the desert or canyon into
                // grey cliffs; canyons already ARE cliffs and should
                // stay red).
                let slope_overrides = matches!(
                    biome,
                    Biome::Plains
                        | Biome::Forest
                        | Biome::Jungle
                        | Biome::Savanna
                        | Biome::Tundra
                        | Biome::Mountains
                );
                let (top, sub) = if slope_overrides && slope >= 4 {
                    // Steep cliff: bare stone surface, stone sub-layer.
                    (BlockType::Stone, BlockType::Stone)
                } else if slope_overrides && slope >= 2 {
                    // Sloped transition: dirt breaks through grass,
                    // sub-layer is still dirt â†’ gravel feel on hillsides.
                    (BlockType::Dirt, BlockType::Gravel)
                } else {
                    (top, sub)
                };
                let top = self.surface_detail_block(biome, top, slope, wx, wz);

                for ly in 0..CHUNK_SIZE {
                    let wy = cy * CHUNK_SIZE_I + ly as i32;

                    // Bedrock at the bottom of the world.
                    if wy <= BEDROCK_LEVEL {
                        chunk.set(lx, ly, lz, BlockType::Bedrock.into());
                        continue;
                    }

                    // The skyway wins against everything: it is the one
                    // structure the player is meant to drive along, so a
                    // deck must never be swallowed by the mesa it cuts
                    // through, and its headroom must never be filled in.
                    if let Some(way) = skyway {
                        if let Some(block) = way.deck_block(wy, skyway_lamp) {
                            chunk.set(lx, ly, lz, block.into());
                            continue;
                        }
                        if wy > way.deck_y && wy <= way.deck_y + SkywayNetwork::CLEARANCE {
                            continue;
                        }
                        // Pylons: fill the gap from the deck underside
                        // down to whatever ground is beneath, so bridges
                        // over a canyon stand on legs instead of hanging.
                        if way.pylon && wy < way.deck_y - 1 && wy > surface {
                            let block = if wy.rem_euclid(6) == 0 {
                                BlockType::PlatingTeal
                            } else {
                                BlockType::PlatingWhite
                            };
                            chunk.set(lx, ly, lz, block.into());
                            continue;
                        }
                    }

                    // Above the surface: air, water, lava, or the glowing
                    // fluid standing in an energy channel.
                    if wy > surface {
                        if river_fill_top.is_some_and(|fill| wy <= fill) {
                            let fluid = river.map(|r| r.fluid).unwrap_or(BlockType::PlasmaFlow);
                            chunk.set(lx, ly, lz, fluid.into());
                        } else if in_volcanic && wy <= volcanic_lava_level {
                            chunk.set(lx, ly, lz, BlockType::Lava.into());
                        } else if wy <= WATER_LEVEL {
                            chunk.set(lx, ly, lz, BlockType::Water.into());
                        }
                        // else: leave as AIR (default-initialised).
                        continue;
                    }

                    // Carve caves â€” never inside the top layer (preserves
                    // the surface skin) and never near the water line so
                    // oceans don't drain through holes. Keep a generous
                    // 14-block buffer below the surface so caves can never
                    // open up straight onto a beach or plain, and so even
                    // steep cliff edges (Î”surface â‰¤ 14 blocks between
                    // adjacent columns) don't expose cave tunnels from the
                    // side. Also keep a wide buffer around WATER_LEVEL
                    // (Â±6) so sub-surface aquifers never punch through
                    // beaches or shallow seabeds.
                    let cave_allowed = wy < surface - 14
                        && wy > BEDROCK_LEVEL + 2
                        && (wy < WATER_LEVEL - 6 || wy > WATER_LEVEL + 6);
                    if cave_allowed && self.is_cave(wx as f64, wy as f64, wz as f64) {
                        continue;
                    }

                    // Big horizontal tunnels â€” carve through mountains
                    // and ridges. Allowed ANYWHERE below a thin 4-block
                    // surface skin so tunnel mouths can open on cliff
                    // faces (cool!) but we never break a beach/seabed.
                    // The tunnel mouth only shows on cliffs steep
                    // enough that the skin is opened from the side.
                    let tunnel_allowed = wy < surface - 4
                        && wy > BEDROCK_LEVEL + 2
                        && (wy < WATER_LEVEL - 4 || wy > WATER_LEVEL + 4);
                    if tunnel_allowed && self.is_tunnel(wx as f64, wy as f64, wz as f64) {
                        continue;
                    }

                    // Rare giant caverns â€” only deep underground so
                    // they never collapse the surface.
                    if wy < surface - 30
                        && wy > BEDROCK_LEVEL + 2
                        && self.is_cavern(wx as f64, wy as f64, wz as f64)
                    {
                        continue;
                    }

                    let depth = surface - wy;
                    let block = if depth == 0 {
                        // A channel bed is scorched by whatever runs
                        // through it, not grassed over.
                        match river.map(|r| r.fluid) {
                            Some(BlockType::Lava) => BlockType::Basalt,
                            Some(_) => BlockType::GlowSand,
                            None => top,
                        }
                    } else if depth <= 3 {
                        sub
                    } else if matches!(biome, Biome::Mesa) {
                        // Mesa cliff faces show horizontal red/buff
                        // sedimentary banding â€” pure function of Y so
                        // adjacent columns line up into continuous
                        // stripes the player can read as geology.
                        Self::mesa_band(wy)
                    } else {
                        Self::core_block(biome, core, wy)
                    };
                    chunk.set(lx, ly, lz, block.into());
                }
            }
        }

        chunk.dirty = true;
        // Decorate AFTER the main fill so trees see the final surface.
        self.decorate(chunk);
        // Landmarks last: sky islands, docking stations and crystal
        // clusters are allowed to overwrite decoration, and several of
        // them straddle chunk borders, so they must be stamped from the
        // shared world-space lattice rather than from chunk-local rolls.
        self.frontier.stamp_landmarks(
            chunk,
            |x, z| self.surface_height_at(x, z),
            |x, z| self.macro_height(x as f64, z as f64).round() as i32,
        );
        chunk.finalize_uniform_flags();
    }

    /// Place trees, flowers, rocks inside the chunk. Only features that
    /// fit entirely inside the chunk are placed â€” cross-chunk trees would
    /// need a deferred population pass and we want to keep the terrain
    /// generator cleanly per-chunk. The result is still dense forests
    /// (16Ã—16 column is wide enough for many trees) with ~1-block gaps
    /// at chunk seams, which is visually imperceptible at normal render
    /// distance.
    fn decorate(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        let origin_y = cy * CHUNK_SIZE_I;

        // Margin so leaf canopy (radius 2) never pokes out horizontally.
        for lz in 2..(CHUNK_SIZE - 2) {
            for lx in 2..(CHUNK_SIZE - 2) {
                let wx = cx * CHUNK_SIZE_I + lx as i32;
                let wz = cz * CHUNK_SIZE_I + lz as i32;
                let (surface, cont) = self.surface_height(wx as f64, wz as f64);
                let biome = self.biome(wx as f64, wz as f64, surface, cont);

                // Trees can't grow on cliffs / steep slopes. Same slope
                // test as the main generator â€” if any cardinal neighbour
                // is >= 3 blocks lower/higher, skip.
                let (hn, _) = self.surface_height(wx as f64, (wz - 1) as f64);
                let (hs, _) = self.surface_height(wx as f64, (wz + 1) as f64);
                let (he, _) = self.surface_height((wx + 1) as f64, wz as f64);
                let (hw, _) = self.surface_height((wx - 1) as f64, wz as f64);
                let slope = (surface - hn)
                    .abs()
                    .max((surface - hs).abs())
                    .max((surface - he).abs())
                    .max((surface - hw).abs());
                if slope >= 3 {
                    continue;
                }

                // Only forest biomes get trees. Density varies per biome.
                let density = match biome {
                    Biome::Forest => 0.06,
                    Biome::Jungle => 0.14,
                    Biome::Plains => 0.008,
                    Biome::Savanna => 0.010,
                    Biome::Tundra => 0.005,
                    _ => 0.0,
                };
                if density == 0.0 {
                    continue;
                }

                // Deterministic hash-based "random" per column.
                let r = column_rand(self.seed, wx, wz);
                if r > density {
                    continue;
                }

                // Surface must be inside this chunk (we only place the
                // trunk base at `surface + 1`, above the top block).
                let base_y = surface + 1;
                if base_y < origin_y || base_y >= origin_y + CHUNK_SIZE_I {
                    continue;
                }
                // And we need room for the whole tree above.
                let (trunk_h, leaf_kind) = match biome {
                    Biome::Jungle => (7 + ((r * 997.0) as i32 % 4), BlockType::JungleLeaves),
                    Biome::Forest => (5 + ((r * 997.0) as i32 % 3), BlockType::Leaves),
                    Biome::Savanna => (4, BlockType::Leaves),
                    Biome::Tundra => (3, BlockType::Leaves),
                    _ => (4 + ((r * 997.0) as i32 % 2), BlockType::Leaves),
                };
                let top_y = base_y + trunk_h + 2; // +2 for canopy above trunk
                if top_y >= origin_y + CHUNK_SIZE_I {
                    continue;
                }

                // Don't plant on water/sand (no trees on beaches).
                let surface_ly = (surface - origin_y) as i32;
                if surface_ly < 0 || surface_ly >= CHUNK_SIZE_I {
                    // Surface block sits in a different chunk: skip to
                    // avoid floating trees when the ground chunk below
                    // turns out to be water/sand. Safer to skip.
                    continue;
                }
                let ground = chunk.get(lx, surface_ly as usize, lz);
                if ground != <BlockType as Into<Voxel>>::into(BlockType::Grass)
                    && ground != <BlockType as Into<Voxel>>::into(BlockType::SavannaGrass)
                    && ground != <BlockType as Into<Voxel>>::into(BlockType::TundraGrass)
                {
                    continue;
                }

                // Trunk.
                for dy in 0..trunk_h {
                    let ly = (base_y + dy - origin_y) as usize;
                    chunk.set(lx, ly, lz, BlockType::Wood.into());
                }

                // Canopy: 5Ã—5 at two middle levels, 3Ã—3 above, 1 on top.
                let crown_y = base_y + trunk_h - 1;
                for (radius, layer) in [(2i32, 0i32), (2, 1), (1, 2), (0, 3)] {
                    let ly_world = crown_y + layer;
                    if ly_world < origin_y || ly_world >= origin_y + CHUNK_SIZE_I {
                        continue;
                    }
                    let ly = (ly_world - origin_y) as usize;
                    for dz in -radius..=radius {
                        for dx in -radius..=radius {
                            // Slight corner trimming for a rounder crown.
                            if dx.abs() == radius && dz.abs() == radius && radius == 2 {
                                continue;
                            }
                            let nx = (lx as i32 + dx) as usize;
                            let nz = (lz as i32 + dz) as usize;
                            if nx >= CHUNK_SIZE || nz >= CHUNK_SIZE {
                                continue;
                            }
                            // Don't overwrite the trunk itself.
                            if chunk.get(nx, ly, nz) == AIR {
                                chunk.set(nx, ly, nz, leaf_kind.into());
                            }
                        }
                    }
                }
            }
        }

        // ----------------------- Structures -------------------------
        // A second pass for the "cool stuff": natural stone arches,
        // ruined pillar clusters, and boulder piles in rocky biomes.
        // Deterministic per-seed, chunk-local, no cross-chunk writes.
        self.decorate_structures(chunk);

        // ----------------------- Futuristic Cities ------------------
        // Rare skyscraper districts that flatten local terrain and
        // scatter sci-fi towers with glowing crystal crowns.
        self.try_place_city(chunk);

        // ----------------------- Flora Scatter ----------------------
        // Dense pass: flowers, tall grass, cacti, bushes, pebbles,
        // kelp, coral, crystals, glowing moss â€” whatever fits the
        // biome. Runs on every column (including margins) at high
        // density so no area ever feels bare.
        self.decorate_flora(chunk);
    }

    /// Low-density single-block tufts for atmosphere. Deliberately
    /// sparse so the ground stays smooth and walkable â€” the player
    /// must never have to jump over decoration. No 2-tall stacks.
    fn decorate_flora(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        let origin_y = cy * CHUNK_SIZE_I;

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = cx * CHUNK_SIZE_I + lx as i32;
                let wz = cz * CHUNK_SIZE_I + lz as i32;
                let (surface, cont) = self.surface_height(wx as f64, wz as f64);
                let biome = self.biome(wx as f64, wz as f64, surface, cont);
                let surface_ly = surface - origin_y;
                let above_ly = surface_ly + 1;
                if surface_ly < 0 || surface_ly >= CHUNK_SIZE_I {
                    continue;
                }
                if above_ly < 0 || above_ly >= CHUNK_SIZE_I {
                    continue;
                }
                let r = column_rand(self.seed ^ 0xF107A, wx, wz);
                let ground = chunk.get(lx, surface_ly as usize, lz);
                let above_slot = chunk.get(lx, above_ly as usize, lz);
                if above_slot != AIR {
                    continue;
                }
                // Skip entirely if any of the 4 cardinal neighbours is
                // lower than the current surface â€” avoids placing
                // flora on cliff edges where it would look floating.
                // This also keeps the ground visually calmer.

                let is_grass_ground = ground == <BlockType as Into<Voxel>>::into(BlockType::Grass)
                    || ground == <BlockType as Into<Voxel>>::into(BlockType::SavannaGrass)
                    || ground == <BlockType as Into<Voxel>>::into(BlockType::TundraGrass);
                let is_sand_ground = ground == <BlockType as Into<Voxel>>::into(BlockType::Sand)
                    || ground == <BlockType as Into<Voxel>>::into(BlockType::GlowSand)
                    || ground == <BlockType as Into<Voxel>>::into(BlockType::RedSand);

                // One single-block tuft per biome, very sparse.
                // Densities chosen so the ground reads as "populated"
                // but never as "obstacle course".
                match biome {
                    Biome::Plains => {
                        if is_grass_ground && r < 0.012 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Leaves.into());
                        }
                    }
                    Biome::Forest => {
                        if is_grass_ground && r < 0.020 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Leaves.into());
                        }
                    }
                    Biome::Jungle => {
                        if is_grass_ground && r < 0.035 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::JungleLeaves.into());
                        }
                    }
                    Biome::Savanna => {
                        if is_grass_ground && r < 0.010 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::SavannaGrass.into());
                        }
                    }
                    Biome::Desert => {
                        if is_sand_ground && r < 0.002 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Leaves.into());
                        }
                    }
                    Biome::Tundra => {
                        if is_grass_ground && r < 0.015 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::TundraGrass.into());
                        }
                    }
                    Biome::SnowyMountains | Biome::Mountains => {
                        if r < 0.008 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Gravel.into());
                        }
                    }
                    Biome::Mesa => {
                        // Green mesa tables get scrub; the bare banded
                        // ledges get crystal glitter instead, so a cliff
                        // shoulder still catches the light.
                        if is_grass_ground && r < 0.030 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Leaves.into());
                        } else if is_grass_ground && r < 0.042 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::MossStone.into());
                        } else if r < 0.006 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Crystal.into());
                        } else if r < 0.009 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::AmberStone.into());
                        }
                    }
                    Biome::Karst => {
                        if is_grass_ground && r < 0.020 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::MossStone.into());
                        }
                    }
                    Biome::Beach => {
                        if is_sand_ground && r < 0.003 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Gravel.into());
                        }
                    }
                    Biome::Ocean => {
                        // Kelp stays below water â€” single-block coral
                        // patches only, no tall stalks that block
                        // visibility.
                        if is_sand_ground && surface < WATER_LEVEL - 2 && r < 0.06 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::MossStone.into());
                        }
                    }
                    Biome::CrystalSpires => {
                        if r < 0.010 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Crystal.into());
                        } else if r < 0.040 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::AlienMoss.into());
                        }
                    }
                    Biome::VolcanicWaste => {
                        if r < 0.012 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Basalt.into());
                        }
                    }
                    Biome::GlacierShards => {
                        if r < 0.010 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Ice.into());
                        }
                    }
                    Biome::AlienReef => {
                        if r < 0.030 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::AlienMoss.into());
                        } else if r < 0.040 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::BoneRock.into());
                        }
                    }
                }
            }
        }

        // Dense sci-fi micro-props pass â€” see `decorate_props`. Runs
        // after flora so we overwrite bland grass tufts with neon
        // pylons, crates and holo-antennas where they land.
        self.decorate_props(chunk);
        self.decorate_micro_specks(chunk);
    }

    /// Populate the surface with small detailed sci-fi structures
    /// (2-6 block voxel props): neon pylons, cargo crates, holo
    /// antennas, warning barriers, landing-pad tiles and energy
    /// conduits. Every structure is defined in chunk-local space and
    /// clipped to chunk bounds so there's no cross-chunk coordination
    /// needed. Density is deliberately high in alien biomes and
    /// moderate in plains/savanna so the world reads as "inhabited
    /// frontier outpost" rather than "empty Minecraft field".
    fn decorate_props(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        let origin_y = cy * CHUNK_SIZE_I;

        // Helper: safe set that ignores out-of-chunk + non-AIR slots.
        let _ = origin_y; // used by helpers below via closure

        // We roll up to ~12 prop candidates per chunk. Each candidate
        // picks a deterministic position + kind from a hash stream.
        const CANDIDATES: usize = 24;
        for i in 0..CANDIDATES {
            let r_pos = column_rand(self.seed ^ (0xF00D_FACE + i as u32 * 7919), cx, cz);
            let r_kind = column_rand(self.seed ^ (0xBEEF_BABE + i as u32 * 104_729), cx, cz);
            let r_gate = column_rand(self.seed ^ (0x1234_5678 + i as u32 * 31), cx, cz);

            let lx = ((r_pos * 65537.0) as usize) % CHUNK_SIZE;
            let lz = ((r_pos * 997.0) as usize) % CHUNK_SIZE;
            let wx = cx * CHUNK_SIZE_I + lx as i32;
            let wz = cz * CHUNK_SIZE_I + lz as i32;
            let (surface, cont) = self.surface_height(wx as f64, wz as f64);
            let biome = self.biome(wx as f64, wz as f64, surface, cont);

            // Density gate per biome. Alien biomes get lots of props;
            // forests/jungles get very few (preserve wilderness).
            // The frontier is inhabited everywhere, so every land biome
            // gets outpost clutter. Forest and jungle stay lowest: they
            // are the wilderness the outposts are cut out of, and the
            // canopy hides most of a prop anyway.
            // These are per-candidate odds and there are 24 candidates a
            // chunk, so the numbers are much smaller than they look: at
            // 0.02 a chunk averages half a prop. Anything near 0.05 puts
            // one outpost in every chunk, which at render distance is a
            // field of glowing litter rather than a frontier.
            let density: f64 = match biome {
                Biome::CrystalSpires => 0.055,
                Biome::AlienReef => 0.050,
                Biome::VolcanicWaste => 0.030,
                Biome::GlacierShards => 0.026,
                Biome::Mesa => 0.022,
                Biome::Karst => 0.014,
                Biome::Plains | Biome::Savanna | Biome::Desert | Biome::Tundra => 0.014,
                Biome::Mountains | Biome::SnowyMountains => 0.010,
                Biome::Forest | Biome::Jungle => 0.006,
                Biome::Ocean | Biome::Beach => 0.0,
            };
            if r_gate > density {
                continue;
            }

            // Slope test â€” props only on reasonably flat ground.
            let (hn, _) = self.surface_height(wx as f64, (wz - 1) as f64);
            let (hs, _) = self.surface_height(wx as f64, (wz + 1) as f64);
            let (he, _) = self.surface_height((wx + 1) as f64, wz as f64);
            let (hw, _) = self.surface_height((wx - 1) as f64, wz as f64);
            let slope = (surface - hn)
                .abs()
                .max((surface - hs).abs())
                .max((surface - he).abs())
                .max((surface - hw).abs());
            if slope >= 2 {
                continue;
            }

            let base_y = surface + 1;
            if base_y < origin_y || base_y >= origin_y + CHUNK_SIZE_I {
                continue;
            }

            // Pick a prop shape from the kind roll â€” each prop is a
            // tightly-packed blueprint of (dx, dy, dz, block) offsets
            // from the base column. All small enough to fit in-chunk
            // with the margin we enforce below.
            let kind = {
                let base = (r_kind * 100.0) as u32 % 10;
                if biome != Biome::CrystalSpires {
                    base
                } else {
                    // Weight toward mushroom caps + crystal gardens so the biome
                    // reads closer to reference art (organic neon fungi silhouettes).
                    let u = (column_rand(self.seed ^ (0xC0FFEE_u32 + i as u32 * 97), cx, cz)
                        * 100.0) as u32
                        % 100;
                    if u < 24 {
                        6
                    } else if u < 46 {
                        7
                    } else if u < 60 {
                        4
                    } else if u < 72 {
                        5
                    } else {
                        base
                    }
                }
            };
            match (biome, kind) {
                // --- CRYSTAL SPIRES ------------------------------------
                (Biome::CrystalSpires, 0) | (Biome::AlienReef, 0) => {
                    // Neon pylon: 1x4 crystal column on a bone-rock base,
                    // crowned with a glowing dot. Like reference image.
                    set_safe(chunk, lx, base_y, lz, BlockType::BoneRock, origin_y);
                    set_safe(
                        chunk,
                        lx,
                        base_y + 1,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                    set_safe(chunk, lx, base_y + 2, lz, BlockType::Crystal, origin_y);
                    set_safe(chunk, lx, base_y + 3, lz, BlockType::IridiumVein, origin_y);
                    set_safe(
                        chunk,
                        lx,
                        base_y + 4,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }
                (Biome::CrystalSpires, 1 | 2) => {
                    // Crystal cluster: 5-block asymmetric sparkle.
                    set_safe(chunk, lx, base_y, lz, BlockType::LuminiteCrystal, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::Crystal, origin_y);
                    set_safe(chunk, lx + 1, base_y, lz, BlockType::MagnetiteOre, origin_y);
                    set_safe(chunk, lx, base_y, lz + 1, BlockType::Crystal, origin_y);
                    set_safe(
                        chunk,
                        lx,
                        base_y + 2,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }
                (Biome::CrystalSpires, 4 | 5) => {
                    // Resource garden: saturated crystal cluster with
                    // cyan/magenta tips, dense enough to read at flight speed.
                    for dx in -1..=1 {
                        for dz in -1..=1 {
                            let nx = lx as i32 + dx;
                            let nz = lz as i32 + dz;
                            if nx < 0 || nz < 0 {
                                continue;
                            }
                            let h = 2 + ((dx * 31 + dz * 17).abs() % 4);
                            for dy in 0..h {
                                let block = match (dx + dz + dy).rem_euclid(5) {
                                    0 => BlockType::LuminiteCrystal,
                                    1 => BlockType::MagnetiteOre,
                                    2 => BlockType::IridiumVein,
                                    3 => BlockType::Crystal,
                                    _ => BlockType::Crystal,
                                };
                                set_safe(
                                    chunk,
                                    nx as usize,
                                    base_y + dy,
                                    nz as usize,
                                    block,
                                    origin_y,
                                );
                            }
                            set_safe(
                                chunk,
                                nx as usize,
                                base_y + h,
                                nz as usize,
                                BlockType::LuminiteCrystal,
                                origin_y,
                            );
                        }
                    }
                }
                (Biome::CrystalSpires, 6 | 7) => {
                    // Giant mushroom landmark — same silhouette class as AlienReef,
                    // but cap reads cyan/crystal (cockpit key art).
                    for dy in 0..5 {
                        set_safe(chunk, lx, base_y + dy, lz, BlockType::BoneRock, origin_y);
                    }
                    let cap_y = base_y + 5;
                    for dx in -2..=2 {
                        for dz in -2..=2 {
                            let nx = lx as i32 + dx;
                            let nz = lz as i32 + dz;
                            if nx < 0 || nz < 0 {
                                continue;
                            }
                            let dist = dx.abs().max(dz.abs());
                            if dist <= 2 {
                                let block = if dist == 2 {
                                    BlockType::Crystal
                                } else {
                                    BlockType::LuminiteCrystal
                                };
                                set_safe(chunk, nx as usize, cap_y, nz as usize, block, origin_y);
                            }
                            if dist <= 1 {
                                set_safe(
                                    chunk,
                                    nx as usize,
                                    cap_y - 1,
                                    nz as usize,
                                    if (dx + dz).rem_euclid(2) == 0 {
                                        BlockType::LuminiteCrystal
                                    } else {
                                        BlockType::Crystal
                                    },
                                    origin_y,
                                );
                            }
                        }
                    }
                    set_safe(
                        chunk,
                        lx,
                        cap_y + 1,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }
                (Biome::CrystalSpires, 3) | (Biome::AlienReef, 3) => {
                    // Holo-antenna: 4-block thin mast with cyan tip.
                    for dy in 0..3 {
                        set_safe(chunk, lx, base_y + dy, lz, BlockType::BoneRock, origin_y);
                    }
                    set_safe(
                        chunk,
                        lx,
                        base_y + 3,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                    if lx + 1 < CHUNK_SIZE {
                        set_safe(chunk, lx + 1, base_y + 2, lz, BlockType::Crystal, origin_y);
                    }
                    if lx >= 1 {
                        set_safe(chunk, lx - 1, base_y + 2, lz, BlockType::Crystal, origin_y);
                    }
                }

                // --- ALIEN REEF ----------------------------------------
                (Biome::AlienReef, 1 | 2) => {
                    // Purple bioluminescent coral fan.
                    set_safe(chunk, lx, base_y, lz, BlockType::AlienMoss, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::AlienMoss, origin_y);
                    if lx + 1 < CHUNK_SIZE {
                        set_safe(
                            chunk,
                            lx + 1,
                            base_y + 1,
                            lz,
                            BlockType::AlienMoss,
                            origin_y,
                        );
                    }
                    if lz + 1 < CHUNK_SIZE {
                        set_safe(
                            chunk,
                            lx,
                            base_y + 1,
                            lz + 1,
                            BlockType::AlienMoss,
                            origin_y,
                        );
                    }
                    set_safe(chunk, lx, base_y + 2, lz, BlockType::Crystal, origin_y);
                }
                (Biome::AlienReef, 4 | 5) => {
                    // Large neon mushroom: dark organic stem, broad cap,
                    // bright underside dots. This is the main reference
                    // silhouette from the cockpit image.
                    for dy in 0..5 {
                        set_safe(chunk, lx, base_y + dy, lz, BlockType::BoneRock, origin_y);
                    }
                    let cap_y = base_y + 5;
                    for dx in -2..=2 {
                        for dz in -2..=2 {
                            let nx = lx as i32 + dx;
                            let nz = lz as i32 + dz;
                            if nx < 0 || nz < 0 {
                                continue;
                            }
                            let dist = dx.abs().max(dz.abs());
                            if dist <= 2 {
                                let block = if dist == 2 {
                                    BlockType::Crystal
                                } else {
                                    BlockType::AlienMoss
                                };
                                set_safe(chunk, nx as usize, cap_y, nz as usize, block, origin_y);
                            }
                            if dist <= 1 {
                                set_safe(
                                    chunk,
                                    nx as usize,
                                    cap_y - 1,
                                    nz as usize,
                                    if (dx + dz).rem_euclid(2) == 0 {
                                        BlockType::MagnetiteOre
                                    } else {
                                        BlockType::Crystal
                                    },
                                    origin_y,
                                );
                            }
                        }
                    }
                    set_safe(
                        chunk,
                        lx,
                        cap_y + 1,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }
                (Biome::AlienReef, 6 | 7) => {
                    // Short bone-and-neon arch. This creates flight
                    // corridors and strong silhouettes without needing
                    // cross-chunk structures.
                    for dx in -2..=2 {
                        let nx = lx as i32 + dx;
                        if nx < 0 {
                            continue;
                        }
                        let edge = dx.abs() == 2;
                        let h = if edge { 5 } else { 3 };
                        for dy in 0..h {
                            let block = if edge {
                                BlockType::BoneRock
                            } else {
                                BlockType::Crystal
                            };
                            set_safe(chunk, nx as usize, base_y + dy, lz, block, origin_y);
                        }
                        let cap = if edge {
                            BlockType::MagnetiteOre
                        } else {
                            BlockType::LuminiteCrystal
                        };
                        set_safe(chunk, nx as usize, base_y + h, lz, cap, origin_y);
                    }
                }
                (Biome::AlienReef, 8 | 9) | (Biome::CrystalSpires, 8 | 9) => {
                    // Mini landing pad / tech plate: dark center,
                    // cyan-magenta rim, amber corner lights.
                    for dx in -2..=2 {
                        for dz in -2..=2 {
                            let nx = lx as i32 + dx;
                            let nz = lz as i32 + dz;
                            if nx < 0 || nz < 0 {
                                continue;
                            }
                            let edge = dx.abs() == 2 || dz.abs() == 2;
                            let corner = dx.abs() == 2 && dz.abs() == 2;
                            let block = if corner {
                                BlockType::MagnetiteOre
                            } else if edge {
                                if (dx + dz).rem_euclid(2) == 0 {
                                    BlockType::LuminiteCrystal
                                } else {
                                    BlockType::Crystal
                                }
                            } else {
                                BlockType::Basalt
                            };
                            set_safe(chunk, nx as usize, base_y, nz as usize, block, origin_y);
                        }
                    }
                }

                // --- VOLCANIC WASTE -----------------------------------
                (Biome::VolcanicWaste, _) => {
                    // Obsidian drill rig: 2-wide basalt pedestal with
                    // a lava core glowing inside.
                    set_safe(chunk, lx, base_y, lz, BlockType::Basalt, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::Basalt, origin_y);
                    set_safe(chunk, lx, base_y + 2, lz, BlockType::Lava, origin_y);
                    if lx + 1 < CHUNK_SIZE {
                        set_safe(chunk, lx + 1, base_y, lz, BlockType::Basalt, origin_y);
                        set_safe(chunk, lx + 1, base_y + 1, lz, BlockType::Basalt, origin_y);
                    }
                }

                // --- GLACIER SHARDS -----------------------------------
                (Biome::GlacierShards, _) => {
                    // Ice sensor spike: 4-tall ice with a glow crown.
                    for dy in 0..3 {
                        set_safe(chunk, lx, base_y + dy, lz, BlockType::Ice, origin_y);
                    }
                    set_safe(
                        chunk,
                        lx,
                        base_y + 3,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }

                // --- MESA / MOUNTAIN / KARST / FOREST -----------------
                // Banded canyon country is where the player spends most
                // of their time, so it gets the richest outpost kit: lit
                // signage, plated pads and plasma conduits in the same
                // palette as the skyways that run overhead.
                (
                    Biome::Mesa
                    | Biome::Karst
                    | Biome::Mountains
                    | Biome::SnowyMountains
                    | Biome::Forest
                    | Biome::Jungle,
                    0 | 1 | 2,
                ) => {
                    // Holo billboard: a plated post carrying a lit pane,
                    // the neon signage that dots the cliffs in the art.
                    for dy in 0..3 {
                        set_safe(
                            chunk,
                            lx,
                            base_y + dy,
                            lz,
                            BlockType::PlatingWhite,
                            origin_y,
                        );
                    }
                    for dy in 3..6 {
                        for dx in 0..3 {
                            let nx = lx + dx;
                            if nx >= CHUNK_SIZE {
                                continue;
                            }
                            let block = if dy == 3 || dx == 2 {
                                BlockType::NeonMagenta
                            } else {
                                BlockType::HoloPanel
                            };
                            set_safe(chunk, nx, base_y + dy, lz, block, origin_y);
                        }
                    }
                }
                (
                    Biome::Mesa
                    | Biome::Karst
                    | Biome::Mountains
                    | Biome::SnowyMountains
                    | Biome::Forest
                    | Biome::Jungle,
                    3 | 4 | 5,
                ) => {
                    // Plated landing pad with a lit rim and a corner mast.
                    for dx in -2..=2 {
                        for dz in -2..=2 {
                            let nx = lx as i32 + dx;
                            let nz = lz as i32 + dz;
                            if nx < 0 || nz < 0 {
                                continue;
                            }
                            let edge = dx.abs() == 2 || dz.abs() == 2;
                            let block = if edge && (dx + dz).rem_euclid(2) == 0 {
                                BlockType::NeonCyan
                            } else if edge {
                                BlockType::PlatingTeal
                            } else {
                                BlockType::RoadDeck
                            };
                            set_safe(chunk, nx as usize, base_y, nz as usize, block, origin_y);
                        }
                    }
                    for dy in 1..4 {
                        set_safe(
                            chunk,
                            lx,
                            base_y + dy,
                            lz,
                            BlockType::PlatingWhite,
                            origin_y,
                        );
                    }
                    set_safe(chunk, lx, base_y + 4, lz, BlockType::NeonAmber, origin_y);
                }
                (
                    Biome::Mesa
                    | Biome::Karst
                    | Biome::Mountains
                    | Biome::SnowyMountains
                    | Biome::Forest
                    | Biome::Jungle,
                    6 | 7,
                ) => {
                    // Plasma conduit: a short run of glowing pipe on
                    // plated saddles, tapping the energy rivers below.
                    for dx in 0..4 {
                        let nx = lx + dx;
                        if nx >= CHUNK_SIZE {
                            continue;
                        }
                        set_safe(chunk, nx, base_y, lz, BlockType::PlatingTeal, origin_y);
                        let block = if dx % 3 == 0 {
                            BlockType::PlatingWhite
                        } else {
                            BlockType::PlasmaFlow
                        };
                        set_safe(chunk, nx, base_y + 1, lz, block, origin_y);
                    }
                }

                // --- PLAINS / SAVANNA / DESERT -------------------------
                (Biome::Plains | Biome::Savanna | Biome::Tundra | Biome::Desert, 0 | 1) => {
                    // Cargo crate: 2x2x2 stone box (stackable shipping
                    // container). Classic sci-fi shooter prop.
                    for dx in 0..2 {
                        for dz in 0..2 {
                            for dy in 0..2 {
                                let nx = lx + dx;
                                let nz = lz + dz;
                                if nx >= CHUNK_SIZE || nz >= CHUNK_SIZE {
                                    continue;
                                }
                                let block = if dy == 1 && (dx + dz) % 2 == 0 {
                                    BlockType::LuminiteCrystal // glowing label strip
                                } else {
                                    BlockType::Stone
                                };
                                set_safe(chunk, nx, base_y + dy, nz, block, origin_y);
                            }
                        }
                    }
                }
                (Biome::Plains | Biome::Savanna | Biome::Tundra | Biome::Desert, 2 | 3) => {
                    // Holo-console: 1x2 stone block with a glowing
                    // crystal top â€” like a sci-fi signpost / terminal.
                    set_safe(chunk, lx, base_y, lz, BlockType::Stone, origin_y);
                    set_safe(
                        chunk,
                        lx,
                        base_y + 1,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }
                (Biome::Plains | Biome::Savanna | Biome::Tundra | Biome::Desert, 4) => {
                    // Landing-pad strip: 3x1 glow-sand tile with
                    // stone markers at each end.
                    set_safe(chunk, lx, base_y, lz, BlockType::Stone, origin_y);
                    if lx + 1 < CHUNK_SIZE {
                        set_safe(
                            chunk,
                            lx + 1,
                            base_y,
                            lz,
                            BlockType::LuminiteCrystal,
                            origin_y,
                        );
                    }
                    if lx + 2 < CHUNK_SIZE {
                        set_safe(chunk, lx + 2, base_y, lz, BlockType::Stone, origin_y);
                    }
                }
                (Biome::Plains | Biome::Savanna | Biome::Tundra | Biome::Desert, 5) => {
                    // Warning pylon: 4-tall stone with alternating
                    // crystal stripes â€” reads as a striped hazard post.
                    set_safe(chunk, lx, base_y, lz, BlockType::Stone, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::MagnetiteOre, origin_y);
                    set_safe(chunk, lx, base_y + 2, lz, BlockType::Stone, origin_y);
                    set_safe(chunk, lx, base_y + 3, lz, BlockType::MagnetiteOre, origin_y);
                }
                (Biome::Plains | Biome::Savanna | Biome::Tundra | Biome::Desert, _) => {
                    // Fuel barrel: single-block glow-sand on stone
                    // pedestal â€” the catch-all cheap prop.
                    set_safe(chunk, lx, base_y, lz, BlockType::Stone, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::Lava, origin_y);
                }

                // --- MESA / KARST / MOUNTAIN ruins --------------------
                (Biome::Mesa | Biome::Karst | Biome::Mountains | Biome::SnowyMountains, _) => {
                    // Weathered strata post with a glow crown.
                    set_safe(chunk, lx, base_y, lz, BlockType::VioletStone, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::AmberStone, origin_y);
                    set_safe(chunk, lx, base_y + 2, lz, BlockType::MagnetiteOre, origin_y);
                }

                // --- FOREST / JUNGLE (rare) ---------------------------
                (Biome::Forest | Biome::Jungle, _) => {
                    // Abandoned alien survey beacon so normal terrain
                    // still carries the neon sci-fi language.
                    set_safe(chunk, lx, base_y, lz, BlockType::Basalt, origin_y);
                    set_safe(
                        chunk,
                        lx,
                        base_y + 1,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                    if lx + 1 < CHUNK_SIZE {
                        set_safe(chunk, lx + 1, base_y, lz, BlockType::Crystal, origin_y);
                    }
                    if lz + 1 < CHUNK_SIZE {
                        set_safe(chunk, lx, base_y, lz + 1, BlockType::MagnetiteOre, origin_y);
                    }
                }

                _ => {}
            }
        }
    }

    /// Single-block crystal / neon specks on the surface — micro-detail
    /// that reads as glitter without extra mesh types.
    fn decorate_micro_specks(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        let origin_y = cy * CHUNK_SIZE_I;
        const N: usize = 36;
        for i in 0..N {
            let r_pos = column_rand(self.seed ^ (0x51EE_1110_u32 + i as u32 * 401), cx, cz);
            let r_mat = column_rand(self.seed ^ (0x51EE_2220_u32 + i as u32 * 403), cx, cz);
            let r_gate = column_rand(self.seed ^ (0x51EE_3330_u32 + i as u32 * 407), cx, cz);

            let lx = ((r_pos * 131_071.0) as usize) % CHUNK_SIZE;
            let lz = ((r_pos * 524_287.0) as usize) % CHUNK_SIZE;
            let wx = cx * CHUNK_SIZE_I + lx as i32;
            let wz = cz * CHUNK_SIZE_I + lz as i32;
            let (surface, _cont) = self.surface_height(wx as f64, wz as f64);
            let biome = self.biome(wx as f64, wz as f64, surface, _cont);

            // Sparse on purpose. At the old rate these read as glitter
            // only because the ground around them was almost black; over
            // a lit, saturated surface the same density looks like litter.
            let keep = match biome {
                Biome::CrystalSpires | Biome::AlienReef => r_gate < 0.075,
                Biome::GlacierShards => r_gate < 0.030,
                Biome::VolcanicWaste => r_gate < 0.022,
                Biome::Forest | Biome::Jungle | Biome::Karst => false,
                Biome::Mesa => false,
                Biome::Desert | Biome::Savanna | Biome::Beach | Biome::Ocean => false,
                Biome::Mountains | Biome::SnowyMountains | Biome::Tundra => false,
                _ => false,
            };
            if !keep {
                continue;
            }

            let base_y = surface + 1;
            if base_y < origin_y || base_y >= origin_y + CHUNK_SIZE_I {
                continue;
            }

            let roll = ((r_mat * 100.0) as u32) % 6;
            let bt = match biome {
                Biome::CrystalSpires => match roll {
                    0 | 1 => BlockType::Crystal,
                    2 => BlockType::LuminiteCrystal,
                    3 => BlockType::GlowSand,
                    4 => BlockType::Limestone,
                    _ => BlockType::MossStone,
                },
                Biome::AlienReef => match roll {
                    0 | 1 => BlockType::AlienMoss,
                    2 => BlockType::BoneRock,
                    3 => BlockType::Crystal,
                    4 => BlockType::MossStone,
                    _ => BlockType::Limestone,
                },
                Biome::GlacierShards => match roll {
                    0 | 1 => BlockType::Ice,
                    2 => BlockType::Crystal,
                    3 => BlockType::Snow,
                    _ => BlockType::Gravel,
                },
                Biome::VolcanicWaste => match roll {
                    0 | 1 => BlockType::Basalt,
                    2 => BlockType::Lava,
                    3 => BlockType::RedStone,
                    _ => BlockType::Gravel,
                },
                Biome::Mesa => match roll {
                    0 | 1 => BlockType::MesaClay,
                    2 => BlockType::RedSand,
                    3 => BlockType::RedStone,
                    _ => BlockType::Gravel,
                },
                Biome::Desert | Biome::Savanna => match roll {
                    0 | 1 => BlockType::Sand,
                    2 => BlockType::RedSand,
                    3 => BlockType::SavannaGrass,
                    _ => BlockType::Gravel,
                },
                Biome::Forest => match roll {
                    0 | 1 => BlockType::MossStone,
                    2 => BlockType::Leaves,
                    3 => BlockType::Wood,
                    _ => BlockType::Gravel,
                },
                Biome::Jungle => match roll {
                    0 | 1 => BlockType::JungleLeaves,
                    2 => BlockType::MossStone,
                    3 => BlockType::Wood,
                    _ => BlockType::Gravel,
                },
                Biome::Karst => match roll {
                    0 | 1 => BlockType::Limestone,
                    2 => BlockType::MossStone,
                    _ => BlockType::Gravel,
                },
                Biome::SnowyMountains | Biome::Tundra => match roll {
                    0 | 1 => BlockType::Snow,
                    2 => BlockType::Ice,
                    _ => BlockType::Gravel,
                },
                Biome::Beach | Biome::Ocean => match roll {
                    0 | 1 => BlockType::Sand,
                    _ => BlockType::Gravel,
                },
                _ => match roll {
                    0 | 1 => BlockType::MossStone,
                    2 => BlockType::Grass,
                    _ => BlockType::Gravel,
                },
            };
            set_safe(chunk, lx, base_y, lz, bt, origin_y);
        }
    }

    /// Scatter natural arches, ruin pillars and boulder piles in
    /// mountain/mesa/karst biomes. Purely chunk-local: anything that
    /// would poke past the chunk boundary is skipped.
    fn decorate_structures(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        let origin_y = cy * CHUNK_SIZE_I;

        // One roll per chunk decides which (if any) landmark spawns
        // here. Keeps density low (â‰ˆ one every few chunks).
        let roll = column_rand(self.seed ^ 0xA11CE, cx, cz);
        // Random but stable anchor inside the chunk â€” not always the
        // centre, so neighbouring chunks don't line up in a grid.
        let anchor_x = 4 + ((column_rand(self.seed ^ 0xB077, cx, cz) * 8.0) as i32);
        let anchor_z = 4 + ((column_rand(self.seed ^ 0xC099, cx, cz) * 8.0) as i32);
        let wx_anchor = cx * CHUNK_SIZE_I + anchor_x;
        let wz_anchor = cz * CHUNK_SIZE_I + anchor_z;
        let (surface, cont) = self.surface_height(wx_anchor as f64, wz_anchor as f64);
        let biome = self.biome(wx_anchor as f64, wz_anchor as f64, surface, cont);

        // Macro landmarks: very rare hero silhouettes that act as
        // long-range navigation anchors and create strong reveal moments.
        if roll < 0.010 && matches!(biome, Biome::CrystalSpires | Biome::AlienReef) {
            self.try_place_spire_cathedral(chunk, anchor_x, anchor_z, surface, origin_y, biome);
            return;
        }
        if roll >= 0.010 && roll < 0.016 && matches!(biome, Biome::Mesa | Biome::VolcanicWaste) {
            self.try_place_crater_basin(chunk, anchor_x, anchor_z, surface, origin_y, biome);
            return;
        }

        // Arch: 1 chance in ~40 chunks, only in rocky biomes.
        if roll < 0.025
            && matches!(
                biome,
                Biome::Mountains | Biome::SnowyMountains | Biome::Mesa | Biome::Karst
            )
        {
            self.try_place_arch(chunk, anchor_x, anchor_z, surface, origin_y, biome);
            return;
        }
        // Ruin pillar cluster: 1 in ~60, plains/mountains/mesa.
        if roll >= 0.025
            && roll < 0.042
            && matches!(
                biome,
                Biome::Plains | Biome::Savanna | Biome::Mountains | Biome::Mesa
            )
        {
            self.try_place_ruin_pillars(chunk, anchor_x, anchor_z, surface, origin_y, biome);
            return;
        }
        // Boulder pile: 1 in ~40 in rocky / alien biomes.
        if roll >= 0.042
            && roll < 0.067
            && matches!(
                biome,
                Biome::Mountains
                    | Biome::SnowyMountains
                    | Biome::Mesa
                    | Biome::Tundra
                    | Biome::CrystalSpires
                    | Biome::GlacierShards
            )
        {
            self.try_place_boulder_pile(chunk, anchor_x, anchor_z, surface, origin_y, biome);
        }
    }

    /// Natural stone arch â€” two pillars with a span of blocks joining
    /// them at the top. Walkable underneath. Scales with biome.
    fn try_place_arch(
        &self,
        chunk: &mut Chunk,
        ax: i32,
        az: i32,
        surface: i32,
        origin_y: i32,
        biome: Biome,
    ) {
        let height = 7 + ((column_rand(self.seed ^ 0xAAAA, ax, az) * 4.0) as i32);
        let span = 5 + ((column_rand(self.seed ^ 0xBBBB, ax, az) * 3.0) as i32);
        let top_y = surface + height;
        if top_y + 1 >= origin_y + CHUNK_SIZE_I {
            return;
        }
        if surface < origin_y - 1 {
            return;
        }
        let block = match biome {
            Biome::Mesa => BlockType::RedStone,
            Biome::Karst => BlockType::Limestone,
            Biome::SnowyMountains => BlockType::Stone,
            _ => BlockType::Stone,
        };
        let left_x = ax - span / 2;
        let right_x = ax + span / 2;
        if left_x < 0 || right_x >= CHUNK_SIZE_I {
            return;
        }
        let lz = az as usize;
        // Two pillars.
        for x in [left_x, right_x] {
            for y in (surface + 1)..=top_y {
                if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                    continue;
                }
                let ly = (y - origin_y) as usize;
                chunk.set(x as usize, ly, lz, block.into());
            }
        }
        // Arching span at top_y with a gentle curve (one row slightly
        // lower at the ends â†’ cleaner arch silhouette).
        for x in left_x..=right_x {
            let curve_off = if x == left_x || x == right_x { 0 } else { 0 };
            let y = top_y - curve_off;
            if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                continue;
            }
            let ly = (y - origin_y) as usize;
            chunk.set(x as usize, ly, lz, block.into());
        }
        // Crown stone on very top centre for visual "keystone".
        let keystone_y = top_y + 1;
        if keystone_y >= origin_y && keystone_y < origin_y + CHUNK_SIZE_I {
            let ly = (keystone_y - origin_y) as usize;
            chunk.set(ax as usize, ly, lz, block.into());
        }
    }

    /// Cluster of 4â€“7 broken pillars on the surface â€” looks like an
    /// ancient ruin. Heights vary so the silhouette feels natural.
    fn try_place_ruin_pillars(
        &self,
        chunk: &mut Chunk,
        ax: i32,
        az: i32,
        _surface: i32,
        origin_y: i32,
        biome: Biome,
    ) {
        let block = match biome {
            Biome::Mesa => BlockType::MesaClay,
            Biome::Savanna => BlockType::Limestone,
            _ => BlockType::Stone,
        };
        let cap = match biome {
            Biome::Mesa => BlockType::RedStone,
            _ => BlockType::MossStone,
        };
        let positions = [(-3, -2), (-2, 2), (0, 0), (2, -1), (3, 2), (-1, -3), (1, 3)];
        for (i, (dx, dz)) in positions.iter().enumerate() {
            let x = ax + dx;
            let z = az + dz;
            if x < 0 || x >= CHUNK_SIZE_I || z < 0 || z >= CHUNK_SIZE_I {
                continue;
            }
            // Varying pillar heights (3, 5, 2, 6, 4, 3, 5).
            let h = 2 + ((column_rand(self.seed ^ (i as u32 * 17), ax + dx, az + dz) * 5.0) as i32);
            let wx = chunk.pos.x * CHUNK_SIZE_I + x;
            let wz = chunk.pos.z * CHUNK_SIZE_I + z;
            let (col_surface, _) = self.surface_height(wx as f64, wz as f64);
            for dy in 1..=h {
                let y = col_surface + dy;
                if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                    continue;
                }
                let ly = (y - origin_y) as usize;
                let b = if dy == h { cap } else { block };
                chunk.set(x as usize, ly, z as usize, b.into());
            }
        }
    }

    /// Loose pile of boulders (5Ã—5 low dome of stone blocks).
    fn try_place_boulder_pile(
        &self,
        chunk: &mut Chunk,
        ax: i32,
        az: i32,
        surface: i32,
        origin_y: i32,
        biome: Biome,
    ) {
        let block = match biome {
            Biome::Mesa => BlockType::RedStone,
            Biome::GlacierShards => BlockType::Ice,
            Biome::SnowyMountains => BlockType::Stone,
            Biome::CrystalSpires => BlockType::Crystal,
            Biome::Tundra => BlockType::Gravel,
            _ => BlockType::Stone,
        };
        // 3 layer dome, shrinking radius.
        let layers = [(2i32, 0i32), (1, 1), (0, 2)];
        for (radius, dy) in layers.iter() {
            let y = surface + dy;
            if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                continue;
            }
            let ly = (y - origin_y) as usize;
            for dz in -*radius..=*radius {
                for dx in -*radius..=*radius {
                    if dx.abs() == *radius && dz.abs() == *radius && *radius == 2 {
                        continue;
                    }
                    let nx = ax + dx;
                    let nz = az + dz;
                    if nx < 0 || nx >= CHUNK_SIZE_I || nz < 0 || nz >= CHUNK_SIZE_I {
                        continue;
                    }
                    chunk.set(nx as usize, ly, nz as usize, block.into());
                }
            }
        }
    }

    fn try_place_spire_cathedral(
        &self,
        chunk: &mut Chunk,
        ax: i32,
        az: i32,
        surface: i32,
        origin_y: i32,
        biome: Biome,
    ) {
        let h = 16 + ((column_rand(self.seed ^ 0x51A1_9001, ax, az) * 9.0) as i32);
        let block = if biome == Biome::AlienReef {
            BlockType::LuminiteCrystal
        } else {
            BlockType::Crystal
        };
        let buttress = if biome == Biome::AlienReef {
            BlockType::ShipHullAlloy
        } else {
            BlockType::Limestone
        };
        for dy in 0..=h {
            let y = surface + dy;
            if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                continue;
            }
            let ly = (y - origin_y) as usize;
            let taper = (h - dy) / 5;
            for dz in -1 - taper..=1 + taper {
                for dx in -1 - taper..=1 + taper {
                    let nx = ax + dx;
                    let nz = az + dz;
                    if nx < 1 || nx >= CHUNK_SIZE_I - 1 || nz < 1 || nz >= CHUNK_SIZE_I - 1 {
                        continue;
                    }
                    let edge = dx.abs() == 1 + taper || dz.abs() == 1 + taper;
                    chunk.set(
                        nx as usize,
                        ly,
                        nz as usize,
                        if edge { buttress } else { block }.into(),
                    );
                }
            }
        }
    }

    fn try_place_crater_basin(
        &self,
        chunk: &mut Chunk,
        ax: i32,
        az: i32,
        surface: i32,
        origin_y: i32,
        biome: Biome,
    ) {
        let rim_block = if biome == Biome::VolcanicWaste {
            BlockType::Basalt
        } else {
            BlockType::RedStone
        };
        let core_block = if biome == Biome::VolcanicWaste {
            BlockType::Lava
        } else {
            BlockType::MagnetiteOre
        };
        for dz in -4..=4 {
            for dx in -4..=4 {
                let nx = ax + dx;
                let nz = az + dz;
                if nx < 0 || nx >= CHUNK_SIZE_I || nz < 0 || nz >= CHUNK_SIZE_I {
                    continue;
                }
                let d2 = dx * dx + dz * dz;
                let depth = if d2 <= 2 {
                    3
                } else if d2 <= 8 {
                    2
                } else if d2 <= 16 {
                    1
                } else {
                    0
                };
                for k in 0..=depth {
                    let y = surface - k;
                    if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                        continue;
                    }
                    let ly = (y - origin_y) as usize;
                    let b = if d2 <= 2 && k == depth {
                        core_block
                    } else {
                        rim_block
                    };
                    chunk.set(nx as usize, ly, nz as usize, b.into());
                }
            }
        }
    }

    /// Hill-sculpting palace pass. Rather than placing buildings on
    /// flattened land, we TURN THE HILL ITSELF into a futuristic
    /// palace: the natural peak becomes the building silhouette,
    /// the insides get hollowed out with multiple floors, the shell
    /// stays solid for manual cutouts, and cardinal entrances let the
    /// player walk inside.
    #[allow(dead_code)]
    pub fn try_sculpt_palace(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        // Palace grid: each 4Ã—4 chunks (64Ã—64 blocks) is one district.
        const DISTRICT: i32 = 4;
        let dx = cx.div_euclid(DISTRICT);
        let dz = cz.div_euclid(DISTRICT);
        let roll = column_rand(self.seed ^ 0xC17A_F00D, dx, dz);
        // ~55% of districts become a sculpted palace.
        if roll > 0.55 {
            return;
        }

        // Centre of the district drives biome + base altitude.
        let centre_wx = dx * DISTRICT * CHUNK_SIZE_I + (DISTRICT * CHUNK_SIZE_I) / 2;
        let centre_wz = dz * DISTRICT * CHUNK_SIZE_I + (DISTRICT * CHUNK_SIZE_I) / 2;
        let (centre_h, centre_cont) = self.surface_height(centre_wx as f64, centre_wz as f64);
        let centre_biome = self.biome(centre_wx as f64, centre_wz as f64, centre_h, centre_cont);
        // Skip water + dangerous terrain.
        if matches!(centre_biome, Biome::Ocean | Biome::GlacierShards) {
            return;
        }
        if centre_h <= WATER_LEVEL + 6 {
            return; // need a real hill to sculpt
        }

        let origin_y = cy * CHUNK_SIZE_I;
        // Ground floor slightly below the peak so the palace merges
        // with the hill instead of floating on top.
        let base_y = centre_h - 2;

        // Palette per biome. `wall` is the bulk of the shell,
        // `accent` is the roof cap, `glow` lights the interior floor.
        let (wall, accent, glow, floor) = match centre_biome {
            Biome::VolcanicWaste => (
                BlockType::Basalt,
                BlockType::Stone,
                BlockType::Lava,
                BlockType::GlowSand,
            ),
            Biome::CrystalSpires => (
                BlockType::BoneRock,
                BlockType::BoneRock,
                BlockType::Crystal,
                BlockType::GlowSand,
            ),
            Biome::AlienReef => (
                BlockType::BoneRock,
                BlockType::BoneRock,
                BlockType::AlienMoss,
                BlockType::GlowSand,
            ),
            Biome::Desert | Biome::Savanna | Biome::Mesa => (
                BlockType::BoneRock,
                BlockType::Stone,
                BlockType::Crystal,
                BlockType::GlowSand,
            ),
            Biome::SnowyMountains | Biome::Tundra => (
                BlockType::Ice,
                BlockType::Limestone,
                BlockType::Crystal,
                BlockType::GlowSand,
            ),
            _ => (
                BlockType::Limestone,
                BlockType::Stone,
                BlockType::Crystal,
                BlockType::GlowSand,
            ),
        };

        let district_side = DISTRICT * CHUNK_SIZE_I;
        let district_ox = dx * district_side;
        let district_oz = dz * district_side;
        let half_side = district_side / 2;
        // Footprint radius â€” leave a ring of natural landscape around
        // the palace for approach/landscaping.
        let max_r = half_side - 6;

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = cx * CHUNK_SIZE_I + lx as i32;
                let wz = cz * CHUNK_SIZE_I + lz as i32;
                let rx = wx - (district_ox + half_side);
                let rz = wz - (district_oz + half_side);
                let cheb = rx.abs().max(rz.abs());
                if cheb > max_r {
                    continue; // outside palace footprint â€” keep natural terrain
                }

                let (h_here, _) = self.surface_height(wx as f64, wz as f64);
                let palace_top = h_here;
                if palace_top - base_y < 8 {
                    // column too short â€” leave natural terrain untouched
                    continue;
                }

                // Shell detection: edge of footprint OR neighbour with
                // a significantly lower natural surface. The shell
                // follows the hill's silhouette.
                let edge_of_footprint = cheb >= max_r - 1;
                let n = [
                    self.surface_height((wx + 1) as f64, wz as f64).0,
                    self.surface_height((wx - 1) as f64, wz as f64).0,
                    self.surface_height(wx as f64, (wz + 1) as f64).0,
                    self.surface_height(wx as f64, (wz - 1) as f64).0,
                ];
                let height_drop = n.iter().any(|&nh| nh + 3 < palace_top);
                let is_shell = edge_of_footprint || height_drop;

                // Rebuild this column from base_y up to palace_top.
                for wy in base_y..=palace_top {
                    if wy < origin_y || wy >= origin_y + CHUNK_SIZE_I {
                        continue;
                    }
                    let ly = (wy - origin_y) as usize;
                    let dy_bottom = wy - base_y;
                    let dy_top = palace_top - wy;

                    // Ground floor is always a warm emissive slab so
                    // the interior is lit from below.
                    if dy_bottom == 0 {
                        chunk.set(lx, ly, lz, floor.into());
                        continue;
                    }

                    if is_shell {
                        // Entrance arches on cardinal axes at the
                        // palace edge, height 1..=3.
                        let on_axis = rx == 0 || rz == 0;
                        let at_edge = cheb == max_r;
                        if on_axis && at_edge && dy_bottom >= 1 && dy_bottom <= 3 {
                            chunk.set(lx, ly, lz, AIR);
                            continue;
                        }
                        if dy_top == 0 {
                            chunk.set(lx, ly, lz, accent.into());
                        } else {
                            chunk.set(lx, ly, lz, wall.into());
                        }
                    } else {
                        // Interior column: hollow with a structural
                        // floor every 6 y (walkable multi-storey).
                        let floor_band = dy_bottom % 6 == 0;
                        if dy_top == 0 {
                            chunk.set(lx, ly, lz, wall.into()); // roof cap
                        } else if floor_band && dy_top >= 2 {
                            chunk.set(lx, ly, lz, wall.into());
                        } else {
                            chunk.set(lx, ly, lz, AIR);
                        }
                    }
                }

                // Pillar tips: at the very top of the hill (within
                // 2 of centre in cheb), extend a thin spire 4 y above
                // palace_top for a gothic-futuristic roofline.
                if cheb <= 1 {
                    for extra in 1..=4 {
                        let wy = palace_top + extra;
                        if wy < origin_y || wy >= origin_y + CHUNK_SIZE_I {
                            continue;
                        }
                        let ly = (wy - origin_y) as usize;
                        let block = if extra == 4 { glow } else { accent };
                        chunk.set(lx, ly, lz, block.into());
                    }
                }
            }
        }
    }

    /// Player and bot commands now own city silhouettes. The automatic
    /// terrain-palace pass made normal worlds look randomly hollowed and
    /// artificial, so default terrain generation leaves cities alone.
    #[inline]
    pub fn try_place_city(&self, _chunk: &mut Chunk) {}

    pub fn biome_at(&self, wx: i32, wz: i32) -> Biome {
        let (h, cont) = self.surface_height(wx as f64, wz as f64);
        self.biome(wx as f64, wz as f64, h, cont)
    }

    pub fn find_neon_showcase_spawn(
        &self,
        origin_x: i32,
        origin_z: i32,
        max_radius: i32,
    ) -> Option<NeonSpawnPoint> {
        let mut best: Option<(i32, NeonSpawnPoint)> = None;
        let step = 64;
        let max_radius = max_radius.max(step);

        for radius in (0..=max_radius).step_by(step as usize) {
            let samples = if radius == 0 {
                vec![(origin_x, origin_z)]
            } else {
                let mut out = Vec::with_capacity(((radius / step) * 8).max(8) as usize);
                let min_x = origin_x - radius;
                let max_x = origin_x + radius;
                let min_z = origin_z - radius;
                let max_z = origin_z + radius;
                for x in (min_x..=max_x).step_by(step as usize) {
                    out.push((x, min_z));
                    out.push((x, max_z));
                }
                for z in ((min_z + step)..=(max_z - step)).step_by(step as usize) {
                    out.push((min_x, z));
                    out.push((max_x, z));
                }
                out
            };

            for (x, z) in samples {
                let surface = self.surface_height_at(x, z);
                let biome = self.biome_at(x, z);
                if !biome.is_neon_showcase() || surface <= WATER_LEVEL + 6 {
                    continue;
                }

                let hn = self.surface_height_at(x, z - 2);
                let hs = self.surface_height_at(x, z + 2);
                let he = self.surface_height_at(x + 2, z);
                let hw = self.surface_height_at(x - 2, z);
                let slope = (surface - hn)
                    .abs()
                    .max((surface - hs).abs())
                    .max((surface - he).abs())
                    .max((surface - hw).abs());
                if slope > 5 {
                    continue;
                }

                let distance = (x - origin_x).abs().max((z - origin_z).abs());
                let floor_score = (surface - (WATER_LEVEL + 22)).abs();
                let biome_bonus = if biome == Biome::AlienReef {
                    -280
                } else {
                    -180
                };
                let score = distance + slope * 320 + floor_score * 6 + biome_bonus;
                let candidate = NeonSpawnPoint {
                    x,
                    y: surface + 26,
                    z,
                    biome,
                };
                if best.map_or(true, |(best_score, _)| score < best_score) {
                    best = Some((score, candidate));
                }
            }

            if radius >= 512 && best.is_some() {
                break;
            }
        }

        best.map(|(_, point)| point)
    }

    pub fn find_natural_spawn(
        &self,
        origin_x: i32,
        origin_z: i32,
        max_radius: i32,
    ) -> Option<NaturalSpawnPoint> {
        let mut best: Option<(i32, NaturalSpawnPoint)> = None;
        let step = 32;
        let max_radius = max_radius.max(step);

        for radius in (0..=max_radius).step_by(step as usize) {
            let samples = if radius == 0 {
                vec![(origin_x, origin_z)]
            } else {
                let mut out = Vec::with_capacity(((radius / step) * 8).max(8) as usize);
                let min_x = origin_x - radius;
                let max_x = origin_x + radius;
                let min_z = origin_z - radius;
                let max_z = origin_z + radius;
                for x in (min_x..=max_x).step_by(step as usize) {
                    out.push((x, min_z));
                    out.push((x, max_z));
                }
                for z in ((min_z + step)..=(max_z - step)).step_by(step as usize) {
                    out.push((min_x, z));
                    out.push((max_x, z));
                }
                out
            };

            for (x, z) in samples {
                let surface = self.surface_height_at(x, z);
                let biome = self.biome_at(x, z);
                if surface <= WATER_LEVEL + 4 {
                    continue;
                }
                // The frontier's showcase biomes ARE the world now, so
                // the only things that disqualify a spawn are the ones
                // that would actually hurt: standing in a lava or plasma
                // channel, or on a wall too steep to walk off.
                if self.frontier.rivers.column(x, z).is_some() {
                    continue;
                }

                let hn = self.surface_height_at(x, z - 2);
                let hs = self.surface_height_at(x, z + 2);
                let he = self.surface_height_at(x + 2, z);
                let hw = self.surface_height_at(x - 2, z);
                let slope = (surface - hn)
                    .abs()
                    .max((surface - hs).abs())
                    .max((surface - he).abs())
                    .max((surface - hw).abs());
                if slope > 6 {
                    continue;
                }

                let distance = (x - origin_x).abs().max((z - origin_z).abs());
                let comfortable_height = (surface - (WATER_LEVEL + 18)).abs();
                // Landing on a mesa table or a reef shelf gives the
                // player the postcard on their first frame instead of a
                // featureless field, so nudge the search toward them.
                // Extra pull toward the hero vista (crystal + river +
                // skyway parked at ~(-Z, +X) of origin).
                let hero_dx = (x - 48).abs();
                let hero_dz = (z - (-28)).abs();
                let near_postcard = hero_dx < 90 && hero_dz < 90;
                let vista_bonus = if near_postcard {
                    -480
                } else if biome.is_showcase_terrain() || biome == Biome::Mesa {
                    -220
                } else {
                    0
                };
                let score = distance + slope * 96 + comfortable_height * 2 + vista_bonus;
                let candidate = NaturalSpawnPoint {
                    x,
                    y: surface + 10,
                    z,
                    biome,
                };
                if best.map_or(true, |(best_score, _)| score < best_score) {
                    best = Some((score, candidate));
                }
            }

            if radius >= 256 && best.is_some() {
                break;
            }
        }

        best.map(|(_, point)| point)
    }

    /// Public surface height lookup â€” block y of the topmost solid block
    /// at a world (x, z) column. Used to spawn the player above terrain.
    pub fn surface_height_at(&self, wx: i32, wz: i32) -> i32 {
        self.surface_height(wx as f64, wz as f64).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk a large sample grid and report which of the frontier's
    /// provinces and biomes actually turn up in a default world.
    fn survey(
        seed: u32,
    ) -> (
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<String>,
    ) {
        let generator = TerrainGenerator::new(seed);
        let mut regions = std::collections::BTreeSet::new();
        let mut biomes = std::collections::BTreeSet::new();
        for z in (-12_000..=12_000).step_by(512) {
            for x in (-12_000..=12_000).step_by(512) {
                let (region, strength) = generator.region(x as f64, z as f64);
                if strength > 0.0 {
                    regions.insert(format!("{region:?}"));
                }
                biomes.insert(format!("{:?}", generator.biome_at(x, z)));
            }
        }
        (regions, biomes)
    }

    #[test]
    fn every_world_is_the_neon_frontier_not_an_earth_like_map() {
        // The engine used to lock the alien provinces out of ordinary
        // worlds and keep them for hand-built showcases. The frontier is
        // now the planet, so a default seed must contain the whole set.
        let (regions, biomes) = survey(12345);

        for expected in [
            "Canyon",
            "CrystalSpires",
            "VolcanicWaste",
            "GlacierShards",
            "AlienReef",
        ] {
            assert!(
                regions.contains(expected),
                "default world never generated the {expected} province; got {regions:?}"
            );
        }
        for expected in ["Mesa", "CrystalSpires", "VolcanicWaste", "AlienReef"] {
            assert!(
                biomes.contains(expected),
                "default world never generated the {expected} biome; got {biomes:?}"
            );
        }
    }

    #[test]
    fn the_frontier_shows_up_on_every_seed_not_just_the_default_one() {
        for seed in [1, 7, 12345, 90210, 4_000_000_007] {
            let (_, biomes) = survey(seed);
            let exotic = ["Mesa", "CrystalSpires", "VolcanicWaste", "AlienReef"]
                .iter()
                .filter(|b| biomes.contains(**b))
                .count();
            assert!(
                exotic >= 3,
                "seed {seed} only produced {exotic} of the frontier's signature biomes: {biomes:?}"
            );
        }
    }

    #[test]
    fn spawn_stays_on_walkable_ground_out_of_the_energy_channels() {
        let generator = TerrainGenerator::new(12345);
        let spawn = generator
            .find_natural_spawn(0, 0, 4096)
            .expect("every world needs a nearby safe terrain entry");

        assert!(spawn.y > WATER_LEVEL + 4);
        // Never drop the player into a lava or plasma channel.
        assert!(generator.frontier.rivers.column(spawn.x, spawn.z).is_none());
        // Spawn postcard is forced mesa country so the opening shot is
        // banded canyon, not a grassy field the seed happened to put
        // under the crystal cluster.
        assert_eq!(
            generator.biome_at(crate::frontier::HERO_CRYSTAL_X, crate::frontier::HERO_CRYSTAL_Z),
            Biome::Mesa
        );
        // And never onto a wall they would immediately slide off.
        let surface = generator.surface_height_at(spawn.x, spawn.z);
        for (dx, dz) in [(-2, 0), (2, 0), (0, -2), (0, 2)] {
            let neighbour = generator.surface_height_at(spawn.x + dx, spawn.z + dz);
            assert!((surface - neighbour).abs() <= 6);
        }
    }

    #[test]
    fn generated_chunks_carry_the_frontier_palette() {
        let generator = TerrainGenerator::new(12345);
        let mut seen = std::collections::BTreeSet::new();
        // A wide net: the signature materials are spread across
        // provinces, so no single column shows all of them.
        for cz in -14..14 {
            for cx in -14..14 {
                for cy in 2..11 {
                    let mut chunk = Chunk::new(ChunkPos::new(cx, cy, cz));
                    generator.generate(&mut chunk);
                    for ly in 0..CHUNK_SIZE {
                        for lz in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                seen.insert(chunk.get(lx, ly, lz));
                            }
                        }
                    }
                }
            }
        }

        for block in [
            BlockType::VioletStone,
            BlockType::AmberStone,
            BlockType::Crystal,
            BlockType::LuminiteCrystal,
        ] {
            let voxel: Voxel = block.into();
            assert!(
                seen.contains(&voxel),
                "generated terrain never produced {block:?}"
            );
        }
    }

    #[test]
    fn look_cone_cliff_carves_windows_into_generated_mesa() {
        let generator = TerrainGenerator::new(12345);
        let rim = generator.surface_height_at(132, -90);
        assert!(rim > 40, "look-cone mesa is missing ({rim})");
        let holo: Voxel = BlockType::HoloPanel.into();
        let deck: Voxel = BlockType::RoadDeck.into();
        let mut windows = 0usize;
        let mut floors = 0usize;
        for cy in 2..8 {
            for cx in 8..11 {
                for cz in -8..-4 {
                    let mut chunk = Chunk::new(ChunkPos::new(cx, cy, cz));
                    generator.generate(&mut chunk);
                    for ly in 0..CHUNK_SIZE {
                        for lz in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                let v = chunk.get(lx, ly, lz);
                                if v == holo {
                                    windows += 1;
                                }
                                if v == deck {
                                    floors += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            windows > 12,
            "carved cliff face has no lit windows in generated chunks (windows={windows} floors={floors} rim={rim})"
        );
        assert!(floors > 20, "carved cliff face has no terrace floors ({floors})");
    }

    #[test]
    fn peaks_stay_under_the_streamed_ceiling_so_nothing_is_decapitated() {
        // The default budget streams 10 chunk layers = 160 blocks. A
        // spire that pokes through that is not tall, it is sheared flat.
        const DEFAULT_CEILING: i32 = 10 * CHUNK_SIZE_I;
        let generator = TerrainGenerator::new(12345);
        let mut highest = i32::MIN;

        for z in (-12_000..=12_000).step_by(384) {
            for x in (-12_000..=12_000).step_by(384) {
                let surface = generator.surface_height_at(x, z);
                highest = highest.max(surface);
            }
        }

        assert!(
            highest < DEFAULT_CEILING,
            "default terrain should stay playable for normal streaming budgets; highest sample was {highest}"
        );
    }
}

// Derive Copy/Clone only for lookup (biome blocks helper is `&self`-free).
impl Clone for TerrainGenerator {
    fn clone(&self) -> Self {
        Self::new(self.seed)
    }
}

/// Cheap deterministic hash â†’ float in [0,1) keyed by (seed, x, z).
/// Used by the decoration pass so tree placement is stable per-seed.
#[inline]
fn column_rand(seed: u32, x: i32, z: i32) -> f64 {
    let mut h = seed as u64;
    h ^= (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h = h.rotate_left(27).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= (z as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    h = h.rotate_left(31).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    ((h >> 11) as f64) * (1.0 / (1u64 << 53) as f64)
}

/// Safe block-set for the sci-fi prop pass. Writes `block` into the
/// chunk at local (lx, wy-origin_y, lz) iff the target slot is
/// currently AIR and within chunk bounds. Used by `decorate_props` so
/// a prop never overwrites existing terrain or pushes out-of-bounds.
#[inline]
fn set_safe(chunk: &mut Chunk, lx: usize, wy: i32, lz: usize, block: BlockType, origin_y: i32) {
    if lx >= CHUNK_SIZE || lz >= CHUNK_SIZE {
        return;
    }
    let ly = wy - origin_y;
    if ly < 0 || ly >= CHUNK_SIZE_I {
        return;
    }
    let ly_u = ly as usize;
    if chunk.get(lx, ly_u, lz) == AIR {
        chunk.set(lx, ly_u, lz, block.into());
    }
}
