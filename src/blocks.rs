//! Block types + palette.
//!
//! Ported and expanded from `lib/voxel/blocks.ts`. Biome-specific surface
//! blocks (snow, sand, jungle leaves, tundra grass variants, etc.) live here
//! so the terrain module can pick them per-biome without branching on strings.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Packed voxel value. `0` = air; everything else is a block id.
/// `u16` gives plenty of room for future blocks without another migration.
pub type Voxel = u16;

pub const AIR: Voxel = 0;

/// Packed material/texture id stored next to each voxel.
///
/// `0` means "use this block's default material"; built-in materials use the
/// same numeric id as their matching [`BlockType`], and custom PNG materials
/// are assigned ids starting at [`CUSTOM_MATERIAL_BASE`].
pub type MaterialId = u16;

pub const DEFAULT_MATERIAL: MaterialId = 0;
pub const CUSTOM_MATERIAL_BASE: MaterialId = 1024;

#[inline]
pub fn default_material_for_voxel(v: Voxel) -> MaterialId {
    if v == AIR {
        DEFAULT_MATERIAL
    } else {
        v as MaterialId
    }
}

#[inline]
pub fn effective_material_for_voxel(v: Voxel, material: MaterialId) -> MaterialId {
    if material == DEFAULT_MATERIAL {
        default_material_for_voxel(v)
    } else {
        material
    }
}

#[inline]
#[allow(dead_code)]
pub fn normalize_material_for_voxel(v: Voxel, material: MaterialId) -> MaterialId {
    if v == AIR || material == default_material_for_voxel(v) {
        DEFAULT_MATERIAL
    } else {
        material
    }
}

#[inline]
pub fn material_is_custom(material: MaterialId) -> bool {
    material >= CUSTOM_MATERIAL_BASE
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockType {
    Air = 0,
    Stone = 1,
    Dirt = 2,
    Grass = 3,
    Sand = 4,
    Water = 5,
    Wood = 6,
    Leaves = 7,
    Snow = 8,
    Ice = 9,
    TundraGrass = 10,
    JungleLeaves = 11,
    SavannaGrass = 12,
    Gravel = 13,
    Bedrock = 14,
    /// Iron-rich red sandstone — Grand-Canyon / Sedona surface layer.
    RedSand = 15,
    /// Deeper canyon sandstone, brick-orange, used for cliff faces.
    RedStone = 16,
    /// Pale buff mesa cap-rock — banded between the reds.
    MesaClay = 17,
    /// Dark moss-coated karst limestone — Zhangjiajie pillars.
    MossStone = 18,
    /// Light grey limestone — bright karst columns.
    Limestone = 19,
    /// Cyan-violet alien crystal — towering Pandora-style spires.
    /// Translucent, non-opaque so light bleeds through the spires.
    Crystal = 20,
    /// Near-black volcanic basalt — Io / Mars cap rock.
    Basalt = 21,
    /// Glowing lava — non-solid, non-opaque (treated like water for
    /// face culling so it pools in flat sheets in lava channels).
    Lava = 22,
    /// Magenta bioluminescent ground cover — alien reef floors.
    AlienMoss = 23,
    /// Bone-white organic rock — alien reef pillar arches.
    BoneRock = 24,
    /// Pale glowing sand — crystal-spire biome floor.
    GlowSand = 25,
    /// Near-black shuttle hull plating.
    ShipHullDark = 26,
    /// Brushed bright alloy for wing edges and nose cones.
    ShipHullAlloy = 27,
    /// Smoked cyan cockpit glass.
    CockpitGlass = 28,
    /// Cyan emissive shuttle trim / runway strip.
    NeonCyan = 29,
    /// Magenta emissive shuttle trim / alien signage.
    NeonMagenta = 30,
    /// Amber emissive warning light / weapon port.
    NeonAmber = 31,
    /// Hot engine-core block.
    EngineCore = 32,
    /// Bright cyan-blue cavern crystal — reference "Luminite".
    LuminiteCrystal = 33,
    /// Orange magnetic ore — reference "Magnetite".
    MagnetiteOre = 34,
    /// Deep purple rare vein — reference "Iridium".
    IridiumVein = 35,
}

/// Voxel ids for the three mineable neon resources (HUD + telemetry).
pub const VOXEL_LUMINITE: Voxel = BlockType::LuminiteCrystal as Voxel;
pub const VOXEL_MAGNETITE: Voxel = BlockType::MagnetiteOre as Voxel;
pub const VOXEL_IRIDIUM: Voxel = BlockType::IridiumVein as Voxel;

impl BlockType {
    #[inline]
    pub fn is_solid(self) -> bool {
        !matches!(self, BlockType::Air | BlockType::Water)
    }

    #[inline]
    pub fn is_opaque(self) -> bool {
        !matches!(
            self,
            BlockType::Air
                | BlockType::Water
                | BlockType::Leaves
                | BlockType::JungleLeaves
                | BlockType::Ice
                | BlockType::CockpitGlass
        )
    }

    /// Is this block a bioluminescent / emissive block? Emissive blocks
    /// get HDR-boosted vertex colors (see `voxel_color`) so they bloom
    /// through the camera's bloom pass and read as glowing neon — matches
    /// the Pandora / sci-fi reference palette.
    #[inline]
    pub fn is_emissive(self) -> bool {
        matches!(
            self,
            BlockType::Lava
                | BlockType::Crystal
                | BlockType::AlienMoss
                | BlockType::GlowSand
                | BlockType::NeonCyan
                | BlockType::NeonMagenta
                | BlockType::NeonAmber
                | BlockType::EngineCore
                | BlockType::LuminiteCrystal
                | BlockType::MagnetiteOre
                | BlockType::IridiumVein
        )
    }

    /// Vertex colour (sRGB). Real texture atlas will replace this later.
    pub fn color(self) -> Color {
        match self {
            BlockType::Air => Color::NONE,
            BlockType::Stone => Color::srgb(0.34, 0.36, 0.44),
            BlockType::Dirt => Color::srgb(0.24, 0.15, 0.11),
            BlockType::Grass => Color::srgb(0.11, 0.40, 0.15),
            BlockType::Sand => Color::srgb(0.76, 0.67, 0.45),
            // Turquoise energy-water read (concept underground river).
            BlockType::Water => Color::srgba(0.06, 0.78, 0.92, 0.62),
            BlockType::Wood => Color::srgb(0.24, 0.14, 0.08),
            BlockType::Leaves => Color::srgb(0.06, 0.32, 0.11),
            BlockType::Snow => Color::srgb(0.96, 0.97, 0.99),
            BlockType::Ice => Color::srgba(0.70, 0.88, 0.98, 0.85),
            BlockType::TundraGrass => Color::srgb(0.62, 0.76, 0.55),
            BlockType::JungleLeaves => Color::srgb(0.03, 0.38, 0.13),
            BlockType::SavannaGrass => Color::srgb(0.46, 0.50, 0.20),
            BlockType::Gravel => Color::srgb(0.42, 0.40, 0.45),
            BlockType::Bedrock => Color::srgb(0.12, 0.12, 0.14),
            // Sedona red — saturated rust-orange surface dust.
            BlockType::RedSand => Color::srgb(0.92, 0.46, 0.24),
            // Brick-red sandstone cliff body.
            BlockType::RedStone => Color::srgb(0.76, 0.32, 0.20),
            // Pale yellow mesa cap, the bright stripe between reds.
            BlockType::MesaClay => Color::srgb(0.94, 0.76, 0.48),
            // Dark mossy limestone — wet karst pillar bodies.
            BlockType::MossStone => Color::srgb(0.20, 0.31, 0.25),
            // Bright pale limestone — sun-lit karst sides.
            BlockType::Limestone => Color::srgb(0.86, 0.84, 0.76),
            // Alien crystal — saturated cyan-violet, slightly translucent.
            BlockType::Crystal => Color::srgba(0.18, 0.72, 1.00, 0.70),
            // Volcanic basalt — dark, but not unreadable black. Keeping
            // it above pure black makes ledges and jump targets visible
            // under strong bloom from nearby lava.
            BlockType::Basalt => Color::srgb(0.26, 0.24, 0.28),
            // Lava — saturated orange-red. Read as glowing thanks to
            // the player camera's HDR + tonemapping.
            BlockType::Lava => Color::srgba(1.00, 0.48, 0.12, 0.88),
            // Bioluminescent magenta moss for alien reef floors.
            BlockType::AlienMoss => Color::srgb(0.26, 0.06, 0.44),
            // Bone-white organic pillar rock.
            BlockType::BoneRock => Color::srgb(0.62, 0.54, 0.70),
            // Pale glowing crystal-biome sand — almost white-cyan.
            BlockType::GlowSand => Color::srgb(0.12, 0.30, 0.42),
            BlockType::ShipHullDark => Color::srgb(0.055, 0.065, 0.095),
            BlockType::ShipHullAlloy => Color::srgb(0.56, 0.68, 0.78),
            BlockType::CockpitGlass => Color::srgba(0.03, 0.48, 0.78, 0.50),
            BlockType::NeonCyan => Color::srgb(0.00, 0.92, 1.00),
            BlockType::NeonMagenta => Color::srgb(1.00, 0.04, 0.82),
            BlockType::NeonAmber => Color::srgb(1.00, 0.52, 0.06),
            BlockType::EngineCore => Color::srgb(0.06, 0.76, 1.00),
            BlockType::LuminiteCrystal => Color::srgba(0.12, 0.82, 1.00, 0.68),
            BlockType::MagnetiteOre => Color::srgb(0.92, 0.38, 0.08),
            BlockType::IridiumVein => Color::srgba(0.62, 0.12, 0.95, 0.72),
        }
    }

    pub fn from_voxel(v: Voxel) -> Self {
        match v {
            1 => BlockType::Stone,
            2 => BlockType::Dirt,
            3 => BlockType::Grass,
            4 => BlockType::Sand,
            5 => BlockType::Water,
            6 => BlockType::Wood,
            7 => BlockType::Leaves,
            8 => BlockType::Snow,
            9 => BlockType::Ice,
            10 => BlockType::TundraGrass,
            11 => BlockType::JungleLeaves,
            12 => BlockType::SavannaGrass,
            13 => BlockType::Gravel,
            14 => BlockType::Bedrock,
            15 => BlockType::RedSand,
            16 => BlockType::RedStone,
            17 => BlockType::MesaClay,
            18 => BlockType::MossStone,
            19 => BlockType::Limestone,
            20 => BlockType::Crystal,
            21 => BlockType::Basalt,
            22 => BlockType::Lava,
            23 => BlockType::AlienMoss,
            24 => BlockType::BoneRock,
            25 => BlockType::GlowSand,
            26 => BlockType::ShipHullDark,
            27 => BlockType::ShipHullAlloy,
            28 => BlockType::CockpitGlass,
            29 => BlockType::NeonCyan,
            30 => BlockType::NeonMagenta,
            31 => BlockType::NeonAmber,
            32 => BlockType::EngineCore,
            33 => BlockType::LuminiteCrystal,
            34 => BlockType::MagnetiteOre,
            35 => BlockType::IridiumVein,
            _ => BlockType::Air,
        }
    }
}

impl From<BlockType> for Voxel {
    #[inline]
    fn from(b: BlockType) -> Self {
        b as Voxel
    }
}

/// Fast voxel → solid? (without converting through the enum).
/// AIR (0), Water (5) and Lava (22) are non-solid for collision.
#[inline]
pub fn voxel_is_solid(v: Voxel) -> bool {
    !matches!(v, 0 | 5 | 22)
}

/// Fast voxel -> can weapons intentionally hit and destroy this block?
/// Lava is not solid for movement, but it is still a visible voxel; if
/// the crosshair is on it, shots should connect instead of passing
/// through and confusing the player.
#[inline]
pub fn voxel_is_weapon_target(v: Voxel) -> bool {
    !matches!(v, 0 | 5)
}

/// Fast voxel → opaque? (used for face-culling).
/// Air (0), water (5), leaves (7), ice (9), jungle leaves (11),
/// crystal (20), lava (22), cockpit (28), luminite/iridium glass are non-opaque.
#[inline]
pub fn voxel_is_opaque(v: Voxel) -> bool {
    !matches!(v, 0 | 5 | 7 | 9 | 11 | 20 | 22 | 28 | 33 | 35)
}

/// Fast voxel → is this block bioluminescent? Emissive blocks get
/// HDR-boosted vertex colors that bloom through the world camera's
/// bloom pass, producing a true neon glow (lava rivers, crystal spires,
/// alien moss, glow-sand).
#[inline]
pub fn voxel_is_emissive(v: Voxel) -> bool {
    // Lava=22, Crystal=20, AlienMoss=23, GlowSand=25, neon ores 33–35, Ice=9.
    // Ice gets a whisper of glow so glacier biomes shimmer at night.
    matches!(v, 9 | 20 | 22 | 23 | 25 | 29 | 30 | 31 | 32 | 33 | 34 | 35)
}

#[inline]
pub fn voxel_color(v: Voxel) -> [f32; 4] {
    // Convert the block's designer sRGB colour to linear and then, for
    // emissive blocks, multiply the linear RGB by a generous scalar so
    // values exceed 1.0. With the world camera running HDR + bloom,
    // anything above ~1.0 survives tonemap and blooms — giving lava,
    // crystal, alien moss and glow-sand a true neon halo without any
    // custom shader work.
    let mut c = BlockType::from_voxel(v).color().to_linear().to_f32_array();
    match v {
        5 => {
            // Water — soft turquoise bloom (concept energy river / cavern pool).
            c[0] *= 1.25;
            c[1] *= 2.0;
            c[2] *= 2.15;
        }
        22 => {
            // Lava — hot orange, dialled back so VolcanicWaste fields
            // don't drown the screen in bloom. Still glows, just
            // doesn't blind the player.
            c[0] *= 2.0;
            c[1] *= 1.15;
            c[2] *= 0.55;
        }
        20 => {
            // Crystal — cyan-violet spires. Subtler than lava so the
            // biome still reads as sky-coloured rock, not lightning.
            c[0] *= 1.7;
            c[1] *= 3.0;
            c[2] *= 4.2;
        }
        23 => {
            // AlienMoss — bioluminescent magenta/violet.
            c[0] *= 3.0;
            c[1] *= 0.9;
            c[2] *= 5.2;
        }
        25 => {
            // GlowSand — cool pale wash.
            c[0] *= 1.4;
            c[1] *= 2.2;
            c[2] *= 3.4;
        }
        29 => {
            c[0] *= 0.9;
            c[1] *= 3.5;
            c[2] *= 4.4;
        }
        30 => {
            c[0] *= 3.7;
            c[1] *= 0.8;
            c[2] *= 3.5;
        }
        31 => {
            c[0] *= 3.8;
            c[1] *= 2.2;
            c[2] *= 0.7;
        }
        32 => {
            c[0] *= 1.4;
            c[1] *= 3.0;
            c[2] *= 4.8;
        }
        9 => {
            // Ice — gentle rim glow so glaciers shimmer at night.
            c[0] *= 1.05;
            c[1] *= 1.15;
            c[2] *= 1.25;
        }
        33 => {
            // Luminite — brilliant aquamarine (concept cavern key light).
            c[0] *= 1.2;
            c[1] *= 3.6;
            c[2] *= 4.8;
        }
        34 => {
            // Magnetite — hot ember orange, reads as ore against cyan crystal.
            c[0] *= 4.2;
            c[1] *= 2.0;
            c[2] *= 0.55;
        }
        35 => {
            // Iridium — violet core glow.
            c[0] *= 2.8;
            c[1] *= 0.9;
            c[2] *= 4.6;
        }
        _ => {}
    }
    c
}

/// When a weapon destroys this voxel, how many inventory units drop (concept HUD).
#[inline]
pub fn ore_units_for_mined_voxel(v: Voxel) -> u32 {
    match v {
        VOXEL_LUMINITE => 2,
        VOXEL_MAGNETITE => 2,
        VOXEL_IRIDIUM => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lava_is_weapon_target_but_not_collision_solid() {
        let lava: Voxel = BlockType::Lava.into();
        assert!(!voxel_is_solid(lava));
        assert!(voxel_is_weapon_target(lava));
    }

    #[test]
    fn shuttle_blocks_map_from_voxel_ids() {
        assert_eq!(BlockType::from_voxel(26), BlockType::ShipHullDark);
        assert_eq!(BlockType::from_voxel(28), BlockType::CockpitGlass);
        assert_eq!(BlockType::from_voxel(32), BlockType::EngineCore);
        assert_eq!(BlockType::from_voxel(33), BlockType::LuminiteCrystal);
        assert_eq!(BlockType::from_voxel(35), BlockType::IridiumVein);
        assert!(voxel_is_emissive(BlockType::NeonCyan.into()));
        assert!(!voxel_is_opaque(BlockType::CockpitGlass.into()));
        assert!(!voxel_is_opaque(BlockType::LuminiteCrystal.into()));
        assert!(voxel_is_opaque(BlockType::MagnetiteOre.into()));
        assert!(ore_units_for_mined_voxel(VOXEL_LUMINITE) > 0);
    }
}
