//! Block types + palette.
//!
//! Ported concept from `lib/voxel/blocks.ts` in R93G. Each block is a small
//! integer; properties (solid? transparent? color/texture) live in a lookup
//! table so the inner voxel loop stays branch-free.

use bevy::prelude::*;

/// Packed voxel value. `0` = air; everything else is a block id.
/// We use `u16` so we have plenty of room for future blocks (stairs,
/// fences, plants, etc.) without another migration later.
pub type Voxel = u16;

pub const AIR: Voxel = 0;

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

impl BlockType {
    #[inline]
    pub fn is_solid(self) -> bool {
        !matches!(self, BlockType::Air | BlockType::Water)
    }

    #[inline]
    pub fn is_transparent(self) -> bool {
        matches!(self, BlockType::Air | BlockType::Water | BlockType::Leaves)
    }

    /// Placeholder flat colour per block until we hook up a texture atlas.
    /// Matches the general palette from R93G's block list.
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
        }
    }
}

impl From<BlockType> for Voxel {
    fn from(b: BlockType) -> Self {
        b as Voxel
    }
}
