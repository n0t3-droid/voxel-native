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
use bevy::render::texture::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use noise::{NoiseFn, Perlin};

use crate::blocks::{BlockType, MaterialId, CUSTOM_MATERIAL_BASE};

/// Folder (relative to cwd) from which the engine loads user-supplied
/// texture overrides and into which the Texture Viewer exports PNGs.
pub const TEX_DIR: &str = "textures";
pub const MATERIAL_DIR: &str = "textures/materials";

#[derive(Resource, Default)]
pub struct MaterialLibrary {
    pub handles: std::collections::BTreeMap<MaterialId, Handle<StandardMaterial>>,
    pub names: std::collections::BTreeMap<MaterialId, String>,
    pub custom_ids: Vec<MaterialId>,
    pub reload_requested: bool,
    pub status: String,
}

impl MaterialLibrary {
    pub fn rebuild(
        &mut self,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
    ) {
        self.handles.clear();
        self.names.clear();
        self.custom_ids.clear();

        for swatch in bake_all_block_swatches(96) {
            let image = images.add(make_repeating_image(
                swatch.width,
                swatch.height,
                swatch.rgba.clone(),
            ));
            let alpha = swatch.block.color().to_srgba().alpha;
            let emissive = if swatch.block.is_emissive() {
                let lin = swatch.block.color().to_linear();
                LinearRgba::rgb(
                    lin.red * 3.2 + 0.35,
                    lin.green * 3.2 + 0.35,
                    lin.blue * 3.2 + 0.35,
                )
            } else {
                LinearRgba::BLACK
            };
            let handle = materials.add(StandardMaterial {
                base_color: Color::WHITE.with_alpha(alpha),
                base_color_texture: Some(image),
                emissive,
                perceptual_roughness: 1.0,
                reflectance: 0.05,
                alpha_mode: if alpha < 0.99 {
                    AlphaMode::Blend
                } else {
                    AlphaMode::Opaque
                },
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
        (RedStone, "red_stone", BlockStyle::Rock),
        (MesaClay, "mesa_clay", BlockStyle::Rock),
        (MossStone, "moss_stone", BlockStyle::Rock),
        (Limestone, "limestone", BlockStyle::Rock),
        (Crystal, "crystal", BlockStyle::Ice),
        (Basalt, "basalt", BlockStyle::Rock),
        (Lava, "lava", BlockStyle::Lava),
        (AlienMoss, "alien_moss", BlockStyle::Grass),
        (BoneRock, "bone_rock", BlockStyle::Rock),
        (GlowSand, "glow_sand", BlockStyle::Sand),
        (ShipHullDark, "ship_hull_dark", BlockStyle::Rock),
        (ShipHullAlloy, "ship_hull_alloy", BlockStyle::Rock),
        (CockpitGlass, "cockpit_glass", BlockStyle::Ice),
        (NeonCyan, "neon_cyan", BlockStyle::Ice),
        (NeonMagenta, "neon_magenta", BlockStyle::Ice),
        (NeonAmber, "neon_amber", BlockStyle::Lava),
        (EngineCore, "engine_core", BlockStyle::Lava),
        (LuminiteCrystal, "luminite_crystal", BlockStyle::Ice),
        (MagnetiteOre, "magnetite_ore", BlockStyle::Rock),
        (IridiumVein, "iridium_vein", BlockStyle::Ice),
    ];
    list.iter()
        .map(|(b, n, style)| bake_block_swatch(*b, n, *style, size))
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum BlockStyle {
    Rock,
    Soil,
    Grass,
    Sand,
    Water,
    Wood,
    Leaves,
    Snow,
    Ice,
    Lava,
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

    let seed_base = block as u32;
    let perlin = Perlin::new(seed_base + 101);
    let detail = Perlin::new(seed_base + 202);
    let warp = Perlin::new(seed_base + 303);
    let strata = Perlin::new(seed_base + 404);
    let cell = Perlin::new(seed_base + 505);

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

            let (bright, tint_r, tint_g, tint_b) = match style {
                BlockStyle::Rock => {
                    let b = 0.90 + fbm * 0.22 + micro * 0.18 - cell_n * 0.25
                        + (strat.abs() - 0.5) * 0.14;
                    let d = fbm as f32 * 0.05;
                    (b, d, 0.0, -d)
                }
                BlockStyle::Soil => {
                    let b = 0.88 + fbm * 0.25 + micro * 0.22;
                    (b, (micro as f32) * 0.07, -(micro as f32) * 0.03, 0.0)
                }
                BlockStyle::Grass => {
                    let blade = detail.get([tx * 18.0, ty * 3.0, tz * 18.0, tw * 3.0]);
                    let b = 0.88 + fbm * 0.18 + blade * 0.22 + micro * 0.08;
                    (b, 0.0, (blade as f32) * 0.08, 0.0)
                }
                BlockStyle::Sand => {
                    let ripple = (v * two_pi * 6.0 + fbm * 2.0).sin() * 0.08;
                    (0.94 + fbm * 0.10 + micro * 0.14 + ripple, 0.0, 0.0, 0.0)
                }
                BlockStyle::Water => {
                    let ripple_a = (u * two_pi * 4.0 + fbm * 3.0).sin() * 0.05;
                    let ripple_b = (v * two_pi * 3.0 + fbm * 2.0).cos() * 0.05;
                    (1.0 + ripple_a + ripple_b + micro * 0.05, 0.0, 0.0, 0.03)
                }
                BlockStyle::Wood => {
                    let du = u - 0.5;
                    let dv = v - 0.5;
                    let r = (du * du + dv * dv).sqrt();
                    let rings = (r * 32.0 + fbm * 2.0).sin();
                    let streak = detail.get([tx * 20.0, ty * 2.5, tz * 20.0, tw * 2.5]);
                    let b = 0.88 + rings * 0.10 + streak * 0.18 + micro * 0.10;
                    (b, (rings as f32) * 0.03, 0.0, 0.0)
                }
                BlockStyle::Leaves => {
                    let b = 0.80 + cell_n * 0.30 + fbm * 0.18 + micro * 0.12;
                    (b, 0.0, (cell_n as f32) * 0.08, 0.0)
                }
                BlockStyle::Snow => {
                    let sparkle = detail.get([tx * 40.0, ty * 40.0, tz * 40.0, tw * 40.0]);
                    let sp = if sparkle > 0.72 {
                        (sparkle - 0.72) * 1.8
                    } else {
                        0.0
                    };
                    (0.97 + fbm * 0.05 + micro * 0.03 + sp, 0.0, 0.0, 0.0)
                }
                BlockStyle::Ice => {
                    let facet = perlin.get([tx * 3.0, ty * 3.0, tz * 3.0, tw * 3.0]);
                    let sparkle = detail.get([tx * 32.0, ty * 32.0, tz * 32.0, tw * 32.0]);
                    let sp = if sparkle > 0.78 {
                        (sparkle - 0.78) * 2.0
                    } else {
                        0.0
                    };
                    (0.94 + facet * 0.10 + sp, 0.0, 0.0, 0.04)
                }
                BlockStyle::Lava => {
                    let b = 0.70 + cell_n * 0.50 + fbm * 0.20;
                    (b, (cell_n as f32) * 0.25, (cell_n as f32) * 0.08, 0.0)
                }
            };

            let bright = bright.clamp(0.55, 1.30) as f32;
            let r = (base[0] * bright + tint_r).clamp(0.0, 1.0);
            let g = (base[1] * bright + tint_g).clamp(0.0, 1.0);
            let bl = (base[2] * bright + tint_b).clamp(0.0, 1.0);

            data.push((r * 255.0).round() as u8);
            data.push((g * 255.0).round() as u8);
            data.push((bl * 255.0).round() as u8);
            data.push(255);
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
