//! Chunk -> Bevy mesh.
//!
//! Port target: `lib/voxel/mesher.ts`. This first version is a naive
//! face-culled mesher (only emit faces between solid and non-solid voxels).
//! The greedy-mesher optimisation from the R93G roadmap will be added next
//! so that mid/far LOD chunks get O(surface) triangles instead of O(volume).

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;

use crate::blocks::{BlockType, AIR};
use crate::chunk::{Chunk, CHUNK_SIZE};

/// Convenience: which voxel lives at (x,y,z) in this chunk? Out-of-bounds
/// returns AIR (so the border of a chunk currently meshes as if adjacent
/// chunks were empty; the world module will later patch neighbour access).
#[inline]
fn voxel_at(chunk: &Chunk, x: i32, y: i32, z: i32) -> u16 {
    let s = CHUNK_SIZE as i32;
    if x < 0 || y < 0 || z < 0 || x >= s || y >= s || z >= s {
        return AIR;
    }
    chunk.get(x as usize, y as usize, z as usize)
}

#[inline]
fn is_solid(v: u16) -> bool {
    // SAFETY: Voxel ids 0..=Snow are a superset of what terrain emits.
    // For any unknown id we conservatively treat as non-solid so the mesher
    // never leaves hidden inner faces visible. Keep in sync with
    // `BlockType::is_solid`.
    match v {
        0 => false,                         // Air
        5 => false,                         // Water (transparent fluid)
        1 | 2 | 3 | 4 | 6 | 7 | 8 => true,  // Stone/Dirt/Grass/Sand/Wood/Leaves/Snow
        _ => false,
    }
}

fn block_color(v: u16) -> [f32; 4] {
    let bt = match v {
        1 => BlockType::Stone,
        2 => BlockType::Dirt,
        3 => BlockType::Grass,
        4 => BlockType::Sand,
        5 => BlockType::Water,
        6 => BlockType::Wood,
        7 => BlockType::Leaves,
        8 => BlockType::Snow,
        _ => BlockType::Air,
    };
    bt.color().to_linear().to_f32_array()
}

/// Build a Bevy `Mesh` for this chunk. Positions are chunk-local; the world
/// module places the entity at `chunk.pos * CHUNK_SIZE`.
pub fn build_mesh(chunk: &Chunk) -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // 6 axis-aligned faces: (normal, 4 corner offsets ordered CCW when viewed
    // from outside the block).
    const FACES: [([f32; 3], [[f32; 3]; 4], [i32; 3]); 6] = [
        // +X
        ([1.0, 0.0, 0.0], [[1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [1.0, 1.0, 0.0]], [1, 0, 0]),
        // -X
        ([-1.0, 0.0, 0.0], [[0.0, 0.0, 1.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 1.0, 1.0]], [-1, 0, 0]),
        // +Y
        ([0.0, 1.0, 0.0], [[0.0, 1.0, 1.0], [1.0, 1.0, 1.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]], [0, 1, 0]),
        // -Y
        ([0.0, -1.0, 0.0], [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]], [0, -1, 0]),
        // +Z
        ([0.0, 0.0, 1.0], [[1.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 1.0, 1.0], [1.0, 1.0, 1.0]], [0, 0, 1]),
        // -Z
        ([0.0, 0.0, -1.0], [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]], [0, 0, -1]),
    ];

    let s = CHUNK_SIZE as i32;
    for y in 0..s {
        for z in 0..s {
            for x in 0..s {
                let v = voxel_at(chunk, x, y, z);
                if !is_solid(v) {
                    continue;
                }
                let col = block_color(v);

                for (normal, corners, offset) in FACES.iter() {
                    let nx = x + offset[0];
                    let ny = y + offset[1];
                    let nz = z + offset[2];
                    if is_solid(voxel_at(chunk, nx, ny, nz)) {
                        continue; // face hidden by neighbour
                    }

                    let base = positions.len() as u32;
                    for corner in corners {
                        positions.push([
                            x as f32 + corner[0],
                            y as f32 + corner[1],
                            z as f32 + corner[2],
                        ]);
                        normals.push(*normal);
                        colors.push(col);
                    }
                    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
                }
            }
        }
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}
