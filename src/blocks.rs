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
}

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
        )
    }

    /// Vertex colour (sRGB). Real texture atlas will replace this later.
    pub fn color(self) -> Color {
        match self {
            BlockType::Air => Color::NONE,
            BlockType::Stone => Color::srgb(0.55, 0.55, 0.55),
            BlockType::Dirt => Color::srgb(0.45, 0.30, 0.18),
            BlockType::Grass => Color::srgb(0.32, 0.68, 0.28),
            BlockType::Sand => Color::srgb(0.93, 0.86, 0.60),
            BlockType::Water => Color::srgba(0.05, 0.45, 0.80, 0.55),
            BlockType::Wood => Color::srgb(0.42, 0.27, 0.13),
            BlockType::Leaves => Color::srgb(0.20, 0.55, 0.18),
            BlockType::Snow => Color::srgb(0.96, 0.97, 0.99),
            BlockType::Ice => Color::srgba(0.70, 0.88, 0.98, 0.85),
            BlockType::TundraGrass => Color::srgb(0.58, 0.70, 0.52),
            BlockType::JungleLeaves => Color::srgb(0.12, 0.52, 0.16),
            BlockType::SavannaGrass => Color::srgb(0.68, 0.74, 0.32),
            BlockType::Gravel => Color::srgb(0.62, 0.60, 0.55),
            BlockType::Bedrock => Color::srgb(0.12, 0.12, 0.14),
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
#[inline]
pub fn voxel_is_solid(v: Voxel) -> bool {
    !matches!(v, 0 | 5)
}

/// Fast voxel → opaque? (used for face-culling).
#[inline]
pub fn voxel_is_opaque(v: Voxel) -> bool {
    !matches!(v, 0 | 5 | 7 | 9 | 11)
}

#[inline]
pub fn voxel_color(v: Voxel) -> [f32; 4] {
    BlockType::from_voxel(v).color().to_linear().to_f32_array()
}
