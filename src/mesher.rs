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

use crate::blocks::{
    effective_material_for_voxel, material_is_custom, voxel_color_with_emission_budget,
    voxel_is_emissive, voxel_is_opaque, BlockType, MaterialId, Voxel, AIR, DEFAULT_MATERIAL,
};
use crate::chunk::{ChunkPos, CHUNK_SIZE, CHUNK_SIZE_I};
use crate::horizon::VirtualHorizonField;
use crate::voxel_budget::EmissionBudget;

/// Greedy-mesh a chunk into a Bevy `Mesh`. Positions are in world-space
/// offset so the owning entity can sit at the origin.
#[allow(dead_code)]
pub fn build_mesh<F: Fn(i32, i32, i32) -> Voxel>(pos: ChunkPos, sample: F) -> Mesh {
    build_mesh_ex(pos, sample, true)
}

/// Like `build_mesh` but lets callers disable per-corner ambient-
/// occlusion sampling. Skipping AO is a ~3× mesher speedup because:
///   * no 12 occluder samples per mask cell,
///   * greedy merge combines larger rectangles (uniform AO = fewer
///     seams → fewer quads → fewer triangles).
///
/// Used for distant "LOD" chunks where per-corner shading is invisible
/// anyway (fog, small on-screen pixel footprint).
#[allow(dead_code)]
pub fn build_mesh_ex<F: Fn(i32, i32, i32) -> Voxel>(
    pos: ChunkPos,
    sample: F,
    compute_ao: bool,
) -> Mesh {
    let (ox, oy, oz) = pos.origin();

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
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

                        // Per-corner AO sampling — skipped in LOD mode.
                        // Uniform [3,3,3,3] lets greedy merge combine
                        // flat areas across what would have been AO
                        // boundaries, cutting distant triangle counts by
                        // ~40% on typical terrain.
                        let mut ao = [3u8; 4];
                        if compute_ao {
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
                        }

                        MaskCell {
                            voxel,
                            positive,
                            ao,
                        }
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
                        while ui + w < CHUNK_SIZE && mask[vi * CHUNK_SIZE + ui + w] == Some(current)
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
                            &mut uvs,
                            &mut indices,
                            [ox, oy, oz],
                            axis,
                            u,
                            v,
                            d,
                            ui as i32,
                            vi as i32,
                            w as i32,
                            h as i32,
                            current.voxel,
                            DEFAULT_MATERIAL,
                            current.positive,
                            current.ao,
                            EmissionBudget::Balanced,
                            None,
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
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

#[derive(Default)]
struct MeshBuffers {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl MeshBuffers {
    fn into_mesh(self) -> Mesh {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs);
        mesh.insert_indices(Indices::U32(self.indices));
        mesh
    }

    fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// Greedy-mesh a chunk into one mesh per effective material id. The budget
/// changes shading work and HDR energy, never voxel topology.
pub fn build_mesh_buckets_budgeted<F: Fn(i32, i32, i32) -> (Voxel, MaterialId)>(
    pos: ChunkPos,
    sample: F,
    compute_ao: bool,
    emission_budget: EmissionBudget,
) -> Vec<(MaterialId, Mesh)> {
    build_mesh_buckets_budgeted_with_horizon(pos, sample, compute_ao, emission_budget, None)
}

/// Material-bucketed meshing with an optional macro-scale terrain horizon.
///
/// The horizon only modulates linear vertex colour. It does not change voxel
/// topology, collision, material buckets, UVs, or the local AO contract.
pub fn build_mesh_buckets_budgeted_with_horizon<F: Fn(i32, i32, i32) -> (Voxel, MaterialId)>(
    pos: ChunkPos,
    sample: F,
    compute_ao: bool,
    emission_budget: EmissionBudget,
    horizon: Option<&VirtualHorizonField>,
) -> Vec<(MaterialId, Mesh)> {
    let (ox, oy, oz) = pos.origin();

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct MaskCell {
        voxel: Voxel,
        material: MaterialId,
        positive: bool,
        ao: [u8; 4],
    }

    let mut buckets: std::collections::BTreeMap<MaterialId, MeshBuffers> =
        std::collections::BTreeMap::new();
    let mut mask: Vec<Option<MaskCell>> = vec![None; CHUNK_SIZE * CHUNK_SIZE];

    let is_occluder = |wx: i32, wy: i32, wz: i32| -> bool {
        let (v, _) = sample(wx, wy, wz);
        v != AIR && voxel_is_opaque(v)
    };

    for axis in 0..3 {
        let u = (axis + 1) % 3;
        let v = (axis + 2) % 3;

        for d in 0..=CHUNK_SIZE_I {
            for vi in 0..CHUNK_SIZE_I {
                for ui in 0..CHUNK_SIZE_I {
                    let mut back = [0i32; 3];
                    let mut front = [0i32; 3];
                    back[axis] = d - 1;
                    back[u] = ui;
                    back[v] = vi;
                    front[axis] = d;
                    front[u] = ui;
                    front[v] = vi;

                    let (back_v, back_m_raw) = sample(ox + back[0], oy + back[1], oz + back[2]);
                    let (front_v, front_m_raw) =
                        sample(ox + front[0], oy + front[1], oz + front[2]);
                    let back_m = effective_material_for_voxel(back_v, back_m_raw);
                    let front_m = effective_material_for_voxel(front_v, front_m_raw);

                    let cell = if back_v == front_v && back_m == front_m {
                        None
                    } else if back_v == AIR {
                        Some((front_v, front_m, false))
                    } else if front_v == AIR {
                        Some((back_v, back_m, true))
                    } else {
                        let back_opaque = voxel_is_opaque(back_v);
                        let front_opaque = voxel_is_opaque(front_v);
                        if back_opaque && !front_opaque {
                            Some((back_v, back_m, true))
                        } else if front_opaque && !back_opaque {
                            Some((front_v, front_m, false))
                        } else {
                            None
                        }
                    };

                    let mask_cell = cell.map(|(voxel, material, positive)| {
                        let ao_side = if positive { d } else { d - 1 };
                        let mut ao = [3u8; 4];
                        if compute_ao {
                            for (ci, (du, dv)) in
                                [(0, 0), (1, 0), (1, 1), (0, 1)].iter().enumerate()
                            {
                                let du_off = (*du as i32) * 2 - 1;
                                let dv_off = (*dv as i32) * 2 - 1;

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
                        }

                        MaskCell {
                            voxel,
                            material,
                            positive,
                            ao,
                        }
                    });

                    mask[(vi as usize) * CHUNK_SIZE + ui as usize] = mask_cell;
                }
            }

            for vi in 0..CHUNK_SIZE {
                let mut ui = 0usize;
                while ui < CHUNK_SIZE {
                    let idx = vi * CHUNK_SIZE + ui;
                    if let Some(current) = mask[idx] {
                        let mut w = 1usize;
                        while ui + w < CHUNK_SIZE && mask[vi * CHUNK_SIZE + ui + w] == Some(current)
                        {
                            w += 1;
                        }

                        let mut h = 1usize;
                        'grow_h: while vi + h < CHUNK_SIZE {
                            for k in 0..w {
                                if mask[(vi + h) * CHUNK_SIZE + ui + k] != Some(current) {
                                    break 'grow_h;
                                }
                            }
                            h += 1;
                        }

                        let buf = buckets.entry(current.material).or_default();
                        emit_quad(
                            &mut buf.positions,
                            &mut buf.normals,
                            &mut buf.colors,
                            &mut buf.uvs,
                            &mut buf.indices,
                            [ox, oy, oz],
                            axis,
                            u,
                            v,
                            d,
                            ui as i32,
                            vi as i32,
                            w as i32,
                            h as i32,
                            current.voxel,
                            current.material,
                            current.positive,
                            current.ao,
                            emission_budget,
                            horizon,
                        );

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

    buckets
        .into_iter()
        .filter_map(|(material, buffers)| {
            if buffers.is_empty() {
                None
            } else {
                Some((material, buffers.into_mesh()))
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn emit_quad(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
    world_origin: [i32; 3],
    axis: usize,
    u: usize,
    v: usize,
    d: i32,
    u0: i32,
    v0: i32,
    w: i32,
    h: i32,
    voxel: Voxel,
    material: MaterialId,
    positive: bool,
    ao: [u8; 4],
    emission_budget: EmissionBudget,
    horizon: Option<&VirtualHorizonField>,
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

    // UVs are anchored to world axes instead of restarting at every greedy
    // quad/chunk. Natural terrain uses broader projection so macro detail
    // survives without the obvious one-tile-per-block checker pattern.
    // This changes only vertex data: topology, material buckets and draw-call
    // count remain identical. Custom and authored object materials retain
    // their original one-tile-per-block scale.
    let [uv00, uv10, uv11, uv01] =
        material_world_uv_rect(world_origin, axis, u, v, u0, v0, w, h, voxel, material);
    if positive {
        uvs.extend_from_slice(&[uv00, uv10, uv11, uv01]);
    } else {
        uvs.extend_from_slice(&[uv00, uv01, uv11, uv10]);
    }

    // AO -> brightness multiplier. The floor models indirect bounce instead
    // of crushing creases to black. Foliage receives a softer curve because
    // thin leaves transmit and scatter daylight through the crown.
    // Voxel cliffs expose hundreds of alternating top/side strips. A deep AO
    // floor may look dramatic on one cube, but at landscape scale it turns
    // every one-block terrace into a black contour line. Keep contact depth
    // readable while modelling the strong blue-sky bounce of an outdoor
    // scene. Material texture and the virtual horizon still supply macro
    // depth; this table only prevents creases from collapsing to ink.
    const SOLID_AO_MUL: [f32; 4] = [0.76, 0.84, 0.93, 1.0];
    const FOLIAGE_AO_MUL: [f32; 4] = [0.80, 0.87, 0.94, 1.0];
    let base_color = terrain_vertex_base_color(voxel, material, emission_budget);
    // Emissive blocks (lava, crystal, alien moss, glow-sand, ice) are
    // treated as self-lit and ignore ambient occlusion — darkening them
    // at crevices would kill the glow. Non-emissive built-ins stay neutral at
    // vertex level because their baked texture already contains the designer
    // albedo. Applying the block colour here as well squared that albedo in
    // linear light, crushing forest floors and trunks into saturated shadow.
    let emissive = voxel_is_emissive(voxel);
    let foliage = matches!(
        BlockType::from_voxel(voxel),
        BlockType::Leaves
            | BlockType::JungleLeaves
            | BlockType::BlossomLeaves
            | BlockType::SakuraPetals
    );
    let directional_face_light = match (axis, positive) {
        (1, true) => 1.02,  // top faces catch sky without clipping pale terrain
        (1, false) => 0.78, // undersides retain reflected ground light
        (0, true) => 0.96,
        (0, false) => 0.90,
        (2, true) => 0.94,
        _ => 0.88,
    };
    let face_light = if foliage {
        // Wrapped diffuse is a cheap, temporally stable approximation of
        // leaf transmission. Wind and collision remain independent.
        0.35 + directional_face_light * 0.65
    } else {
        directional_face_light
    };
    let ao_mul = if foliage {
        &FOLIAGE_AO_MUL
    } else {
        &SOLID_AO_MUL
    };
    let tint = |a: u8, local_position: [i32; 3]| -> [f32; 4] {
        let world_position = [
            world_origin[0] + local_position[0],
            world_origin[1] + local_position[1],
            world_origin[2] + local_position[2],
        ];
        let macro_light = if emissive || material_is_custom(material) {
            1.0
        } else {
            horizon.map_or(1.0, |field| field.macro_light_multiplier(world_position))
        };
        let m = if emissive {
            1.0
        } else {
            ao_mul[a as usize] * face_light * macro_light
        };
        let chroma = natural_vertex_chromatic_tint(voxel, material, world_position);
        [
            base_color[0] * m * chroma[0],
            base_color[1] * m * chroma[1],
            base_color[2] * m * chroma[2],
            base_color[3],
        ]
    };
    // Color order must match the position order chosen above.
    let (c_a, c_b, c_c, c_d) = if positive {
        (
            tint(ao[0], p00),
            tint(ao[1], p10),
            tint(ao[2], p11),
            tint(ao[3], p01),
        )
    } else {
        (
            tint(ao[0], p00),
            tint(ao[3], p01),
            tint(ao[2], p11),
            tint(ao[1], p10),
        )
    };

    let mut n = [0.0f32; 3];
    n[axis] = if positive { 1.0 } else { -1.0 };
    for _ in 0..4 {
        normals.push(n);
    }
    colors.extend_from_slice(&[c_a, c_b, c_c, c_d]);

    indices.extend_from_slice(&[
        base_idx,
        base_idx + 1,
        base_idx + 2,
        base_idx,
        base_idx + 2,
        base_idx + 3,
    ]);
}

/// Base vertex tint for a textured terrain material.
///
/// Procedural built-in swatches already encode the block's sRGB albedo. A
/// neutral RGB tint prevents accidental double pigmentation, while keeping
/// transparent voxel opacity in the vertex stream. Emissive vertices retain
/// their bounded HDR gain so the existing bloom budget remains authoritative.
#[inline]
fn terrain_vertex_base_color(
    voxel: Voxel,
    material: MaterialId,
    emission_budget: EmissionBudget,
) -> [f32; 4] {
    if material_is_custom(material) {
        return [1.0, 1.0, 1.0, 1.0];
    }
    if voxel_is_emissive(voxel) {
        return voxel_color_with_emission_budget(voxel, emission_budget);
    }

    // `BlockType::color` is an art-palette display colour, not a measured
    // diffuse reflectance. Mapping it to 55% linear energy keeps pale stone
    // below daylight clipping while preserving hue (unlike multiplying the
    // RGB palette twice, which disproportionately crushed dark channels).
    const BUILTIN_DIFFUSE_REFLECTANCE_GAIN: f32 = 0.55;
    let alpha = BlockType::from_voxel(voxel).color().to_srgba().alpha;
    [
        BUILTIN_DIFFUSE_REFLECTANCE_GAIN,
        BUILTIN_DIFFUSE_REFLECTANCE_GAIN,
        BUILTIN_DIFFUSE_REFLECTANCE_GAIN,
        alpha,
    ]
}

/// Continuous world-space colour variation for natural canopy vertices.
///
/// Sampling the global vertex position (rather than a per-chunk hash) keeps
/// adjoining chunks seam-free. Two broad bands describe stand/crown colour,
/// while one lighter band nudges leaves between warm and cool greens. This is
/// vertex data only: it adds no materials, draw calls, geometry, or simulation
/// state and therefore stays independent from flight and collision physics.
#[inline]
fn natural_vertex_chromatic_tint(
    voxel: Voxel,
    material: MaterialId,
    world_position: [i32; 3],
) -> [f32; 3] {
    if material_is_custom(material)
        || !matches!(
            BlockType::from_voxel(voxel),
            BlockType::Leaves
                | BlockType::JungleLeaves
                | BlockType::BlossomLeaves
                | BlockType::SakuraPetals
        )
    {
        return [1.0; 3];
    }

    let x = world_position[0] as f32;
    let y = world_position[1] as f32;
    let z = world_position[2] as f32;
    let stand = (x * 0.027 + z * 0.041).sin() + (x * 0.019 - z * 0.033).cos();
    let crown = (x * 0.19 + y * 0.13 - z * 0.17).sin();
    let warmth = (x * 0.011 - z * 0.015 + y * 0.007).sin();
    let value = (1.01 + stand * 0.030 + crown * 0.018).clamp(0.91, 1.10);

    [
        value * (1.0 + warmth * 0.060),
        value * (1.0 - warmth * 0.018),
        value * (1.0 - warmth * 0.075),
    ]
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn material_world_uv_rect(
    world_origin: [i32; 3],
    face_axis: usize,
    u_axis: usize,
    v_axis: usize,
    u0: i32,
    v0: i32,
    width: i32,
    height: i32,
    voxel: Voxel,
    material: MaterialId,
) -> [[f32; 2]; 4] {
    let scale = texture_world_scale(voxel, material);
    let vertical_bark = !material_is_custom(material)
        && face_axis == 0
        && matches!(
            BlockType::from_voxel(voxel),
            BlockType::Wood | BlockType::Bamboo
        );
    if vertical_bark {
        // X-facing quads normally map world Y to U. Swap their axes so the
        // same bark texture maps world Y to V on both X- and Z-facing sides.
        world_uv_rect(world_origin, v_axis, u_axis, v0, u0, height, width, scale)
    } else {
        world_uv_rect(world_origin, u_axis, v_axis, u0, v0, width, height, scale)
    }
}

#[inline]
fn texture_world_scale(voxel: Voxel, material: MaterialId) -> f32 {
    if material_is_custom(material) {
        return 1.0;
    }

    match BlockType::from_voxel(voxel) {
        BlockType::Water => 0.125,
        BlockType::Leaves
        | BlockType::JungleLeaves
        | BlockType::BlossomLeaves
        | BlockType::SakuraPetals => 0.75,
        BlockType::Grass
        | BlockType::TundraGrass
        | BlockType::SavannaGrass
        | BlockType::AlienMoss => 0.375,
        BlockType::Stone
        | BlockType::Dirt
        | BlockType::Sand
        | BlockType::Snow
        | BlockType::Gravel
        | BlockType::Bedrock
        | BlockType::RedSand
        | BlockType::RedStone
        | BlockType::MesaClay
        | BlockType::MossStone
        | BlockType::Limestone
        | BlockType::Basalt
        | BlockType::BoneRock
        | BlockType::GlowSand => 0.25,
        _ => 1.0,
    }
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn world_uv_rect(
    world_origin: [i32; 3],
    u_axis: usize,
    v_axis: usize,
    u0: i32,
    v0: i32,
    width: i32,
    height: i32,
    scale: f32,
) -> [[f32; 2]; 4] {
    let u_min = (world_origin[u_axis] + u0) as f32 * scale;
    let v_min = (world_origin[v_axis] + v0) as f32 * scale;
    let u_max = (world_origin[u_axis] + u0 + width) as f32 * scale;
    let v_max = (world_origin[v_axis] + v0 + height) as f32 * scale;
    [
        [u_min, v_min],
        [u_max, v_min],
        [u_max, v_max],
        [u_min, v_max],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::CUSTOM_MATERIAL_BASE;

    fn quad_colors_with_horizon(
        voxel: Voxel,
        material: MaterialId,
        horizon: Option<&VirtualHorizonField>,
    ) -> Vec<[f32; 4]> {
        let mut positions = Vec::new();
        let mut normals = Vec::new();
        let mut colors = Vec::new();
        let mut uvs = Vec::new();
        let mut indices = Vec::new();
        emit_quad(
            &mut positions,
            &mut normals,
            &mut colors,
            &mut uvs,
            &mut indices,
            [0, 0, 0],
            1,
            2,
            0,
            1,
            4,
            4,
            4,
            4,
            voxel,
            material,
            true,
            [3; 4],
            EmissionBudget::Balanced,
            horizon,
        );
        colors
    }

    fn quad_colors_for_face(axis: usize, positive: bool) -> Vec<[f32; 4]> {
        let (u, v) = match axis {
            0 => (1, 2),
            1 => (2, 0),
            _ => (0, 1),
        };
        let mut positions = Vec::new();
        let mut normals = Vec::new();
        let mut colors = Vec::new();
        let mut uvs = Vec::new();
        let mut indices = Vec::new();
        emit_quad(
            &mut positions,
            &mut normals,
            &mut colors,
            &mut uvs,
            &mut indices,
            [0, 0, 0],
            axis,
            u,
            v,
            1,
            4,
            4,
            4,
            4,
            BlockType::Limestone as Voxel,
            BlockType::Limestone as MaterialId,
            positive,
            [3; 4],
            EmissionBudget::Balanced,
            None,
        );
        colors
    }

    #[test]
    fn outdoor_face_fill_prevents_voxel_cliffs_from_becoming_black_contours() {
        let luminance = |colors: &[[f32; 4]]| {
            let color = colors[0];
            color[0] * 0.2126 + color[1] * 0.7152 + color[2] * 0.0722
        };
        let top = luminance(&quad_colors_for_face(1, true));
        let darkest_side = (0..3)
            .flat_map(|axis| [false, true].map(move |positive| (axis, positive)))
            .filter(|&(axis, positive)| axis != 1 || !positive)
            .map(|(axis, positive)| luminance(&quad_colors_for_face(axis, positive)))
            .fold(f32::INFINITY, f32::min);

        assert!(darkest_side < top, "directional form must remain visible");
        assert!(
            darkest_side >= top * 0.75,
            "one-block terraces must retain enough sky fill: side={darkest_side:.3}, top={top:.3}"
        );
    }

    #[test]
    fn virtual_horizon_adds_restrained_linear_macro_depth_without_mutating_overrides() {
        let flat = VirtualHorizonField::build(ChunkPos::new(0, 0, 0), |_, _| 0);
        let enclosed = VirtualHorizonField::build(ChunkPos::new(0, 0, 0), |x, z| {
            if matches!((x, z), (0, 0) | (16, 0) | (0, 16) | (16, 16)) {
                0
            } else {
                64
            }
        });
        let stone = BlockType::Stone as Voxel;
        let stone_material = BlockType::Stone as MaterialId;
        let flat_colors = quad_colors_with_horizon(stone, stone_material, Some(&flat));
        let enclosed_colors = quad_colors_with_horizon(stone, stone_material, Some(&enclosed));
        let luminance = |colors: &[[f32; 4]]| {
            colors
                .iter()
                .map(|color| color[0] * 0.2126 + color[1] * 0.7152 + color[2] * 0.0722)
                .sum::<f32>()
        };
        assert!(luminance(&enclosed_colors) < luminance(&flat_colors));
        assert!(luminance(&enclosed_colors) > luminance(&flat_colors) * 0.80);

        let custom_without = quad_colors_with_horizon(stone, CUSTOM_MATERIAL_BASE, None);
        let custom_with = quad_colors_with_horizon(stone, CUSTOM_MATERIAL_BASE, Some(&enclosed));
        assert_eq!(custom_without, custom_with, "designer overrides stay exact");

        let lava = BlockType::Lava as Voxel;
        let lava_material = BlockType::Lava as MaterialId;
        let emissive_without = quad_colors_with_horizon(lava, lava_material, None);
        let emissive_with = quad_colors_with_horizon(lava, lava_material, Some(&enclosed));
        assert_eq!(
            emissive_without, emissive_with,
            "self-lit voxels stay self-lit"
        );
    }

    #[test]
    fn natural_terrain_uvs_are_coarser_than_authored_materials() {
        let grass = texture_world_scale(BlockType::Grass as Voxel, BlockType::Grass as MaterialId);
        let water = texture_world_scale(BlockType::Water as Voxel, BlockType::Water as MaterialId);
        let leaves =
            texture_world_scale(BlockType::Leaves as Voxel, BlockType::Leaves as MaterialId);
        let roof = texture_world_scale(
            BlockType::RoofTile as Voxel,
            BlockType::RoofTile as MaterialId,
        );
        let custom = texture_world_scale(BlockType::Grass as Voxel, CUSTOM_MATERIAL_BASE);

        assert_eq!(grass, 0.375);
        assert_eq!(water, 0.125);
        assert_eq!(leaves, 0.75);
        assert_eq!(roof, 1.0);
        assert_eq!(custom, 1.0);
    }

    #[test]
    fn textured_non_emissive_materials_use_neutral_reflectance_not_squared_albedo() {
        let grass = terrain_vertex_base_color(
            BlockType::Grass as Voxel,
            BlockType::Grass as MaterialId,
            EmissionBudget::Balanced,
        );
        assert_eq!(grass, [0.55, 0.55, 0.55, 1.0]);

        let water = terrain_vertex_base_color(
            BlockType::Water as Voxel,
            BlockType::Water as MaterialId,
            EmissionBudget::Balanced,
        );
        assert_eq!(&water[..3], &[0.55, 0.55, 0.55]);
        assert!((water[3] - 0.72).abs() < 1e-6);

        assert_eq!(
            terrain_vertex_base_color(
                BlockType::Grass as Voxel,
                CUSTOM_MATERIAL_BASE,
                EmissionBudget::Balanced,
            ),
            [1.0, 1.0, 1.0, 1.0],
        );

        let lava = terrain_vertex_base_color(
            BlockType::Lava as Voxel,
            BlockType::Lava as MaterialId,
            EmissionBudget::Balanced,
        );
        assert_ne!(&lava[..3], &[1.0, 1.0, 1.0]);
    }

    #[test]
    fn canopy_chromatic_variation_is_seam_free_bounded_and_override_safe() {
        let leaf = BlockType::Leaves as Voxel;
        let material = BlockType::Leaves as MaterialId;
        let shared_vertex = [CHUNK_SIZE_I, 17, -9];
        let first = natural_vertex_chromatic_tint(leaf, material, shared_vertex);
        let repeated = natural_vertex_chromatic_tint(leaf, material, shared_vertex);
        assert_eq!(first, repeated, "global vertex samples must replay exactly");
        assert!(first.iter().all(|channel| (0.80..=1.15).contains(channel)));

        let distant = natural_vertex_chromatic_tint(leaf, material, [73, 29, 41]);
        assert_ne!(first, distant, "separate crowns should not share one tint");
        assert_eq!(
            natural_vertex_chromatic_tint(leaf, CUSTOM_MATERIAL_BASE, shared_vertex),
            [1.0; 3],
            "authored material overrides keep their exact designer colour"
        );
        assert_eq!(
            natural_vertex_chromatic_tint(
                BlockType::Stone as Voxel,
                BlockType::Stone as MaterialId,
                shared_vertex,
            ),
            [1.0; 3],
        );
    }

    #[test]
    fn world_anchored_uvs_join_across_greedy_quad_boundaries() {
        let left = world_uv_rect([0, 0, 0], 0, 2, 0, 3, 7, 4, 0.25);
        let right = world_uv_rect([0, 0, 0], 0, 2, 7, 3, 5, 4, 0.25);

        assert_eq!(left[1], right[0]);
        assert_eq!(left[2], right[3]);
    }

    #[test]
    fn world_anchored_uvs_join_across_chunk_boundaries() {
        let left = world_uv_rect([0, 0, 0], 0, 2, CHUNK_SIZE_I - 2, 4, 2, 3, 0.25);
        let right = world_uv_rect([CHUNK_SIZE_I, 0, 0], 0, 2, 0, 4, 3, 3, 0.25);

        assert_eq!(left[1], right[0]);
        assert_eq!(left[2], right[3]);
    }

    #[test]
    fn natural_bark_keeps_world_height_on_the_same_texture_axis() {
        let wood = BlockType::Wood as Voxel;
        let material = BlockType::Wood as MaterialId;
        let x_facing = material_world_uv_rect([0, 0, 0], 0, 1, 2, 4, 6, 3, 2, wood, material);
        let z_facing = material_world_uv_rect([0, 0, 0], 2, 0, 1, 6, 4, 2, 3, wood, material);

        assert_eq!(x_facing, z_facing);
        assert_eq!(x_facing, [[6.0, 4.0], [8.0, 4.0], [8.0, 7.0], [6.0, 7.0]]);

        let authored =
            material_world_uv_rect([0, 0, 0], 0, 1, 2, 4, 6, 3, 2, wood, CUSTOM_MATERIAL_BASE);
        assert_eq!(
            authored,
            [[4.0, 6.0], [7.0, 6.0], [7.0, 8.0], [4.0, 8.0]],
            "custom designer materials keep their original UV contract"
        );
    }
}
