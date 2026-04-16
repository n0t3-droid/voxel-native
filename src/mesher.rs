//! Greedy meshing — turn a chunk into the smallest mesh that still draws
//! every visible block face.
//!
//! Port target: `lib/voxel/mesher.ts`.
//!
//! For each of the 3 axes we sweep through slices, build a 2D face-mask,
//! and greedily combine equal adjacent mask cells into rectangles. The
//! result is one quad per rectangle, instead of one quad per visible face.
//! On flat terrain this produces orders of magnitude fewer triangles than
//! the naive face-culled mesher (a 16×16 grass slab becomes 2 triangles).
//!
//! The sampler callback lets the world module answer "what voxel is at
//! world coord (wx,wy,wz)?" — that way border faces are culled correctly
//! against neighbouring chunks, without the mesher knowing how chunks are
//! stored.

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;

use crate::blocks::{voxel_color, voxel_is_opaque, Voxel, AIR};
use crate::chunk::{ChunkPos, CHUNK_SIZE, CHUNK_SIZE_I};

/// Greedy-mesh a chunk into a Bevy `Mesh`. Positions are in world-space
/// offset so the owning entity can sit at the origin.
pub fn build_mesh<F: Fn(i32, i32, i32) -> Voxel>(pos: ChunkPos, sample: F) -> Mesh {
    let (ox, oy, oz) = pos.origin();

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Mask entries — `None` means no face at this cell. The bool encodes
    // whether the face points along the positive axis direction.
    // `ao` holds per-corner ambient-occlusion values (0=darkest, 3=full
    // light). Two cells only merge if ALL four AO corners match, which
    // preserves ambient shading at creases while still letting wide flat
    // surfaces collapse to a single quad.
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct MaskCell {
        voxel: Voxel,
        positive: bool,
        ao: [u8; 4],
    }

    let mut mask: Vec<Option<MaskCell>> = vec![None; CHUNK_SIZE * CHUNK_SIZE];

    // AO occluder test: "does this voxel block light from a corner?"
    let is_occluder = |wx: i32, wy: i32, wz: i32| -> bool {
        let v = sample(wx, wy, wz);
        v != AIR && voxel_is_opaque(v)
    };

    // For each axis d in {0=X, 1=Y, 2=Z}: u = (d+1)%3, v = (d+2)%3.
    for axis in 0..3 {
        let u = (axis + 1) % 3;
        let v = (axis + 2) % 3;

        // Slice index along `axis`: 0..=CHUNK_SIZE (inclusive = one extra
        // slice at the far side of the chunk).
        for d in 0..=CHUNK_SIZE_I {
            // Build the mask for this slice.
            for vi in 0..CHUNK_SIZE_I {
                for ui in 0..CHUNK_SIZE_I {
                    // Build world coords of "back" (d-1) and "front" (d) cells.
                    let mut back = [0i32; 3];
                    let mut front = [0i32; 3];
                    back[axis] = d - 1;
                    back[u] = ui;
                    back[v] = vi;
                    front[axis] = d;
                    front[u] = ui;
                    front[v] = vi;

                    let back_v = sample(ox + back[0], oy + back[1], oz + back[2]);
                    let front_v = sample(ox + front[0], oy + front[1], oz + front[2]);

                    // Face generation rule (handles transparent blocks
                    // like water + leaves correctly):
                    //   - same voxel on both sides: no face (interior).
                    //   - one side AIR, other side anything: draw the
                    //     non-air voxel's face (so water → air gives a
                    //     water surface; stone → air gives stone).
                    //   - both non-air different voxels: draw face of
                    //     the more-opaque side toward the less-opaque
                    //     side (stone-under-water shows stone). If both
                    //     are equally (non-)opaque, no face.
                    let cell = if back_v == front_v {
                        None
                    } else if back_v == AIR {
                        Some((front_v, false))
                    } else if front_v == AIR {
                        Some((back_v, true))
                    } else {
                        let back_opaque = voxel_is_opaque(back_v);
                        let front_opaque = voxel_is_opaque(front_v);
                        if back_opaque && !front_opaque {
                            Some((back_v, true))
                        } else if front_opaque && !back_opaque {
                            Some((front_v, false))
                        } else {
                            None
                        }
                    };

                    let mask_cell = cell.map(|(voxel, positive)| {
                        // `ao_side` = axis-coord of the AIR side of this
                        // face. Light / shading neighbours live in the air
                        // half-space, not inside the solid block.
                        let ao_side = if positive { d } else { d - 1 };

                        // For each of the 4 quad corners (du, dv) ∈ {0,1}²,
                        // sample the 3 voxels diagonally adjacent on the
                        // air side. Classic Minecraft AO formula:
                        //     if both sides solid  -> 0
                        //     else                 -> 3 - (s1 + s2 + corner)
                        let mut ao = [3u8; 4];
                        for (ci, (du, dv)) in
                            [(0, 0), (1, 0), (1, 1), (0, 1)].iter().enumerate()
                        {
                            let du_off = (*du as i32) * 2 - 1;
                            let dv_off = (*dv as i32) * 2 - 1;

                            // Build world coords for side1, side2, corner.
                            let mut s1 = [0i32; 3];
                            let mut s2 = [0i32; 3];
                            let mut co = [0i32; 3];
                            s1[axis] = ao_side;
                            s1[u] = ui + du_off;
                            s1[v] = vi;
                            s2[axis] = ao_side;
                            s2[u] = ui;
                            s2[v] = vi + dv_off;
                            co[axis] = ao_side;
                            co[u] = ui + du_off;
                            co[v] = vi + dv_off;

                            let s1o = is_occluder(ox + s1[0], oy + s1[1], oz + s1[2]);
                            let s2o = is_occluder(ox + s2[0], oy + s2[1], oz + s2[2]);
                            let coo = is_occluder(ox + co[0], oy + co[1], oz + co[2]);

                            ao[ci] = if s1o && s2o {
                                0
                            } else {
                                3 - (s1o as u8 + s2o as u8 + coo as u8)
                            };
                        }

                        MaskCell { voxel, positive, ao }
                    });

                    mask[(vi as usize) * CHUNK_SIZE + ui as usize] = mask_cell;
                }
            }

            // Greedy-merge the mask into rectangles.
            for vi in 0..CHUNK_SIZE {
                let mut ui = 0usize;
                while ui < CHUNK_SIZE {
                    let idx = vi * CHUNK_SIZE + ui;
                    if let Some(current) = mask[idx] {
                        // Width along u: keep extending while cells match.
                        let mut w = 1usize;
                        while ui + w < CHUNK_SIZE
                            && mask[vi * CHUNK_SIZE + ui + w] == Some(current)
                        {
                            w += 1;
                        }

                        // Height along v: extend whole rows at a time.
                        let mut h = 1usize;
                        'grow_h: while vi + h < CHUNK_SIZE {
                            for k in 0..w {
                                if mask[(vi + h) * CHUNK_SIZE + ui + k] != Some(current) {
                                    break 'grow_h;
                                }
                            }
                            h += 1;
                        }

                        emit_quad(
                            &mut positions,
                            &mut normals,
                            &mut colors,
                            &mut indices,
                            axis,
                            u,
                            v,
                            d,
                            ui as i32,
                            vi as i32,
                            w as i32,
                            h as i32,
                            current.voxel,
                            current.positive,
                            current.ao,
                        );

                        // Clear the consumed rectangle.
                        for dv in 0..h {
                            for du in 0..w {
                                mask[(vi + dv) * CHUNK_SIZE + ui + du] = None;
                            }
                        }
                        ui += w;
                    } else {
                        ui += 1;
                    }
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

#[allow(clippy::too_many_arguments)]
fn emit_quad(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    indices: &mut Vec<u32>,
    axis: usize,
    u: usize,
    v: usize,
    d: i32,
    u0: i32,
    v0: i32,
    w: i32,
    h: i32,
    voxel: Voxel,
    positive: bool,
    ao: [u8; 4],
) {
    // Four corners of the quad in chunk-local coordinates.
    let mut p00 = [0i32; 3];
    let mut p10 = [0i32; 3];
    let mut p11 = [0i32; 3];
    let mut p01 = [0i32; 3];

    p00[axis] = d;
    p00[u] = u0;
    p00[v] = v0;

    p10[axis] = d;
    p10[u] = u0 + w;
    p10[v] = v0;

    p11[axis] = d;
    p11[u] = u0 + w;
    p11[v] = v0 + h;

    p01[axis] = d;
    p01[u] = u0;
    p01[v] = v0 + h;

    let mut n = [0.0f32; 3];
    n[axis] = if positive { 1.0 } else { -1.0 };

    let base_idx = positions.len() as u32;

    // Winding order depends on face direction so the front face points
    // outwards (Bevy uses CCW-front by default).
    let p00f = [p00[0] as f32, p00[1] as f32, p00[2] as f32];
    let p10f = [p10[0] as f32, p10[1] as f32, p10[2] as f32];
    let p11f = [p11[0] as f32, p11[1] as f32, p11[2] as f32];
    let p01f = [p01[0] as f32, p01[1] as f32, p01[2] as f32];

    if positive {
        positions.extend_from_slice(&[p00f, p10f, p11f, p01f]);
    } else {
        positions.extend_from_slice(&[p00f, p01f, p11f, p10f]);
    }

    // AO -> brightness multiplier. 0 (deeply occluded) → dim; 3 (open
    // air) → full colour. The curve is tuned so corners look chunky but
    // not black.
    const AO_MUL: [f32; 4] = [0.55, 0.72, 0.87, 1.00];
    let base_color = voxel_color(voxel);
    let tint = |a: u8| -> [f32; 4] {
        let m = AO_MUL[a as usize];
        [
            base_color[0] * m,
            base_color[1] * m,
            base_color[2] * m,
            base_color[3],
        ]
    };
    // Color order must match the position order chosen above.
    let (c_a, c_b, c_c, c_d) = if positive {
        (tint(ao[0]), tint(ao[1]), tint(ao[2]), tint(ao[3]))
    } else {
        (tint(ao[0]), tint(ao[3]), tint(ao[2]), tint(ao[1]))
    };

    let mut n = [0.0f32; 3];
    n[axis] = if positive { 1.0 } else { -1.0 };
    for _ in 0..4 {
        normals.push(n);
    }
    colors.extend_from_slice(&[c_a, c_b, c_c, c_d]);

    indices.extend_from_slice(&[base_idx, base_idx + 1, base_idx + 2, base_idx, base_idx + 2, base_idx + 3]);
}
