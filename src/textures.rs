//! Procedural texture generation + in-engine texture library.
//!
//! The game runs **without shipping any PNG files** by default: at startup
//! we bake a high-detail seamless universal-grain texture and a set of
//! per-block photorealistic *swatches* that the Texture Viewer can preview
//! and export to disk.
//!
//! Override pipeline:
//!   * If `./textures/universal_grain.png` exists at startup, we load it
//!     INSTEAD of baking the procedural one. Drop in a real photo-sourced
//!     PBR-style tile here to make every surface in the world feel
//!     photographic.
//!   * Per-block swatches are baked on demand for the Texture Viewer.
//!
//! The procedural pipeline is a blend of:
//!   * 6-octave domain-warped Perlin FBM (large shapes, pores, pockmarks)
//!   * Pseudo-Worley ridge noise          (crack / grout network, aggregate)
//!   * Anisotropic stretched Perlin       (wood grain, wind-drift, strata)
//!   * High-frequency speckle + sparkle   (pixel-level realism)

use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::texture::{
    Image, ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor,
};
use noise::{NoiseFn, Perlin};

use crate::blocks::{BlockType, MaterialId, CUSTOM_MATERIAL_BASE};

/// Folder (relative to cwd) from which the engine loads user-supplied
/// texture overrides and into which the Texture Viewer exports PNGs.
pub const TEX_DIR: &str = "textures";
pub const MATERIAL_DIR: &str = "textures/materials";

/// Built-in swatches are kept modest enough for low-end GPUs, but large
/// enough that the repeated terrain texture survives mip/downsample blending.
pub const BUILTIN_SWATCH_SIZE: u32 = 128;

#[derive(Resource, Default)]
pub struct MaterialLibrary {
    pub handles: std::collections::BTreeMap<MaterialId, Handle<StandardMaterial>>,
    pub names: std::collections::BTreeMap<MaterialId, String>,
    pub custom_ids: Vec<MaterialId>,
    pub reload_requested: bool,
    pub status: String,
}

pub(crate) fn terrain_alpha_mode_for_block(block: BlockType) -> AlphaMode {
    let alpha = block.color().to_srgba().alpha;
    if alpha < 0.99 {
        // Chunk terrain can fill the whole screen. Alpha-to-coverage keeps
        // glass/ice/crystal out of Bevy's sorted Blend pass.
        AlphaMode::AlphaToCoverage
    } else {
        AlphaMode::Opaque
    }
}

impl MaterialLibrary {
    pub fn rebuild(
        &mut self,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
        swatch_size: u32,
    ) {
        self.handles.clear();
        self.names.clear();
        self.custom_ids.clear();

        let size = swatch_size.clamp(32, 256);
        for swatch in bake_all_block_swatches(size) {
            let image = images.add(make_repeating_image(
                swatch.width,
                swatch.height,
                swatch.rgba.clone(),
            ));
            let alpha = swatch.block.color().to_srgba().alpha;
            let emissive = emissive_for_block(swatch.block);
            let handle = materials.add(StandardMaterial {
                base_color: Color::WHITE.with_alpha(alpha),
                base_color_texture: Some(image),
                emissive,
                perceptual_roughness: roughness_for_block(swatch.block),
                reflectance: reflectance_for_block(swatch.block),
                alpha_mode: terrain_alpha_mode_for_block(swatch.block),
                ..default()
            });
            let id = swatch.block as MaterialId;
            self.handles.insert(id, handle);
            self.names.insert(id, swatch.name.to_string());
        }

        let mut custom_loaded = 0usize;
        let _ = std::fs::create_dir_all(MATERIAL_DIR);
        if let Ok(read) = std::fs::read_dir(MATERIAL_DIR) {
            let mut paths: Vec<_> = read
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map_or(false, |e| e.eq_ignore_ascii_case("png"))
                })
                .collect();
            paths.sort();
            for (i, path) in paths.into_iter().enumerate() {
                let id = CUSTOM_MATERIAL_BASE.saturating_add(i as MaterialId);
                let Some(path_str) = path.to_str() else {
                    continue;
                };
                let Some(image) = load_png_as_repeating_image(path_str) else {
                    continue;
                };
                let image = images.add(image);
                let handle = materials.add(StandardMaterial {
                    base_color: Color::WHITE,
                    base_color_texture: Some(image),
                    perceptual_roughness: 1.0,
                    reflectance: 0.04,
                    ..default()
                });
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("custom")
                    .to_string();
                self.handles.insert(id, handle);
                self.names.insert(id, name);
                self.custom_ids.push(id);
                custom_loaded += 1;
            }
        }

        self.reload_requested = false;
        self.status = format!(
            "Materialien geladen: {} built-in, {} custom aus ./{}",
            self.handles.len().saturating_sub(custom_loaded),
            custom_loaded,
            MATERIAL_DIR
        );
    }

    pub fn handle_for(&self, id: MaterialId) -> Option<Handle<StandardMaterial>> {
        self.handles
            .get(&id)
            .cloned()
            .or_else(|| self.handles.get(&(BlockType::Stone as MaterialId)).cloned())
    }

    #[allow(dead_code)]
    pub fn name_for(&self, id: MaterialId) -> String {
        self.names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("material_{id}"))
    }
}

// ---------------------------------------------------------------------------
// Universal grain -----------------------------------------------------------
// ---------------------------------------------------------------------------

/// Load `./textures/universal_grain.png` if present, otherwise bake the
/// procedural universal grain at `size × size`.
pub fn universal_grain_or_override(size: u32) -> Image {
    let path = format!("{TEX_DIR}/universal_grain.png");
    if let Some(img) = load_png_as_repeating_image(&path) {
        info!("textures: using override {path}");
        return img;
    }
    universal_grain(size)
}

/// Build the universal surface-grain image. Seamlessly tileable because
/// every noise source is sampled on a torus (sin/cos of uv).
pub fn universal_grain(size: u32) -> Image {
    let perlin_a = Perlin::new(1);
    let micro = Perlin::new(2);
    let warp_x = Perlin::new(3);
    let warp_y = Perlin::new(4);
    let strata = Perlin::new(5);
    let cell = Perlin::new(6);
    let cell2 = Perlin::new(7);
    let sparkle = Perlin::new(8);

    let mut data = Vec::with_capacity((size * size * 4) as usize);

    let s = size as f64;
    let two_pi = std::f64::consts::TAU;

    for y in 0..size {
        for x in 0..size {
            let u = x as f64 / s;
            let v = y as f64 / s;
            let tx = (u * two_pi).cos();
            let tz = (u * two_pi).sin();
            let ty = (v * two_pi).cos();
            let tw = (v * two_pi).sin();

            let wx = warp_x.get([tx * 1.7, ty * 1.7, tz * 1.7, tw * 1.7]) * 0.22;
            let wy = warp_y.get([tx * 1.7 + 5.1, ty * 1.7 - 3.4, tz * 1.7, tw * 1.7]) * 0.22;

            let mut n = 0.0;
            let mut amp = 1.0;
            let mut freq = 1.0;
            let mut norm = 0.0;
            for _ in 0..6 {
                n += amp
                    * perlin_a.get([
                        (tx + wx) * freq,
                        (ty + wy) * freq,
                        (tz + wx) * freq,
                        (tw + wy) * freq,
                    ]);
                norm += amp;
                amp *= 0.52;
                freq *= 2.0;
            }
            n /= norm.max(1e-6);

            let m = micro.get([tx * 9.0, ty * 9.0, tz * 9.0, tw * 9.0]) * 0.6
                + micro.get([tx * 21.0, ty * 21.0, tz * 21.0, tw * 21.0]) * 0.18;

            // Pseudo-Worley: two low-freq perlins, take min absolute value,
            // sharpen → bright ridge network that reads as cracks / mortar.
            let c1 = cell.get([tx * 3.2, ty * 3.2, tz * 3.2, tw * 3.2]).abs();
            let c2 = cell2
                .get([tx * 3.5 + 1.3, ty * 3.5, tz * 3.5, tw * 3.5])
                .abs();
            let ridge = (1.0 - c1.min(c2)).powf(8.0);
            let cracks = -ridge * 0.18;

            let st_raw = strata.get([tx * 0.6, ty * 4.5, tz * 0.6, tw * 4.5]);
            let st = (st_raw.abs() - 0.5) * 0.14;

            let sp_raw = sparkle.get([tx * 34.0, ty * 34.0, tz * 34.0, tw * 34.0]);
            let sp = if sp_raw > 0.74 {
                (sp_raw - 0.74) * 0.9
            } else {
                0.0
            };

            let brightness = 0.93 + n * 0.16 + m * 0.05 + st + cracks + sp;
            let brightness = brightness.clamp(0.68, 1.18);

            let b = (brightness.clamp(0.0, 1.0) * 255.0).round() as u8;
            data.push(b);
            data.push(b);
            data.push(b);
            data.push(255);
        }
    }

    make_repeating_image(size, size, data)
}

// ---------------------------------------------------------------------------
// Per-block swatches (for the Texture Viewer) -------------------------------
// ---------------------------------------------------------------------------

/// Small RGBA8 preview of one BlockType's material.
#[derive(Clone, Debug)]
pub struct BlockSwatch {
    pub block: BlockType,
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    /// Tightly-packed RGBA8 sRGB, row-major top-down.
    pub rgba: Vec<u8>,
}

impl BlockSwatch {
    pub fn to_png_bytes(&self) -> Option<Vec<u8>> {
        use image::ImageEncoder;
        let mut out: Vec<u8> = Vec::with_capacity(self.rgba.len() + 4096);
        {
            let mut cursor = std::io::Cursor::new(&mut out);
            image::codecs::png::PngEncoder::new(&mut cursor)
                .write_image(
                    &self.rgba,
                    self.width,
                    self.height,
                    image::ExtendedColorType::Rgba8,
                )
                .ok()?;
        }
        Some(out)
    }

    pub fn save_png(&self, dir: &str) -> std::io::Result<std::path::PathBuf> {
        let _ = std::fs::create_dir_all(dir);
        let path = std::path::Path::new(dir).join(format!("{}.png", self.name));
        let bytes = self
            .to_png_bytes()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "png encode failed"))?;
        std::fs::write(&path, bytes)?;
        Ok(path)
    }
}

/// Bake photorealistic preview swatches for every registered block type.
pub fn bake_all_block_swatches(size: u32) -> Vec<BlockSwatch> {
    use BlockType::*;
    let list: &[(BlockType, &str, BlockStyle)] = &[
        (Stone, "stone", BlockStyle::Rock),
        (Dirt, "dirt", BlockStyle::Soil),
        (Grass, "grass", BlockStyle::Grass),
        (Sand, "sand", BlockStyle::Sand),
        (Water, "water", BlockStyle::Water),
        (Wood, "wood", BlockStyle::Wood),
        (Leaves, "leaves", BlockStyle::Leaves),
        (Snow, "snow", BlockStyle::Snow),
        (Ice, "ice", BlockStyle::Ice),
        (TundraGrass, "tundra_grass", BlockStyle::Grass),
        (JungleLeaves, "jungle_leaves", BlockStyle::Leaves),
        (SavannaGrass, "savanna_grass", BlockStyle::Grass),
        (Gravel, "gravel", BlockStyle::Rock),
        (Bedrock, "bedrock", BlockStyle::Rock),
        (RedSand, "red_sand", BlockStyle::Sand),
        (RedStone, "red_stone", BlockStyle::Strata),
        (MesaClay, "mesa_clay", BlockStyle::Strata),
        (MossStone, "moss_stone", BlockStyle::Rock),
        (Limestone, "limestone", BlockStyle::Rock),
        (Crystal, "crystal", BlockStyle::Crystal),
        (Basalt, "basalt", BlockStyle::Strata),
        (Lava, "lava", BlockStyle::Lava),
        (AlienMoss, "alien_moss", BlockStyle::Grass),
        (BoneRock, "bone_rock", BlockStyle::Rock),
        (GlowSand, "glow_sand", BlockStyle::Sand),
        (ShipHullDark, "ship_hull_dark", BlockStyle::Metal),
        (ShipHullAlloy, "ship_hull_alloy", BlockStyle::Metal),
        (CockpitGlass, "cockpit_glass", BlockStyle::Ice),
        (NeonCyan, "neon_cyan", BlockStyle::Energy),
        (NeonMagenta, "neon_magenta", BlockStyle::Energy),
        (NeonAmber, "neon_amber", BlockStyle::Lava),
        (EngineCore, "engine_core", BlockStyle::Lava),
        (LuminiteCrystal, "luminite_crystal", BlockStyle::Crystal),
        (MagnetiteOre, "magnetite_ore", BlockStyle::Rock),
        (IridiumVein, "iridium_vein", BlockStyle::Crystal),
        (VioletStone, "violet_stone", BlockStyle::Strata),
        (AmberStone, "amber_stone", BlockStyle::Strata),
        (PlasmaFlow, "plasma_flow", BlockStyle::Energy),
        (CrystalMagenta, "crystal_magenta", BlockStyle::Crystal),
        (CrystalGreen, "crystal_green", BlockStyle::Crystal),
        (HoloPanel, "holo_panel", BlockStyle::Crystal),
        (PlatingWhite, "plating_white", BlockStyle::Metal),
        (PlatingTeal, "plating_teal", BlockStyle::Metal),
        (RoadDeck, "road_deck", BlockStyle::Metal),
        (RoadMarking, "road_marking", BlockStyle::Metal),
    ];
    list.iter()
        .map(|(b, n, style)| bake_block_swatch(*b, n, *style, size))
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum BlockStyle {
    Rock,
    Strata,
    Soil,
    Grass,
    Sand,
    Water,
    Wood,
    Leaves,
    Snow,
    Ice,
    Lava,
    Metal,
    Crystal,
    Energy,
}

fn bake_block_swatch(
    block: BlockType,
    name: &'static str,
    style: BlockStyle,
    size: u32,
) -> BlockSwatch {
    let color = block.color();
    let rgba = color.to_srgba();
    let base = [rgba.red, rgba.green, rgba.blue];
    let alpha = (rgba.alpha.clamp(0.0, 1.0) * 255.0).round() as u8;

    let seed_base = block as u32;
    let perlin = Perlin::new(seed_base + 101);
    let detail = Perlin::new(seed_base + 202);
    let warp = Perlin::new(seed_base + 303);
    let strata = Perlin::new(seed_base + 404);
    let cell = Perlin::new(seed_base + 505);
    let macro_shape = Perlin::new(seed_base + 606);
    let grain = Perlin::new(seed_base + 707);
    let vein_noise = Perlin::new(seed_base + 808);

    let s = size as f64;
    let two_pi = std::f64::consts::TAU;
    let mut data = Vec::with_capacity((size * size * 4) as usize);

    for y in 0..size {
        for x in 0..size {
            let u = x as f64 / s;
            let v = y as f64 / s;
            let tx = (u * two_pi).cos();
            let tz = (u * two_pi).sin();
            let ty = (v * two_pi).cos();
            let tw = (v * two_pi).sin();

            let wx = warp.get([tx * 1.8, ty * 1.8, tz * 1.8, tw * 1.8]) * 0.2;
            let wy = warp.get([tx * 1.8 + 4.0, ty * 1.8 - 2.5, tz, tw]) * 0.2;

            let mut fbm = 0.0;
            let mut amp = 1.0;
            let mut freq = 1.0;
            let mut norm = 0.0;
            for _ in 0..5 {
                fbm += amp
                    * perlin.get([
                        (tx + wx) * freq,
                        (ty + wy) * freq,
                        (tz + wx) * freq,
                        (tw + wy) * freq,
                    ]);
                norm += amp;
                amp *= 0.55;
                freq *= 2.0;
            }
            fbm /= norm.max(1e-6);

            let micro = detail.get([tx * 14.0, ty * 14.0, tz * 14.0, tw * 14.0]) * 0.35;
            let strat = strata.get([tx * 0.7, ty * 5.0, tz * 0.7, tw * 5.0]);
            let cell_n = (1.0 - cell.get([tx * 4.0, ty * 4.0, tz * 4.0, tw * 4.0]).abs()).powf(7.0);
            let macro_n = macro_shape.get([tx * 0.55, ty * 0.55, tz * 0.55, tw * 0.55]);
            let broad = macro_shape.get([tx * 1.15 + 9.0, ty * 1.15 - 2.0, tz * 1.15, tw * 1.15]);
            let grain_n = grain.get([tx * 38.0, ty * 38.0, tz * 38.0, tw * 38.0]);
            let brushed = grain.get([tx * 5.2 + fbm, ty * 1.4, tz * 5.2, tw * 1.4]);
            let vein = (1.0
                - vein_noise
                    .get([tx * 2.7 + 1.7, ty * 2.7, tz * 2.7 - 3.1, tw * 2.7])
                    .abs())
            .powf(9.0);
            let vein_wide = (1.0
                - vein_noise
                    .get([tx * 1.45 - 4.0, ty * 1.45 + 2.0, tz * 1.45, tw * 1.45])
                    .abs())
            .powf(4.0);
            let macro_shadow = macro_n * 0.06 + broad * 0.04;

            let (bright, tint_r, tint_g, tint_b) = match style {
                BlockStyle::Rock => {
                    let aggregate = (grain_n.abs() - 0.35).max(0.0) * 0.32;
                    let slab = (u * two_pi * 3.0 + v * two_pi * 2.0 + broad * 2.5).sin() * 0.12;
                    let sediment =
                        (u * two_pi * 1.35 - v * two_pi * 3.10 + macro_n * 3.4).sin() * 0.10;
                    let weathered_face = if vein_wide > 0.42 {
                        (vein_wide - 0.42) * 0.28
                    } else {
                        0.0
                    };
                    let mineral_wash = (macro_n * 0.045 + broad * 0.03) as f32;
                    let b = 0.86 + macro_shadow * 1.25 + fbm * 0.28 + micro * 0.18
                        - cell_n * 0.34
                        - vein * 0.28
                        + (strat.abs() - 0.5) * 0.18
                        + aggregate
                        + slab
                        + sediment
                        - weathered_face;
                    let d = (fbm * 0.045 + broad * 0.025) as f32;
                    (
                        b,
                        d + mineral_wash + (sediment as f32) * 0.035,
                        (vein_wide as f32) * 0.030 - mineral_wash * 0.25,
                        -d * 0.7 - mineral_wash * 0.45 - (weathered_face as f32) * 0.035,
                    )
                }
                BlockStyle::Soil => {
                    let pore = (grain_n.abs() - 0.20).max(0.0) * 0.10;
                    let b = 0.82 + macro_shadow * 1.2 + fbm * 0.22 + micro * 0.19 - cell_n * 0.10
                        + pore;
                    (
                        b,
                        (micro as f32) * 0.07 + (broad as f32) * 0.025,
                        -(micro as f32) * 0.035,
                        -(cell_n as f32) * 0.015,
                    )
                }
                BlockStyle::Grass => {
                    let blade = detail.get([tx * 22.0, ty * 2.2, tz * 22.0, tw * 2.2]);
                    let moss_patch = macro_n.max(0.0) * 0.16;
                    let soil_fleck = if grain_n < -0.48 { 0.22 } else { 0.0 };
                    let meadow_wave =
                        (u * two_pi * 1.75 + v * two_pi * 2.35 + broad * 2.2).sin() * 0.22;
                    let meadow_cross =
                        (u * two_pi * 3.65 - v * two_pi * 1.45 + macro_n * 2.8).sin() * 0.16;
                    let lush_patch = macro_n.max(0.0) * 0.22;
                    let dry_patch = if vein_wide > 0.50 {
                        (vein_wide - 0.50) * 0.62
                    } else {
                        0.0
                    };
                    let b = 0.78
                        + macro_shadow
                        + fbm * 0.18
                        + blade * 0.32
                        + micro * 0.10
                        + moss_patch
                        + meadow_wave
                        + meadow_cross
                        + lush_patch * 0.50
                        - soil_fleck * 0.90
                        - dry_patch * 1.05;
                    (
                        b,
                        (dry_patch as f32) * 0.16 - (soil_fleck as f32) * 0.06,
                        (blade as f32) * 0.12
                            + (moss_patch as f32) * 0.12
                            + (meadow_wave as f32) * 0.12
                            + (meadow_cross as f32) * 0.09
                            + (lush_patch as f32) * 0.18
                            - (dry_patch as f32) * 0.06,
                        (lush_patch as f32) * 0.05
                            - (soil_fleck as f32) * 0.04
                            - (dry_patch as f32) * 0.07,
                    )
                }
                BlockStyle::Sand => {
                    let ripple = (v * two_pi * 7.0 + broad * 2.4 + fbm * 2.0).sin() * 0.18;
                    let dune = (u * two_pi * 2.2 + v * two_pi * 0.8 + macro_n).sin() * 0.10;
                    let mineral = (grain_n - 0.42).max(0.0) * 0.14;
                    (
                        0.88 + macro_shadow * 0.85 + fbm * 0.14 + micro * 0.16 + ripple + dune
                            + mineral,
                        mineral as f32 * 0.07 + (dune as f32) * 0.04,
                        -(ripple as f32) * 0.03,
                        -(mineral as f32) * 0.03,
                    )
                }
                BlockStyle::Water => {
                    let ripple_a = (u * two_pi * 4.0 + broad * 2.0 + fbm * 3.0).sin() * 0.05;
                    let ripple_b = (v * two_pi * 3.0 + macro_n * 2.0 + fbm * 2.0).cos() * 0.05;
                    let caustic = vein_wide * 0.07;
                    (
                        1.0 + ripple_a + ripple_b + micro * 0.05 + caustic,
                        0.0,
                        caustic as f32 * 0.04,
                        0.04,
                    )
                }
                BlockStyle::Wood => {
                    let du = u - 0.5;
                    let dv = v - 0.5;
                    let r = (du * du + dv * dv).sqrt();
                    let rings = (r * 32.0 + fbm * 2.0 + broad * 1.2).sin();
                    let streak = detail.get([tx * 20.0, ty * 2.5, tz * 20.0, tw * 2.5]);
                    let long_grain = brushed * 0.12;
                    let b = 0.86
                        + macro_shadow * 0.8
                        + rings * 0.11
                        + streak * 0.17
                        + long_grain
                        + micro * 0.08;
                    (
                        b,
                        (rings as f32) * 0.035 + (long_grain as f32) * 0.025,
                        0.0,
                        -(long_grain as f32) * 0.015,
                    )
                }
                BlockStyle::Leaves => {
                    let canopy = cell_n * 0.26 + macro_n.max(0.0) * 0.10;
                    let twig = if grain_n < -0.62 { 0.10 } else { 0.0 };
                    let b = 0.76 + macro_shadow + canopy + fbm * 0.17 + micro * 0.11 - twig * 0.75;
                    (
                        b,
                        -(twig as f32) * 0.035,
                        (canopy as f32) * 0.08,
                        -(twig as f32) * 0.025,
                    )
                }
                BlockStyle::Snow => {
                    let sparkle = detail.get([tx * 40.0, ty * 40.0, tz * 40.0, tw * 40.0]);
                    let sp = if sparkle > 0.72 {
                        (sparkle - 0.72) * 1.8
                    } else {
                        0.0
                    };
                    (
                        0.96 + macro_shadow * 0.45 + fbm * 0.05 + micro * 0.03 + sp,
                        0.0,
                        0.0,
                        0.0,
                    )
                }
                BlockStyle::Ice => {
                    let facet = perlin.get([tx * 3.0, ty * 3.0, tz * 3.0, tw * 3.0]);
                    let sparkle = detail.get([tx * 32.0, ty * 32.0, tz * 32.0, tw * 32.0]);
                    let sp = if sparkle > 0.78 {
                        (sparkle - 0.78) * 2.0
                    } else {
                        0.0
                    };
                    let crystalline = vein_wide * 0.08;
                    (
                        0.92 + macro_shadow * 0.6 + facet * 0.10 + crystalline + sp,
                        0.0,
                        crystalline as f32 * 0.025,
                        0.045,
                    )
                }
                BlockStyle::Lava => {
                    let heat = cell_n.max(vein_wide);
                    let crust = (1.0 - heat) * 0.35;
                    let b = 0.48 + macro_n.abs() * 0.10 + heat * 0.72 + fbm * 0.16 - crust;
                    (
                        b,
                        (heat as f32) * 0.38,
                        (heat as f32) * 0.10 + (vein as f32) * 0.05,
                        -(crust as f32) * 0.12,
                    )
                }
                BlockStyle::Strata => {
                    // Three thick world-readable bands. Thin 6-band
                    // stripes averaged to mud under mips at flying
                    // distance; 3 bands survive an 8× box filter.
                    let band = ((v * 3.0 + strat * 0.12).floor() as i32).rem_euclid(3);
                    let (mul, tr, tg, tb) = match band {
                        0 => (0.42, 0.22, -0.10, 0.18),
                        1 => (1.22, 0.08, 0.10, -0.06),
                        _ => (0.70, 0.18, 0.00, -0.14),
                    };
                    let grit = grain_n.abs() * 0.08;
                    let crack = cell_n * 0.22;
                    (
                        mul + macro_shadow * 0.5 + fbm * 0.08 + grit - crack,
                        tr,
                        tg,
                        tb,
                    )
                }
                BlockStyle::Metal => {
                    // Large panels, not a high-contrast waffle. Fine grout
                    // turned into a checkerboard under flying-distance mips.
                    let cells = 2.0;
                    let px = (u * cells).fract();
                    let py = (v * cells).fract();
                    let grout = if px < 0.05 || py < 0.05 || px > 0.95 || py > 0.95 {
                        0.78
                    } else {
                        1.0
                    };
                    let rivet_u = (px - 0.14).abs() + (py - 0.14).abs();
                    let rivet = if rivet_u < 0.05 { 0.18 } else { 0.0 };
                    let brush = (u * 24.0 + fbm * 2.0).sin() * 0.07;
                    let b = 0.92 + macro_shadow * 0.35 + brush + micro * 0.04;
                    (
                        b * grout - rivet,
                        (brush as f32) * 0.02,
                        0.0,
                        -(grout as f32 - 1.0) * 0.04,
                    )
                }
                BlockStyle::Crystal => {
                    let facet = cell_n;
                    let grout = facet * 0.62;
                    let sparkle = detail.get([tx * 32.0, ty * 32.0, tz * 32.0, tw * 32.0]);
                    let sp = if sparkle > 0.62 {
                        (sparkle - 0.62) * 3.1
                    } else {
                        0.0
                    };
                    let face = macro_n * 0.18 + fbm * 0.10;
                    (
                        0.82 + face + sp - grout,
                        (sp as f32) * 0.12 + 0.06,
                        (sp as f32) * 0.16 + (face as f32) * 0.05,
                        0.18 + (sp as f32) * 0.16 - (grout as f32) * 0.08,
                    )
                }
                BlockStyle::Energy => {
                    let flow = ((u * 7.0 + v * 0.40 + fbm * 0.8).sin() * 0.5 + 0.5).powf(1.8);
                    let core = (flow - 0.50).max(0.0) * 1.8;
                    let vein_glow = vein * 0.35;
                    (
                        0.42 + flow * 0.90 + micro * 0.08 + vein_glow,
                        -(core as f32) * 0.06,
                        (flow as f32) * 0.22,
                        (core as f32) * 0.28 + 0.10,
                    )
                }
            };

            let bright = bright.clamp(0.32, 1.55) as f32;
            let mut r = (base[0] * bright + tint_r).clamp(0.0, 1.0);
            let mut g = (base[1] * bright + tint_g).clamp(0.0, 1.0);
            let mut bl = (base[2] * bright + tint_b).clamp(0.0, 1.0);

            // Thin 1px lip only. A 10% bevel ate the mip average and
            // washed flying-distance faces into grey-brown.
            let margin = 2.0_f32;
            let dxe = (x as f32).min((size - 1 - x) as f32);
            let dye = (y as f32).min((size - 1 - y) as f32);
            let edge = dxe.min(dye);
            let mut bevel = if edge < margin {
                0.55 + 0.45 * (edge / margin)
            } else {
                1.0
            };
            if edge < 0.6 {
                bevel *= 0.72;
            }
            r = (r * bevel).clamp(0.0, 1.0);
            g = (g * bevel).clamp(0.0, 1.0);
            bl = (bl * bevel).clamp(0.0, 1.0);

            data.push((r * 255.0).round() as u8);
            data.push((g * 255.0).round() as u8);
            data.push((bl * 255.0).round() as u8);
            data.push(alpha);
        }
    }

    BlockSwatch {
        block,
        name,
        width: size,
        height: size,
        rgba: data,
    }
}

// ---------------------------------------------------------------------------
// Helpers -------------------------------------------------------------------
// ---------------------------------------------------------------------------

/// Per-block HDR emissive. Fast has bloom intensity 0, so this is the
/// only glow those voxels get; Cinematic adds bloom on top, so plasma
/// stays chromatic and below the ACES clip while crystals stay saturated.
fn emissive_for_block(block: BlockType) -> LinearRgba {
    if !block.is_emissive() {
        return LinearRgba::BLACK;
    }
    let lin = block.color().to_linear();
    match block {
        BlockType::Crystal | BlockType::LuminiteCrystal => LinearRgba::rgb(
            lin.red * 0.55,
            lin.green * 3.40,
            lin.blue * 4.10,
        ),
        BlockType::CrystalMagenta => LinearRgba::rgb(
            lin.red * 3.60,
            lin.green * 0.45,
            lin.blue * 3.10,
        ),
        BlockType::PlasmaFlow | BlockType::NeonCyan => LinearRgba::rgb(
            lin.red * 0.70 + 0.01,
            lin.green * 1.05 + 0.04,
            lin.blue * 1.20 + 0.05,
        ),
        BlockType::Lava => LinearRgba::rgb(lin.red * 3.80, lin.green * 1.55, lin.blue * 0.05),
        _ => LinearRgba::rgb(
            lin.red * 3.2 + 0.35,
            lin.green * 3.2 + 0.35,
            lin.blue * 3.2 + 0.35,
        ),
    }
}

fn roughness_for_block(block: BlockType) -> f32 {
    if matches!(
        block,
        BlockType::PlatingWhite
            | BlockType::PlatingTeal
            | BlockType::RoadDeck
            | BlockType::RoadMarking
            | BlockType::ShipHullDark
            | BlockType::ShipHullAlloy
    ) {
        0.38
    } else if matches!(
        block,
        BlockType::Crystal
            | BlockType::CrystalMagenta
            | BlockType::CrystalGreen
            | BlockType::LuminiteCrystal
            | BlockType::IridiumVein
            | BlockType::Ice
            | BlockType::HoloPanel
            | BlockType::CockpitGlass
    ) {
        0.14
    } else if block.is_emissive() {
        0.42
    } else if matches!(block, BlockType::Sand | BlockType::RedSand | BlockType::GlowSand) {
        0.94
    } else {
        0.84
    }
}

fn reflectance_for_block(block: BlockType) -> f32 {
    if matches!(
        block,
        BlockType::PlatingWhite
            | BlockType::PlatingTeal
            | BlockType::ShipHullAlloy
            | BlockType::RoadMarking
    ) {
        0.14
    } else if matches!(
        block,
        BlockType::Crystal
            | BlockType::CrystalMagenta
            | BlockType::CrystalGreen
            | BlockType::Ice
            | BlockType::HoloPanel
    ) {
        0.10
    } else {
        0.045
    }
}

fn make_repeating_image(w: u32, h: u32, data: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        // Nearest mips keep thick strata / grass / sand hues from
        // blending into a single flying-distance mud.
        mipmap_filter: ImageFilterMode::Nearest,
        ..ImageSamplerDescriptor::linear()
    });
    image
}

fn load_png_as_repeating_image(path: &str) -> Option<Image> {
    let bytes = std::fs::read(path).ok()?;
    let decoded = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (w, h) = decoded.dimensions();
    Some(make_repeating_image(w, h, decoded.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luma(pixel: &[u8]) -> u8 {
        ((pixel[0] as u16 * 54 + pixel[1] as u16 * 183 + pixel[2] as u16 * 19) / 256) as u8
    }

    fn luma_range(swatch: &BlockSwatch) -> u8 {
        let mut min = u8::MAX;
        let mut max = u8::MIN;
        for pixel in swatch.rgba.chunks_exact(4) {
            let y = luma(pixel);
            min = min.min(y);
            max = max.max(y);
        }
        max.saturating_sub(min)
    }

    fn unique_rgb_count(swatch: &BlockSwatch) -> usize {
        let mut unique = std::collections::BTreeSet::new();
        for pixel in swatch.rgba.chunks_exact(4) {
            unique.insert([pixel[0], pixel[1], pixel[2]]);
        }
        unique.len()
    }

    fn downsample_signature_count(swatch: &BlockSwatch, step: usize) -> usize {
        let mut unique = std::collections::BTreeSet::new();
        let width = swatch.width as usize;
        let height = swatch.height as usize;

        for y in (0..height).step_by(step) {
            for x in (0..width).step_by(step) {
                let mut acc = [0u32; 3];
                let mut count = 0u32;
                for yy in y..(y + step).min(height) {
                    for xx in x..(x + step).min(width) {
                        let i = (yy * width + xx) * 4;
                        acc[0] += swatch.rgba[i] as u32;
                        acc[1] += swatch.rgba[i + 1] as u32;
                        acc[2] += swatch.rgba[i + 2] as u32;
                        count += 1;
                    }
                }
                if count > 0 {
                    unique.insert([
                        (acc[0] / count / 8) as u8,
                        (acc[1] / count / 8) as u8,
                        (acc[2] / count / 8) as u8,
                    ]);
                }
            }
        }

        unique.len()
    }

    fn mean_rgb(swatch: &BlockSwatch) -> [f32; 3] {
        let mut acc = [0.0f32; 3];
        let n = (swatch.rgba.len() / 4) as f32;
        for pixel in swatch.rgba.chunks_exact(4) {
            acc[0] += pixel[0] as f32;
            acc[1] += pixel[1] as f32;
            acc[2] += pixel[2] as f32;
        }
        [acc[0] / n / 255.0, acc[1] / n / 255.0, acc[2] / n / 255.0]
    }

    fn swatch_for(swatches: &[BlockSwatch], block: BlockType) -> &BlockSwatch {
        swatches
            .iter()
            .find(|swatch| swatch.block == block)
            .expect("block swatch exists")
    }

    #[test]
    fn built_in_materials_keep_detail_after_far_distance_downsampling() {
        assert!(BUILTIN_SWATCH_SIZE >= 128);

        let swatches = bake_all_block_swatches(BUILTIN_SWATCH_SIZE);
        let stone = swatch_for(&swatches, BlockType::Stone);
        let grass = swatch_for(&swatches, BlockType::Grass);
        let lava = swatch_for(&swatches, BlockType::Lava);
        let stone_signatures = downsample_signature_count(stone, 8);
        let grass_signatures = downsample_signature_count(grass, 8);
        let lava_signatures = downsample_signature_count(lava, 8);

        assert!(unique_rgb_count(stone) > 512);
        assert!(luma_range(stone) > 54);
        let red = swatch_for(&swatches, BlockType::RedStone);
        let crystal = swatch_for(&swatches, BlockType::Crystal);
        let plate = swatch_for(&swatches, BlockType::PlatingWhite);
        assert!(
            luma_range(red) > 90,
            "mesa strata tile is still too flat ({})",
            luma_range(red)
        );
        assert!(
            luma_range(crystal) > 80,
            "crystal tile is still too flat ({})",
            luma_range(crystal)
        );
        assert!(
            luma_range(plate) > 80,
            "metal tile is still too flat ({})",
            luma_range(plate)
        );
        assert!(
            stone_signatures > 20,
            "stone only preserved {stone_signatures} far-distance material signatures"
        );
        assert!(
            grass_signatures > 18,
            "grass only preserved {grass_signatures} far-distance material signatures"
        );
        assert!(
            lava_signatures > 14,
            "lava only preserved {lava_signatures} far-distance material signatures"
        );
    }

    #[test]
    fn flying_distance_swatch_means_keep_grass_rock_and_sand_apart() {
        let swatches = bake_all_block_swatches(128);
        let grass = mean_rgb(swatch_for(&swatches, BlockType::Grass));
        let red = mean_rgb(swatch_for(&swatches, BlockType::RedStone));
        let clay = mean_rgb(swatch_for(&swatches, BlockType::MesaClay));
        let sand = mean_rgb(swatch_for(&swatches, BlockType::RedSand));
        let violet = mean_rgb(swatch_for(&swatches, BlockType::VioletStone));
        let crystal = mean_rgb(swatch_for(&swatches, BlockType::Crystal));
        let lava = mean_rgb(swatch_for(&swatches, BlockType::Lava));
        assert!(
            grass[1] > red[1] + 0.18,
            "grass mean lost its green vs brick ({grass:?} vs {red:?})"
        );
        assert!(
            red[0] > grass[0] + 0.18,
            "brick mean lost its rust vs grass ({red:?} vs {grass:?})"
        );
        assert!(
            clay[1] > red[1] + 0.12,
            "cream stripe should stay brighter than brick ({clay:?} vs {red:?})"
        );
        assert!(
            sand[0] > sand[1] + 0.08,
            "red sand should stay warm ({sand:?})"
        );
        assert!(
            violet[2] > violet[0] + 0.08,
            "violet band should stay cool ({violet:?})"
        );
        assert!(
            crystal[2] > crystal[0] + 0.20,
            "crystal should stay cyan ({crystal:?})"
        );
        assert!(
            lava[0] > lava[2] + 0.35,
            "lava should stay molten ({lava:?})"
        );
    }

    #[test]
    fn translucent_builtin_world_materials_avoid_sorted_alpha_blend() {
        for block in [
            BlockType::Water,
            BlockType::Ice,
            BlockType::CockpitGlass,
            BlockType::IridiumVein,
            BlockType::CrystalGreen,
            BlockType::HoloPanel,
        ] {
            assert_eq!(
                terrain_alpha_mode_for_block(block),
                AlphaMode::AlphaToCoverage,
                "{block:?} should stay out of Bevy's sorted alpha-blend terrain path"
            );
        }
        for block in [
            BlockType::Crystal,
            BlockType::CrystalMagenta,
            BlockType::LuminiteCrystal,
            BlockType::Lava,
            BlockType::PlasmaFlow,
        ] {
            assert_eq!(
                terrain_alpha_mode_for_block(block),
                AlphaMode::Opaque,
                "{block:?} must occlude so the hero emissive reads as a mass"
            );
        }
    }

    #[test]
    fn hero_emissive_materials_stay_chromatic() {
        let crystal = emissive_for_block(BlockType::Crystal);
        assert!(
            crystal.blue > crystal.red * 4.0 && crystal.green > crystal.red * 3.0,
            "crystal emissive lost cyan ({crystal:?})"
        );
        let magenta = emissive_for_block(BlockType::CrystalMagenta);
        assert!(
            magenta.red > magenta.green * 4.0 && magenta.blue > magenta.green * 3.0,
            "magenta emissive lost chroma ({magenta:?})"
        );
        let plasma = emissive_for_block(BlockType::PlasmaFlow);
        let peak = plasma.red.max(plasma.green).max(plasma.blue);
        assert!(
            peak < 1.6,
            "plasma emissive peak {peak:.3} will ACES-clip to white"
        );
        assert!(plasma.blue > plasma.red * 3.0);
        let lava = emissive_for_block(BlockType::Lava);
        assert!(lava.red > lava.blue * 8.0, "lava emissive went cream ({lava:?})");
    }
}
