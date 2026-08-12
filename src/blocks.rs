//! Block types + palette.
//!
//! Ported and expanded from `lib/voxel/blocks.ts`. Biome-specific surface
//! blocks (snow, sand, jungle leaves, tundra grass variants, etc.) live here
//! so the terrain module can pick them per-biome without branching on strings.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::voxel_budget::EmissionBudget;

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
    /// Pale pink sakura / rose foliage for zen-garden and lush forest silhouettes.
    BlossomLeaves = 36,
    /// Smooth pale garden stone for zen paths, courtyards and modern walls.
    ZenStone = 37,
    /// Tall green bamboo cane / plant block for gardens and groves.
    Bamboo = 38,
    /// Soft pink fallen petals / flower mass for sakura ground detail.
    SakuraPetals = 39,
    /// Warm translucent paper wall for shoji screens and interior panels.
    ShojiPaper = 40,
    /// Charcoal ceramic roof tile for Japanese roofs and modern dark trim.
    RoofTile = 41,
    /// Warm woven floor block for tatami rooms and calm interiors.
    TatamiMat = 42,
    /// Cyan transparent neon glass for sci-fi windows and railings.
    NeonGlass = 43,
    /// Warm emissive lantern block for zen streets and interiors.
    ShojiLamp = 44,
}

/// Voxel ids for the three mineable neon resources (HUD + telemetry).
pub const VOXEL_LUMINITE: Voxel = BlockType::LuminiteCrystal as Voxel;
pub const VOXEL_MAGNETITE: Voxel = BlockType::MagnetiteOre as Voxel;
pub const VOXEL_IRIDIUM: Voxel = BlockType::IridiumVein as Voxel;

impl BlockType {
    #[inline]
    pub fn is_solid(self) -> bool {
        !matches!(
            self,
            BlockType::Air
                | BlockType::Water
                | BlockType::Lava
                | BlockType::Leaves
                | BlockType::JungleLeaves
                | BlockType::BlossomLeaves
                | BlockType::SakuraPetals
        )
    }

    #[inline]
    pub fn is_opaque(self) -> bool {
        !matches!(
            self,
            BlockType::Air
                | BlockType::Water
                | BlockType::Leaves
                | BlockType::JungleLeaves
                | BlockType::BlossomLeaves
                | BlockType::SakuraPetals
                | BlockType::Ice
                | BlockType::CockpitGlass
                | BlockType::ShojiPaper
                | BlockType::NeonGlass
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
                | BlockType::NeonCyan
                | BlockType::NeonMagenta
                | BlockType::NeonAmber
                | BlockType::EngineCore
                | BlockType::LuminiteCrystal
                | BlockType::MagnetiteOre
                | BlockType::IridiumVein
                | BlockType::NeonGlass
                | BlockType::ShojiLamp
        )
    }

    /// Vertex colour (sRGB). Real texture atlas will replace this later.
    pub fn color(self) -> Color {
        match self {
            BlockType::Air => Color::NONE,
            BlockType::Stone => Color::srgb(0.42, 0.43, 0.42),
            BlockType::Dirt => Color::srgb(0.38, 0.25, 0.16),
            // Warm meadow green keeps the ground distinct from the cooler
            // tree canopy. The previous almost-pure green collapsed a whole
            // forest into one saturated plane at flight distance.
            BlockType::Grass => Color::srgb(0.32, 0.42, 0.24),
            BlockType::Sand => Color::srgb(0.73, 0.65, 0.46),
            // Clear mineral water: saturated enough to read from flight, but
            // no longer clips to electric cyan under the noon key light.
            BlockType::Water => Color::srgba(0.08, 0.50, 0.62, 0.72),
            BlockType::Wood => Color::srgb(0.38, 0.26, 0.16),
            BlockType::Leaves => Color::srgb(0.25, 0.42, 0.27),
            BlockType::Snow => Color::srgb(0.90, 0.92, 0.95),
            BlockType::Ice => Color::srgba(0.70, 0.88, 0.98, 0.85),
            BlockType::TundraGrass => Color::srgb(0.62, 0.76, 0.55),
            BlockType::JungleLeaves => Color::srgb(0.24, 0.40, 0.30),
            BlockType::SavannaGrass => Color::srgb(0.58, 0.54, 0.28),
            BlockType::Gravel => Color::srgb(0.45, 0.44, 0.43),
            BlockType::Bedrock => Color::srgb(0.12, 0.12, 0.14),
            // Sedona red — saturated rust-orange surface dust.
            BlockType::RedSand => Color::srgb(0.92, 0.46, 0.24),
            // Brick-red sandstone cliff body.
            BlockType::RedStone => Color::srgb(0.76, 0.32, 0.20),
            // Pale yellow mesa cap, the bright stripe between reds.
            BlockType::MesaClay => Color::srgb(0.94, 0.76, 0.48),
            // Mossy limestone is still rock: a muted slate/olive midtone
            // keeps it separate from living foliage at eye level.
            BlockType::MossStone => Color::srgb(0.35, 0.39, 0.31),
            // Warm weathered limestone. The former near-white albedo clipped
            // under the daylight key and turned whole karst valleys into an
            // empty white plane, erasing their shape.
            BlockType::Limestone => Color::srgb(0.59, 0.57, 0.51),
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
            BlockType::BlossomLeaves => Color::srgba(0.88, 0.52, 0.67, 0.90),
            BlockType::ZenStone => Color::srgb(0.68, 0.68, 0.62),
            BlockType::Bamboo => Color::srgb(0.47, 0.68, 0.26),
            BlockType::SakuraPetals => Color::srgba(0.88, 0.48, 0.62, 0.86),
            BlockType::ShojiPaper => Color::srgba(1.00, 0.88, 0.68, 0.70),
            BlockType::RoofTile => Color::srgb(0.10, 0.13, 0.16),
            BlockType::TatamiMat => Color::srgb(0.72, 0.62, 0.34),
            BlockType::NeonGlass => Color::srgba(0.18, 0.92, 1.00, 0.48),
            BlockType::ShojiLamp => Color::srgb(1.00, 0.62, 0.24),
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
            36 => BlockType::BlossomLeaves,
            37 => BlockType::ZenStone,
            38 => BlockType::Bamboo,
            39 => BlockType::SakuraPetals,
            40 => BlockType::ShojiPaper,
            41 => BlockType::RoofTile,
            42 => BlockType::TatamiMat,
            43 => BlockType::NeonGlass,
            44 => BlockType::ShojiLamp,
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

pub const BUILDABLE_BLOCKS: [BlockType; 44] = [
    BlockType::Stone,
    BlockType::Dirt,
    BlockType::Grass,
    BlockType::Sand,
    BlockType::Water,
    BlockType::Wood,
    BlockType::Leaves,
    BlockType::Snow,
    BlockType::Ice,
    BlockType::TundraGrass,
    BlockType::JungleLeaves,
    BlockType::SavannaGrass,
    BlockType::Gravel,
    BlockType::Bedrock,
    BlockType::RedSand,
    BlockType::RedStone,
    BlockType::MesaClay,
    BlockType::MossStone,
    BlockType::Limestone,
    BlockType::Crystal,
    BlockType::Basalt,
    BlockType::Lava,
    BlockType::AlienMoss,
    BlockType::BoneRock,
    BlockType::GlowSand,
    BlockType::ShipHullDark,
    BlockType::ShipHullAlloy,
    BlockType::CockpitGlass,
    BlockType::NeonCyan,
    BlockType::NeonMagenta,
    BlockType::NeonAmber,
    BlockType::EngineCore,
    BlockType::LuminiteCrystal,
    BlockType::MagnetiteOre,
    BlockType::IridiumVein,
    BlockType::BlossomLeaves,
    BlockType::ZenStone,
    BlockType::Bamboo,
    BlockType::SakuraPetals,
    BlockType::ShojiPaper,
    BlockType::RoofTile,
    BlockType::TatamiMat,
    BlockType::NeonGlass,
    BlockType::ShojiLamp,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPaletteEntry {
    pub block: BlockType,
    pub label: &'static str,
    pub role: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPaletteCategory {
    pub label: &'static str,
    pub hint: &'static str,
    pub entries: &'static [BlockPaletteEntry],
}

const ASPHALT_CONCRETE: &[BlockPaletteEntry] = &[
    BlockPaletteEntry {
        block: BlockType::Stone,
        label: "Stone",
        role: "asphalt/concrete body",
    },
    BlockPaletteEntry {
        block: BlockType::Gravel,
        label: "Gravel",
        role: "aggregate edge",
    },
    BlockPaletteEntry {
        block: BlockType::Bedrock,
        label: "Bedrock",
        role: "dark foundation",
    },
    BlockPaletteEntry {
        block: BlockType::ZenStone,
        label: "Zen Stone",
        role: "smooth garden concrete",
    },
];

const BRICK_MASONRY: &[BlockPaletteEntry] = &[
    BlockPaletteEntry {
        block: BlockType::RedStone,
        label: "Red Stone",
        role: "brick wall",
    },
    BlockPaletteEntry {
        block: BlockType::MesaClay,
        label: "Mesa Clay",
        role: "warm stucco stripe",
    },
    BlockPaletteEntry {
        block: BlockType::RedSand,
        label: "Red Sand",
        role: "terracotta dust",
    },
];

const GLASS: &[BlockPaletteEntry] = &[
    BlockPaletteEntry {
        block: BlockType::Ice,
        label: "Ice Glass",
        role: "clear window",
    },
    BlockPaletteEntry {
        block: BlockType::CockpitGlass,
        label: "Cockpit Glass",
        role: "smoked sci-fi pane",
    },
    BlockPaletteEntry {
        block: BlockType::Crystal,
        label: "Crystal",
        role: "cyan translucent feature",
    },
    BlockPaletteEntry {
        block: BlockType::LuminiteCrystal,
        label: "Luminite",
        role: "glowing glass accent",
    },
    BlockPaletteEntry {
        block: BlockType::IridiumVein,
        label: "Iridium",
        role: "violet rare-glass vein",
    },
    BlockPaletteEntry {
        block: BlockType::NeonGlass,
        label: "Neon Glass",
        role: "cyan sci-fi window",
    },
];

const GROUND: &[BlockPaletteEntry] = &[
    BlockPaletteEntry {
        block: BlockType::Dirt,
        label: "Dirt",
        role: "soil base",
    },
    BlockPaletteEntry {
        block: BlockType::Grass,
        label: "Grass",
        role: "lawn",
    },
    BlockPaletteEntry {
        block: BlockType::Sand,
        label: "Sand",
        role: "beach/desert",
    },
    BlockPaletteEntry {
        block: BlockType::TundraGrass,
        label: "Tundra",
        role: "cold ground",
    },
    BlockPaletteEntry {
        block: BlockType::JungleLeaves,
        label: "Jungle",
        role: "dense planting",
    },
    BlockPaletteEntry {
        block: BlockType::BlossomLeaves,
        label: "Blossom",
        role: "sakura canopy",
    },
    BlockPaletteEntry {
        block: BlockType::SakuraPetals,
        label: "Petals",
        role: "sakura ground",
    },
    BlockPaletteEntry {
        block: BlockType::SavannaGrass,
        label: "Savanna",
        role: "dry planting",
    },
    BlockPaletteEntry {
        block: BlockType::AlienMoss,
        label: "Alien Moss",
        role: "biolume ground",
    },
    BlockPaletteEntry {
        block: BlockType::GlowSand,
        label: "Glow Sand",
        role: "lit path sand",
    },
];

const METAL: &[BlockPaletteEntry] = &[
    BlockPaletteEntry {
        block: BlockType::ShipHullDark,
        label: "Dark Hull",
        role: "black metal panel",
    },
    BlockPaletteEntry {
        block: BlockType::ShipHullAlloy,
        label: "Alloy",
        role: "brushed metal",
    },
    BlockPaletteEntry {
        block: BlockType::MagnetiteOre,
        label: "Magnetite",
        role: "copper-orange ore",
    },
    BlockPaletteEntry {
        block: BlockType::EngineCore,
        label: "Engine Core",
        role: "hot machinery",
    },
];

const PLASTER_LIGHT: &[BlockPaletteEntry] = &[
    BlockPaletteEntry {
        block: BlockType::Limestone,
        label: "Limestone",
        role: "clean wall plaster",
    },
    BlockPaletteEntry {
        block: BlockType::BoneRock,
        label: "Bone Rock",
        role: "organic ivory wall",
    },
    BlockPaletteEntry {
        block: BlockType::Snow,
        label: "White",
        role: "bright paint",
    },
    BlockPaletteEntry {
        block: BlockType::ShojiPaper,
        label: "Shoji Paper",
        role: "warm paper wall",
    },
    BlockPaletteEntry {
        block: BlockType::TatamiMat,
        label: "Tatami",
        role: "woven interior floor",
    },
];

const PATTERN_TILE_ROOFING: &[BlockPaletteEntry] = &[
    BlockPaletteEntry {
        block: BlockType::RoofTile,
        label: "Roof Tile",
        role: "charcoal ceramic roof",
    },
    BlockPaletteEntry {
        block: BlockType::Basalt,
        label: "Basalt",
        role: "dark roof tile",
    },
    BlockPaletteEntry {
        block: BlockType::MossStone,
        label: "Moss Stone",
        role: "aged patterned tile",
    },
];

const SOLID_COLORS: &[BlockPaletteEntry] = &[
    BlockPaletteEntry {
        block: BlockType::NeonCyan,
        label: "Neon Cyan",
        role: "cyan color/accent",
    },
    BlockPaletteEntry {
        block: BlockType::NeonMagenta,
        label: "Neon Magenta",
        role: "magenta color/accent",
    },
    BlockPaletteEntry {
        block: BlockType::NeonAmber,
        label: "Neon Amber",
        role: "amber color/accent",
    },
    BlockPaletteEntry {
        block: BlockType::ShojiLamp,
        label: "Lantern",
        role: "warm emissive light",
    },
];

const WOOD_NATURE: &[BlockPaletteEntry] = &[
    BlockPaletteEntry {
        block: BlockType::Wood,
        label: "Wood",
        role: "timber",
    },
    BlockPaletteEntry {
        block: BlockType::Leaves,
        label: "Leaves",
        role: "foliage",
    },
    BlockPaletteEntry {
        block: BlockType::Bamboo,
        label: "Bamboo",
        role: "bamboo posts/plants",
    },
];

const WATER_ENERGY: &[BlockPaletteEntry] = &[
    BlockPaletteEntry {
        block: BlockType::Water,
        label: "Water",
        role: "liquid surface",
    },
    BlockPaletteEntry {
        block: BlockType::Lava,
        label: "Lava",
        role: "hot emissive liquid",
    },
];

const BLOCK_PALETTE_CATALOG: &[BlockPaletteCategory] = &[
    BlockPaletteCategory {
        label: "Asphalt & Concrete",
        hint: "roads, foundations, parking decks",
        entries: ASPHALT_CONCRETE,
    },
    BlockPaletteCategory {
        label: "Brick & Masonry",
        hint: "facades, walls, warm city detail",
        entries: BRICK_MASONRY,
    },
    BlockPaletteCategory {
        label: "Glass",
        hint: "windows, cockpit glass, lit crystalline panels",
        entries: GLASS,
    },
    BlockPaletteCategory {
        label: "Ground",
        hint: "terrain, lawns, gardens, paths",
        entries: GROUND,
    },
    BlockPaletteCategory {
        label: "Metal",
        hint: "spacecraft, machines, sci-fi structure",
        entries: METAL,
    },
    BlockPaletteCategory {
        label: "Plaster & Light",
        hint: "clean walls, white paint, bright interiors",
        entries: PLASTER_LIGHT,
    },
    BlockPaletteCategory {
        label: "Pattern / Tile / Roofing",
        hint: "roofs, aged floors, repeated detail",
        entries: PATTERN_TILE_ROOFING,
    },
    BlockPaletteCategory {
        label: "Solid Colors",
        hint: "signage, trims, workflow color coding",
        entries: SOLID_COLORS,
    },
    BlockPaletteCategory {
        label: "Wood & Nature",
        hint: "houses, gardens, trees",
        entries: WOOD_NATURE,
    },
    BlockPaletteCategory {
        label: "Water & Energy",
        hint: "pools, lava, sci-fi hazards",
        entries: WATER_ENERGY,
    },
];

fn block_palette_entry_count(categories: &[BlockPaletteCategory]) -> usize {
    categories
        .iter()
        .map(|category| category.entries.len())
        .sum()
}

pub fn block_palette_catalog() -> &'static [BlockPaletteCategory] {
    debug_assert_eq!(
        block_palette_entry_count(BLOCK_PALETTE_CATALOG),
        BUILDABLE_BLOCKS.len()
    );
    BLOCK_PALETTE_CATALOG
}

pub fn block_palette_entry(block: BlockType) -> Option<BlockPaletteEntry> {
    block_palette_catalog()
        .iter()
        .flat_map(|category| category.entries.iter().copied())
        .find(|entry| entry.block == block)
}

pub fn block_label(block: BlockType) -> &'static str {
    block_palette_entry(block)
        .map(|entry| entry.label)
        .unwrap_or("Air")
}

/// Fast voxel → solid? (without converting through the enum).
/// Fluid and foliage/detail blocks are non-solid for collision so sakura
/// petals, leaves and future half-height details do not behave like full
/// hard cubes.
#[inline]
pub fn voxel_is_solid(v: Voxel) -> bool {
    !matches!(v, 0 | 5 | 7 | 11 | 22 | 36 | 39)
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
/// blossom leaves (36), sakura petals (39), shoji paper (40), neon glass
/// (43), crystal (20), lava (22), cockpit (28), luminite/iridium glass are
/// non-opaque.
#[inline]
pub fn voxel_is_opaque(v: Voxel) -> bool {
    !matches!(
        v,
        0 | 5 | 7 | 9 | 11 | 20 | 22 | 28 | 33 | 35 | 36 | 39 | 40 | 43
    )
}

/// Fast voxel → is this block bioluminescent? Emissive blocks get
/// HDR-boosted vertex colors that bloom through the world camera's
/// bloom pass, producing a true neon glow (lava rivers, crystal spires,
/// alien moss, glow-sand).
#[inline]
pub fn voxel_is_emissive(v: Voxel) -> bool {
    // Lava=22, Crystal=20, AlienMoss=23, GlowSand=25, neon ores 33–35,
    // NeonGlass=43, ShojiLamp=44, Ice=9.
    // Ice gets a whisper of glow so glacier biomes shimmer at night.
    matches!(v, 20 | 22 | 29 | 30 | 31 | 32 | 33 | 34 | 35 | 43 | 44)
}

#[inline]
pub fn voxel_color(v: Voxel) -> [f32; 4] {
    voxel_color_with_emission_budget(v, EmissionBudget::Balanced)
}

/// Resolve a linear vertex color under an explicit HDR budget.
///
/// Atmospheric color and scene emission are separate concepts. Actual
/// emitters receive their material gain, then one hue-preserving scale clamps
/// both peak channel and Rec.709 luminance.
#[inline]
pub fn voxel_color_with_emission_budget(v: Voxel, emission_budget: EmissionBudget) -> [f32; 4] {
    // Convert the block's designer sRGB colour to linear and then, for
    // emissive blocks, multiply the linear RGB by a generous scalar so
    // values exceed 1.0. With the world camera running HDR + bloom,
    // anything above ~1.0 survives tonemap and blooms — giving lava,
    // crystal, alien moss and glow-sand a true neon halo without any
    // custom shader work.
    let mut c = BlockType::from_voxel(v).color().to_linear().to_f32_array();
    if !voxel_is_emissive(v) {
        return c;
    }
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
        43 => {
            // Neon glass — transparent cyan architecture accent.
            c[0] *= 1.2;
            c[1] *= 3.4;
            c[2] *= 4.2;
        }
        44 => {
            // Shoji lamp — warm lantern glow for streets/interiors.
            c[0] *= 3.4;
            c[1] *= 2.0;
            c[2] *= 0.7;
        }
        _ => {}
    }

    let peak = c[0].max(c[1]).max(c[2]);
    let luminance = c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722;
    let peak_scale = emission_budget.max_peak_channel() / peak.max(f32::EPSILON);
    let luminance_scale = emission_budget.max_luminance() / luminance.max(f32::EPSILON);
    let scale = peak_scale.min(luminance_scale).min(1.0);
    c[0] *= scale;
    c[1] *= scale;
    c[2] *= scale;
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
    fn foliage_and_petal_details_do_not_collide_like_full_cubes() {
        for block in [
            BlockType::Leaves,
            BlockType::JungleLeaves,
            BlockType::BlossomLeaves,
            BlockType::SakuraPetals,
        ] {
            assert!(
                !block.is_solid(),
                "{block:?} should be a soft detail block, not a full collision cube"
            );
            assert!(
                !voxel_is_solid(block.into()),
                "{block:?} should be non-solid in the fast collision path"
            );
            assert!(
                !block.is_opaque(),
                "{block:?} should not occlude a neighbouring natural material like stone"
            );
            assert!(
                !voxel_is_opaque(block.into()),
                "{block:?} should also be non-opaque in the fast meshing path"
            );
        }
    }

    #[test]
    fn natural_palette_keeps_grass_and_foliage_readable() {
        let grass = voxel_color(BlockType::Grass.into());
        let leaves = voxel_color(BlockType::Leaves.into());
        let jungle = voxel_color(BlockType::JungleLeaves.into());

        assert!(
            grass[1] >= 0.13,
            "grass green channel is too dark for scenic worlds"
        );
        assert!(
            leaves[1] >= 0.11,
            "tree leaves should not collapse into black silhouettes"
        );
        assert!(
            jungle[1] >= 0.13,
            "jungle/bonsai leaves need a visible midtone under fog and dusk light"
        );
        let canopy_distance = grass[..3]
            .iter()
            .zip(&leaves[..3])
            .map(|(ground, canopy)| (ground - canopy).powi(2))
            .sum::<f32>();
        assert!(
            canopy_distance > 0.001,
            "grass and tree crowns need separate colour planes at flight distance"
        );
        for (label, color) in [("leaves", leaves), ("jungle", jungle)] {
            let green_dominance = color[1] / (color[0] + color[2]).max(1e-5);
            assert!(
                green_dominance < 2.5,
                "{label} is too spectrally narrow and will read as neon green"
            );
        }
    }

    #[test]
    fn atmospheric_terrain_blocks_do_not_emit_light() {
        for block in [
            BlockType::Water,
            BlockType::Ice,
            BlockType::AlienMoss,
            BlockType::GlowSand,
        ] {
            let voxel = block.into();
            let color = voxel_color_with_emission_budget(voxel, EmissionBudget::Cinematic);
            assert!(!voxel_is_emissive(voxel), "{block:?} must receive AO");
            assert!(
                color[..3].iter().all(|channel| *channel <= 1.0),
                "{block:?} unexpectedly contains HDR terrain color: {color:?}"
            );
        }
    }

    #[test]
    fn emissive_color_is_bounded_and_monotonic_across_profiles() {
        let voxel = BlockType::LuminiteCrystal.into();
        let low = voxel_color_with_emission_budget(voxel, EmissionBudget::Low);
        let balanced = voxel_color_with_emission_budget(voxel, EmissionBudget::Balanced);
        let cinematic = voxel_color_with_emission_budget(voxel, EmissionBudget::Cinematic);

        let luminance = |color: [f32; 4]| color[0] * 0.2126 + color[1] * 0.7152 + color[2] * 0.0722;
        assert!(luminance(low) <= EmissionBudget::Low.max_luminance() + 1e-5);
        assert!(luminance(balanced) <= EmissionBudget::Balanced.max_luminance() + 1e-5);
        assert!(luminance(cinematic) <= EmissionBudget::Cinematic.max_luminance() + 1e-5);
        assert!(luminance(low) <= luminance(balanced));
        assert!(luminance(balanced) <= luminance(cinematic));
    }

    #[test]
    fn shuttle_blocks_map_from_voxel_ids() {
        assert_eq!(BlockType::from_voxel(26), BlockType::ShipHullDark);
        assert_eq!(BlockType::from_voxel(28), BlockType::CockpitGlass);
        assert_eq!(BlockType::from_voxel(32), BlockType::EngineCore);
        assert_eq!(BlockType::from_voxel(33), BlockType::LuminiteCrystal);
        assert_eq!(BlockType::from_voxel(35), BlockType::IridiumVein);
        assert_eq!(BlockType::from_voxel(38), BlockType::Bamboo);
        assert_eq!(BlockType::from_voxel(40), BlockType::ShojiPaper);
        assert_eq!(BlockType::from_voxel(44), BlockType::ShojiLamp);
        assert!(voxel_is_emissive(BlockType::NeonCyan.into()));
        assert!(voxel_is_emissive(BlockType::ShojiLamp.into()));
        assert!(!voxel_is_opaque(BlockType::CockpitGlass.into()));
        assert!(!voxel_is_opaque(BlockType::LuminiteCrystal.into()));
        assert!(!voxel_is_opaque(BlockType::NeonGlass.into()));
        assert!(!voxel_is_opaque(BlockType::ShojiPaper.into()));
        assert!(voxel_is_opaque(BlockType::MagnetiteOre.into()));
        assert!(ore_units_for_mined_voxel(VOXEL_LUMINITE) > 0);
    }

    #[test]
    fn zen_builder_inventory_exposes_plants_and_architecture_blocks() {
        for block in [
            BlockType::ZenStone,
            BlockType::Bamboo,
            BlockType::SakuraPetals,
            BlockType::ShojiPaper,
            BlockType::RoofTile,
            BlockType::TatamiMat,
            BlockType::NeonGlass,
            BlockType::ShojiLamp,
        ] {
            assert!(
                BUILDABLE_BLOCKS.contains(&block),
                "{block:?} should be directly buildable"
            );
            assert!(
                block_palette_entry(block).is_some(),
                "{block:?} should be visible in the material catalog"
            );
        }
    }

    #[test]
    fn material_palette_catalog_covers_every_buildable_block_once() {
        let mut seen = std::collections::BTreeSet::new();
        for category in block_palette_catalog() {
            assert!(
                !category.entries.is_empty(),
                "material category '{}' should expose swatches",
                category.label
            );
            for entry in category.entries {
                assert!(
                    seen.insert(entry.block as Voxel),
                    "block {:?} appears in more than one material category",
                    entry.block
                );
                assert!(!entry.label.is_empty());
                assert!(!entry.role.is_empty());
            }
        }

        assert_eq!(seen.len(), BUILDABLE_BLOCKS.len());
        for block in BUILDABLE_BLOCKS {
            assert!(
                seen.contains(&(block as Voxel)),
                "missing material palette entry for {block:?}"
            );
            assert_eq!(
                block_label(block),
                block_palette_entry(block).unwrap().label
            );
        }
    }
}
