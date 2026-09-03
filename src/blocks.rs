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
    /// Magenta bloom crystal — the hero clusters that grow out of
    /// canyon walls and sky-island keels. Translucent so the spikes
    /// bleed light into each other.
    CrystalMagenta = 36,
    /// Verdant bloom crystal — the green third of the crystal triad.
    CrystalVerdant = 37,
    /// Electric-blue plasma coolant — flows in canyon channels the way
    /// water pools in rivers. Non-solid, non-opaque, strongly emissive.
    PlasmaFlow = 38,
    /// Skyway deck plating — dark structural alloy for elevated roads,
    /// monorail bridges and station decks.
    SkywayDeck = 39,
}

/// Voxel ids for the three mineable neon resources (HUD + telemetry).
pub const VOXEL_LUMINITE: Voxel = BlockType::LuminiteCrystal as Voxel;
pub const VOXEL_MAGNETITE: Voxel = BlockType::MagnetiteOre as Voxel;
pub const VOXEL_IRIDIUM: Voxel = BlockType::IridiumVein as Voxel;

/// Voxel ids the Aether Frontier overlay generates. Kept together so the
/// frontier generator, HUD readouts and regression tests all agree on
/// which blocks belong to the overlay.
pub const VOXEL_CRYSTAL_MAGENTA: Voxel = BlockType::CrystalMagenta as Voxel;
pub const VOXEL_CRYSTAL_VERDANT: Voxel = BlockType::CrystalVerdant as Voxel;
pub const VOXEL_PLASMA_FLOW: Voxel = BlockType::PlasmaFlow as Voxel;
pub const VOXEL_SKYWAY_DECK: Voxel = BlockType::SkywayDeck as Voxel;

impl BlockType {
    #[inline]
    pub fn is_solid(self) -> bool {
        !matches!(
            self,
            BlockType::Air | BlockType::Water | BlockType::PlasmaFlow
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
                | BlockType::Ice
                | BlockType::CockpitGlass
                | BlockType::CrystalMagenta
                | BlockType::CrystalVerdant
                | BlockType::PlasmaFlow
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
                | BlockType::CrystalMagenta
                | BlockType::CrystalVerdant
                | BlockType::PlasmaFlow
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
            // Crystal triad (sRGB IEC 61966-2-1, D65). The three hues are
            // spaced ~120° apart in sRGB hue so magenta / cyan / green
            // clusters stay separable at long range and under bloom.
            // Magenta #E01FC6, verdant #2EE86B; the cyan third of the
            // triad is the existing `Crystal` / `LuminiteCrystal` pair.
            BlockType::CrystalMagenta => Color::srgba(0.878, 0.122, 0.776, 0.70),
            BlockType::CrystalVerdant => Color::srgba(0.180, 0.910, 0.420, 0.70),
            // Plasma coolant #1C8CFF — an electric blue well inside the
            // sRGB gamut so it survives ACES tonemapping without clipping
            // to white the way a pure-primary blue does.
            BlockType::PlasmaFlow => Color::srgba(0.110, 0.549, 1.000, 0.84),
            // Structural deck alloy #2A3140 — dark but never crushed to
            // black, so skyway rails and guard edges stay readable when
            // the deck is silhouetted against a bright nebula.
            BlockType::SkywayDeck => Color::srgb(0.165, 0.192, 0.251),
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
            36 => BlockType::CrystalMagenta,
            37 => BlockType::CrystalVerdant,
            38 => BlockType::PlasmaFlow,
            39 => BlockType::SkywayDeck,
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

pub const BUILDABLE_BLOCKS: [BlockType; 39] = [
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
    BlockType::CrystalMagenta,
    BlockType::CrystalVerdant,
    BlockType::PlasmaFlow,
    BlockType::SkywayDeck,
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
        block: BlockType::CrystalMagenta,
        label: "Magenta Bloom",
        role: "magenta crystal cluster",
    },
    BlockPaletteEntry {
        block: BlockType::CrystalVerdant,
        label: "Verdant Bloom",
        role: "green crystal cluster",
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
    BlockPaletteEntry {
        block: BlockType::SkywayDeck,
        label: "Skyway Deck",
        role: "elevated road plating",
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
];

const PATTERN_TILE_ROOFING: &[BlockPaletteEntry] = &[
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
    BlockPaletteEntry {
        block: BlockType::PlasmaFlow,
        label: "Plasma Flow",
        role: "cold emissive energy river",
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
/// AIR (0), Water (5), Lava (22) and PlasmaFlow (38) are non-solid for
/// collision — plasma channels are swimmable/hazard volumes, not walls.
#[inline]
pub fn voxel_is_solid(v: Voxel) -> bool {
    !matches!(v, 0 | 5 | 22 | 38)
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
/// crystal (20), lava (22), cockpit (28), luminite/iridium glass,
/// the magenta/verdant bloom crystals (36, 37) and plasma (38) are
/// non-opaque.
#[inline]
pub fn voxel_is_opaque(v: Voxel) -> bool {
    !matches!(
        v,
        0 | 5 | 7 | 9 | 11 | 20 | 22 | 28 | 33 | 35 | 36 | 37 | 38
    )
}

/// Fast voxel → is this block bioluminescent? Emissive blocks get
/// HDR-boosted vertex colors that bloom through the world camera's
/// bloom pass, producing a true neon glow (lava rivers, crystal spires,
/// alien moss, glow-sand).
#[inline]
pub fn voxel_is_emissive(v: Voxel) -> bool {
    // Lava=22, Crystal=20, AlienMoss=23, GlowSand=25, neon ores 33–35, Ice=9,
    // bloom crystals 36–37, plasma 38.
    // Ice gets a whisper of glow so glacier biomes shimmer at night.
    matches!(
        v,
        9 | 20 | 22 | 23 | 25 | 29 | 30 | 31 | 32 | 33 | 34 | 35 | 36 | 37 | 38
    )
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
        36 => {
            // Magenta bloom crystal — hot pink core, no green leak.
            c[0] *= 3.4;
            c[1] *= 3.0;
            c[2] *= 3.8;
        }
        37 => {
            // Verdant bloom crystal — the calmest of the triad so green
            // clusters read as accents next to magenta hero spikes.
            c[0] *= 3.0;
            c[1] *= 3.0;
            c[2] *= 3.0;
        }
        38 => {
            // Plasma flow — the green lift pushes the channel core toward
            // white-hot cyan while the banks stay deep electric blue.
            c[0] *= 2.2;
            c[1] *= 5.0;
            c[2] *= 3.6;
        }
        _ => {}
    }
    c
}

/// When a weapon destroys this voxel, how many inventory units drop (concept HUD).
#[inline]
pub fn ore_units_for_mined_voxel(v: Voxel) -> u32 {
    match v {
        VOXEL_LUMINITE | VOXEL_CRYSTAL_MAGENTA | VOXEL_CRYSTAL_VERDANT => 2,
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

    #[test]
    fn aether_frontier_blocks_map_and_glow() {
        assert_eq!(BlockType::from_voxel(36), BlockType::CrystalMagenta);
        assert_eq!(BlockType::from_voxel(37), BlockType::CrystalVerdant);
        assert_eq!(BlockType::from_voxel(38), BlockType::PlasmaFlow);
        assert_eq!(BlockType::from_voxel(39), BlockType::SkywayDeck);
        assert_eq!(VOXEL_CRYSTAL_MAGENTA, 36);
        assert_eq!(VOXEL_CRYSTAL_VERDANT, 37);
        assert_eq!(VOXEL_PLASMA_FLOW, 38);
        assert_eq!(VOXEL_SKYWAY_DECK, 39);

        assert!(!voxel_is_solid(VOXEL_PLASMA_FLOW));
        assert!(!BlockType::PlasmaFlow.is_solid());
        assert!(voxel_is_solid(VOXEL_SKYWAY_DECK));
        assert!(voxel_is_emissive(VOXEL_CRYSTAL_MAGENTA));
        assert!(voxel_is_emissive(VOXEL_CRYSTAL_VERDANT));
        assert!(voxel_is_emissive(VOXEL_PLASMA_FLOW));
        assert!(!voxel_is_emissive(VOXEL_SKYWAY_DECK));
        assert!(!voxel_is_opaque(VOXEL_CRYSTAL_MAGENTA));
        assert!(!voxel_is_opaque(VOXEL_CRYSTAL_VERDANT));
        assert!(!voxel_is_opaque(VOXEL_PLASMA_FLOW));
        assert!(voxel_is_opaque(VOXEL_SKYWAY_DECK));
        assert!(voxel_is_weapon_target(VOXEL_PLASMA_FLOW));
        assert_eq!(ore_units_for_mined_voxel(VOXEL_CRYSTAL_MAGENTA), 2);
        assert_eq!(ore_units_for_mined_voxel(VOXEL_CRYSTAL_VERDANT), 2);
        assert_eq!(ore_units_for_mined_voxel(VOXEL_PLASMA_FLOW), 0);
        assert_eq!(ore_units_for_mined_voxel(VOXEL_SKYWAY_DECK), 0);
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
