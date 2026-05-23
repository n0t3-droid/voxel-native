//! Intent-first building tools.
//!
//! The normal sculpt tools are precise, but they still ask the player to do
//! too much manual labor for city-scale work. Smart Tower is the first
//! "lazy builder" primitive: pick a footprint with two clicks and the engine
//! generates a usable high-rise shell with floors, window rhythm, podium,
//! setbacks, roof crown, and undo support.

use bevy::prelude::*;

use crate::blocks::{BlockType, Voxel, AIR};
use crate::builder::BuilderHistory;
use crate::mode::ModeContext;
use crate::player::Player;
use crate::sculpt::raycast::dda_voxel;
use crate::toolbelt::{ToolbeltState, ToolbeltTool};
use crate::world::{VoxelWorld, WorldEditBatch};

const SMART_REACH: f32 = 180.0;
const SMART_MAX_FOOTPRINT: i32 = 48;
const SMART_MAX_CHANGES: usize = 240_000;

#[derive(Resource, Default)]
pub struct SmartTowerState {
    anchor: Option<IVec3>,
    cursor: Option<IVec3>,
    preview: Option<TowerPlan>,
}

#[derive(Debug, Clone, Copy)]
struct TowerPlan {
    min: IVec3,
    max: IVec3,
    base_y: i32,
    floors: i32,
    floor_h: i32,
    height: i32,
}

impl TowerPlan {
    fn width_x(self) -> i32 {
        self.max.x - self.min.x + 1
    }

    fn width_z(self) -> i32 {
        self.max.z - self.min.z + 1
    }

    fn footprint_cells(self) -> i32 {
        self.width_x() * self.width_z()
    }
}

fn smart_tower_active(mode: &ModeContext) -> bool {
    mode.is_build_live() && mode.build_tool() == Some(ToolbeltTool::SmartTower)
}

pub fn smart_tower_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<ModeContext>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut smart: ResMut<SmartTowerState>,
    mut world: ResMut<VoxelWorld>,
    mut history: ResMut<BuilderHistory>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
) {
    if !smart_tower_active(&mode) {
        if smart.anchor.is_some() {
            smart.anchor = None;
            smart.cursor = None;
            smart.preview = None;
        }
        return;
    }

    if keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right) {
        if smart.anchor.take().is_some() {
            smart.cursor = None;
            smart.preview = None;
            toolbelt.status = "Smart Tower cancelled. Click a ground corner to start again.".into();
        }
        return;
    }

    let Ok(cam_tf) = cam_q.get_single() else {
        if mouse.just_pressed(MouseButton::Left) {
            toolbelt.status = "Smart Tower could not find the player camera this frame.".into();
        }
        return;
    };
    let origin = cam_tf.translation();
    let dir = cam_tf.forward().as_vec3();
    let Some(cursor) = pick_ground_cell(&world, origin, dir) else {
        if mouse.just_pressed(MouseButton::Left) {
            toolbelt.status = "Smart Tower needs terrain or a block under the crosshair.".into();
        }
        return;
    };

    if let Some(anchor) = smart.anchor {
        smart.cursor = Some(cursor);
        let plan = make_plan(&world, anchor, cursor);
        smart.preview = Some(plan);
        toolbelt.status = format!(
            "Smart Tower preview: {}x{}, {} floors, ~{} facade cells. Second LMB commits, RMB/Esc cancels.",
            plan.width_x(),
            plan.width_z(),
            plan.floors,
            plan.footprint_cells() * plan.floors
        );
    }

    if mouse.just_pressed(MouseButton::Left) {
        if smart.anchor.is_none() {
            smart.anchor = Some(cursor);
            smart.cursor = Some(cursor);
            smart.preview = Some(make_plan(&world, cursor, cursor));
            toolbelt.status =
                "Smart Tower corner A set. Aim at opposite corner and click again.".into();
            return;
        }

        let Some(anchor) = smart.anchor else {
            return;
        };
        let plan = make_plan(&world, anchor, cursor);
        let (changed, clipped) = stamp_smart_tower(&mut world, &mut history, plan);
        smart.anchor = None;
        smart.cursor = None;
        smart.preview = None;
        toolbelt.status = if changed == 0 {
            "Smart Tower made no changes. Try a larger footprint on terrain.".into()
        } else if clipped {
            format!(
                "Smart Tower committed: {} changes before safety cap. Use Ctrl+Z to undo.",
                changed
            )
        } else {
            format!(
                "Smart Tower committed: {}x{}, {} floors, {} changes. Ctrl+Z undo.",
                plan.width_x(),
                plan.width_z(),
                plan.floors,
                changed
            )
        };
    }
}

fn pick_ground_cell(world: &VoxelWorld, origin: Vec3, dir: Vec3) -> Option<IVec3> {
    if let Some((hit, prev)) = dda_voxel(world, origin, dir, SMART_REACH) {
        let cell = if prev.y >= hit.y { prev } else { hit };
        let ground = world.surface_height_at(cell.x, cell.z);
        return Some(IVec3::new(cell.x, ground, cell.z));
    }
    let fallback = origin + dir * 32.0;
    if !fallback.is_finite() {
        return None;
    }
    let x = fallback.x.floor() as i32;
    let z = fallback.z.floor() as i32;
    Some(IVec3::new(x, world.surface_height_at(x, z), z))
}

fn make_plan(world: &VoxelWorld, a: IVec3, b: IVec3) -> TowerPlan {
    let mut min = IVec3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z));
    let mut max = IVec3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z));

    if max.x - min.x + 1 > SMART_MAX_FOOTPRINT {
        max.x = min.x + SMART_MAX_FOOTPRINT - 1;
    }
    if max.z - min.z + 1 > SMART_MAX_FOOTPRINT {
        max.z = min.z + SMART_MAX_FOOTPRINT - 1;
    }

    let mut ground = i32::MIN;
    for x in min.x..=max.x {
        for z in min.z..=max.z {
            ground = ground.max(world.surface_height_at(x, z));
        }
    }
    if ground == i32::MIN {
        ground = min.y.max(max.y);
    }
    min.y = ground;
    max.y = ground;

    let width_x = max.x - min.x + 1;
    let width_z = max.z - min.z + 1;
    let longest = width_x.max(width_z).max(4);
    let area = (width_x * width_z).max(1);
    let floors = ((longest + area / 18) / 2).clamp(8, 42);
    let floor_h = 4;
    let height = floors * floor_h + 6;

    TowerPlan {
        min,
        max,
        base_y: ground + 1,
        floors,
        floor_h,
        height,
    }
}

fn stamp_smart_tower(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    plan: TowerPlan,
) -> (usize, bool) {
    let mut batch = WorldEditBatch::default();
    let mut changes: Vec<(IVec3, Voxel, Voxel)> = Vec::new();
    let mut clipped = false;

    let x_mid = (plan.min.x + plan.max.x) / 2;
    let z_mid = (plan.min.z + plan.max.z) / 2;
    let wall: Voxel = BlockType::Limestone.into();
    let mullion: Voxel = BlockType::Stone.into();
    let glass: Voxel = BlockType::Ice.into();
    let floor: Voxel = BlockType::MossStone.into();
    let crown: Voxel = BlockType::Snow.into();
    let accent: Voxel = BlockType::Crystal.into();

    for x in plan.min.x..=plan.max.x {
        for z in plan.min.z..=plan.max.z {
            let ground_top = world.surface_height_at(x, z);
            for y in (plan.base_y)..=(ground_top + 2).max(plan.base_y) {
                push_change(
                    world,
                    &mut batch,
                    &mut changes,
                    IVec3::new(x, y, z),
                    AIR,
                    &mut clipped,
                );
                if clipped {
                    finish_tower(world, history, batch, changes);
                    return (SMART_MAX_CHANGES, true);
                }
            }
        }
    }

    for y_rel in 0..=plan.height {
        let y = plan.base_y + y_rel;
        let floor_idx = (y_rel / plan.floor_h).max(0);
        let setback = if floor_idx > 28 {
            3
        } else if floor_idx > 18 {
            2
        } else if floor_idx > 10 {
            1
        } else {
            0
        };
        let min_x = (plan.min.x + setback).min(x_mid);
        let max_x = (plan.max.x - setback).max(x_mid);
        let min_z = (plan.min.z + setback).min(z_mid);
        let max_z = (plan.max.z - setback).max(z_mid);
        let roof_y = plan.base_y + plan.height;
        let is_floor = y_rel == 0 || y_rel % plan.floor_h == 0;
        let is_roof_band = y >= roof_y - 2;
        let is_sky_lobby = floor_idx > 0 && floor_idx % 8 == 0 && y_rel % plan.floor_h <= 1;

        for x in min_x..=max_x {
            for z in min_z..=max_z {
                let on_x_edge = x == min_x || x == max_x;
                let on_z_edge = z == min_z || z == max_z;
                let on_edge = on_x_edge || on_z_edge;
                let corner = on_x_edge && on_z_edge;
                let inner = !on_edge;

                let v = if is_roof_band {
                    if on_edge || y == roof_y {
                        Some(if corner { accent } else { crown })
                    } else {
                        None
                    }
                } else if is_floor {
                    Some(floor)
                } else if on_edge {
                    let coord = if on_x_edge { z - min_z } else { x - min_x };
                    let vertical = (x - min_x) % 5 == 0 || (z - min_z) % 5 == 0;
                    let horizontal_band = y_rel % plan.floor_h == 1;
                    let window_slot = !corner && coord % 3 != 0 && y_rel % plan.floor_h >= 2;
                    if vertical || horizontal_band {
                        Some(mullion)
                    } else if window_slot || is_sky_lobby {
                        Some(glass)
                    } else {
                        Some(wall)
                    }
                } else if inner && is_sky_lobby {
                    Some(AIR)
                } else {
                    None
                };

                if let Some(v) = v {
                    push_change(
                        world,
                        &mut batch,
                        &mut changes,
                        IVec3::new(x, y, z),
                        v,
                        &mut clipped,
                    );
                    if clipped {
                        finish_tower(world, history, batch, changes);
                        return (SMART_MAX_CHANGES, true);
                    }
                } else if world.is_solid(x, y, z) {
                    push_change(
                        world,
                        &mut batch,
                        &mut changes,
                        IVec3::new(x, y, z),
                        AIR,
                        &mut clipped,
                    );
                    if clipped {
                        finish_tower(world, history, batch, changes);
                        return (SMART_MAX_CHANGES, true);
                    }
                }
            }
        }
    }

    let spire_h = (plan.floors / 3).clamp(4, 18);
    for dy in 1..=spire_h {
        let y = plan.base_y + plan.height + dy;
        for (x, z) in [(x_mid, z_mid), (x_mid + 1, z_mid), (x_mid, z_mid + 1)] {
            push_change(
                world,
                &mut batch,
                &mut changes,
                IVec3::new(x, y, z),
                accent,
                &mut clipped,
            );
            if clipped {
                finish_tower(world, history, batch, changes);
                return (SMART_MAX_CHANGES, true);
            }
        }
    }

    let changed = changes.len();
    finish_tower(world, history, batch, changes);
    (changed, false)
}

fn push_change(
    world: &mut VoxelWorld,
    batch: &mut WorldEditBatch,
    changes: &mut Vec<(IVec3, Voxel, Voxel)>,
    pos: IVec3,
    voxel: Voxel,
    clipped: &mut bool,
) {
    if changes.len() >= SMART_MAX_CHANGES {
        *clipped = true;
        return;
    }
    if let Some((before, after)) = world.edit_set_voxel_batched(pos.x, pos.y, pos.z, voxel, batch) {
        changes.push((pos, before, after));
    }
}

fn finish_tower(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    batch: WorldEditBatch,
    changes: Vec<(IVec3, Voxel, Voxel)>,
) {
    world.finish_edit_batch(batch);
    history.record_external(format!("Smart Tower {} cells", changes.len()), changes);
}

pub fn smart_tower_gizmo(smart: Res<SmartTowerState>, mut gizmos: Gizmos, time: Res<Time>) {
    let Some(plan) = smart.preview else {
        return;
    };
    let pulse = 0.55 + 0.45 * (time.elapsed_seconds() * 5.0).sin().abs();
    let color = Color::srgb(0.25 + 0.25 * pulse, 0.9, 1.0);
    let base_y = plan.base_y as f32;
    let top_y = (plan.base_y + plan.height) as f32;
    let min_x = plan.min.x as f32;
    let max_x = plan.max.x as f32 + 1.0;
    let min_z = plan.min.z as f32;
    let max_z = plan.max.z as f32 + 1.0;

    let base = [
        Vec3::new(min_x, base_y, min_z),
        Vec3::new(max_x, base_y, min_z),
        Vec3::new(max_x, base_y, max_z),
        Vec3::new(min_x, base_y, max_z),
        Vec3::new(min_x, base_y, min_z),
    ];
    let top = [
        Vec3::new(min_x, top_y, min_z),
        Vec3::new(max_x, top_y, min_z),
        Vec3::new(max_x, top_y, max_z),
        Vec3::new(min_x, top_y, max_z),
        Vec3::new(min_x, top_y, min_z),
    ];
    gizmos.linestrip(base, color);
    gizmos.linestrip(top, color.with_alpha(0.75));
    for (x, z) in [
        (min_x, min_z),
        (max_x, min_z),
        (max_x, max_z),
        (min_x, max_z),
    ] {
        gizmos.line(Vec3::new(x, base_y, z), Vec3::new(x, top_y, z), color);
    }
}
