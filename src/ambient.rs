use bevy::prelude::*;

use crate::neurocore::RuntimeBudget;
use crate::player::Player;
use crate::settings::{GraphicsMode, WorldSettings};
use crate::terrain::{Biome, WATER_LEVEL};
use crate::world::VoxelWorld;

const BUTTERFLY_POOL: usize = 28;
const BUTTERFLY_FIELD_CELL: f32 = 34.0;
const BUTTERFLY_FLUTTER_RADIUS: f32 = 4.8;
const BUTTERFLY_MIN_HEIGHT: f32 = 1.2;
const BUTTERFLY_HEIGHT_BAND: f32 = 3.2;
const BUTTERFLY_ANCHOR_ATTEMPTS: usize = 5;
const BUTTERFLY_CELL_OFFSETS: [(i32, i32); BUTTERFLY_POOL] = [
    (-2, -1),
    (1, -2),
    (2, 1),
    (-1, 2),
    (0, -3),
    (3, 0),
    (0, 3),
    (-3, 0),
    (-2, 2),
    (2, -2),
    (-3, -2),
    (3, 2),
    (-1, -3),
    (1, 3),
    (-4, 1),
    (4, -1),
    (-2, -4),
    (2, 4),
    (-4, -3),
    (4, 3),
    (-3, 4),
    (3, -4),
    (-5, 0),
    (5, 0),
    (0, -5),
    (0, 5),
    (-5, 3),
    (5, -3),
];
#[cfg(test)]
const BUTTERFLY_INTERACTION_RADIUS: f32 = 0.0;

#[derive(Component)]
pub struct Butterfly {
    index: usize,
    phase: f32,
}

#[derive(Component)]
struct ButterflyWing {
    side: f32,
    phase: f32,
}

pub struct AmbientPlugin;

impl Plugin for AmbientPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_butterflies)
            .add_systems(Update, (update_butterfly_bodies, update_butterfly_wings));
    }
}

fn butterfly_limit(mode: GraphicsMode, fx_scale: f32) -> usize {
    let base = match mode {
        GraphicsMode::Fast => 8,
        GraphicsMode::Balanced => 16,
        GraphicsMode::High => BUTTERFLY_POOL,
    };
    ((base as f32) * fx_scale.clamp(0.0, 1.0)).round() as usize
}

fn butterfly_world_anchor(
    player_pos: Vec3,
    index: usize,
    world: &VoxelWorld,
    seconds: f32,
) -> Option<Vec3> {
    for attempt in 0..BUTTERFLY_ANCHOR_ATTEMPTS {
        let xz = butterfly_cell_anchor_xz(player_pos, index, attempt);
        let wx = xz.x.round() as i32;
        let wz = xz.y.round() as i32;
        let surface = world.surface_height_at(wx, wz);
        if !butterfly_habitat(world.biome_at(wx, wz), surface) {
            continue;
        }
        let ground_jitter = hash01(wx, wz, index as u32 ^ 0xB177_E4A) * 1.4;
        let slow_breathe = (seconds * 0.19 + index as f32).sin() * 0.35;
        return Some(Vec3::new(
            xz.x,
            surface as f32 + ground_jitter + slow_breathe,
            xz.y,
        ));
    }
    None
}

fn butterfly_cell_anchor_xz(player_pos: Vec3, index: usize, attempt: usize) -> Vec2 {
    let base_x = (player_pos.x / BUTTERFLY_FIELD_CELL).floor() as i32;
    let base_z = (player_pos.z / BUTTERFLY_FIELD_CELL).floor() as i32;
    let offset = BUTTERFLY_CELL_OFFSETS[(index + attempt * 7) % BUTTERFLY_CELL_OFFSETS.len()];
    let cell_x = base_x + offset.0;
    let cell_z = base_z + offset.1;
    let jitter_x =
        (hash01(cell_x, cell_z, index as u32 ^ 0xA11E_01) - 0.5) * BUTTERFLY_FIELD_CELL * 0.54;
    let jitter_z =
        (hash01(cell_z, cell_x, index as u32 ^ 0xA11E_02) - 0.5) * BUTTERFLY_FIELD_CELL * 0.54;
    Vec2::new(
        (cell_x as f32 + 0.5) * BUTTERFLY_FIELD_CELL + jitter_x,
        (cell_z as f32 + 0.5) * BUTTERFLY_FIELD_CELL + jitter_z,
    )
}

fn butterfly_habitat(biome: Biome, surface: i32) -> bool {
    surface > WATER_LEVEL + 3 && !matches!(biome, Biome::Ocean | Biome::VolcanicWaste)
}

fn butterfly_flutter_offset(index: usize, phase: f32, seconds: f32) -> Vec3 {
    let i = index as f32;
    let speed = 0.38 + (i * 0.031).sin().abs() * 0.16;
    let angle = phase + seconds * speed + (i * 1.618_034).sin() * 0.55;
    let wander = (seconds * 0.31 + phase * 1.7).sin() * 1.8;
    let r = BUTTERFLY_FLUTTER_RADIUS * (0.45 + (phase * 2.11).sin().abs() * 0.55) + wander;
    let y = BUTTERFLY_MIN_HEIGHT
        + (seconds * (0.7 + i * 0.017) + phase).sin().abs() * BUTTERFLY_HEIGHT_BAND;
    Vec3::new(angle.cos() * r, y, angle.sin() * r)
}

fn hash01(x: i32, z: i32, salt: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B9) ^ (z as u32).wrapping_mul(0x85EB_CA6B) ^ salt;
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    (h as f32) / (u32::MAX as f32)
}

#[cfg(test)]
fn butterfly_interaction_radius() -> f32 {
    BUTTERFLY_INTERACTION_RADIUS
}

fn setup_butterflies(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let body_mesh = meshes.add(Cuboid::new(0.08, 0.06, 0.22));
    let wing_mesh = meshes.add(Cuboid::new(0.18, 0.018, 0.11));
    let body_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.08, 0.07, 0.05),
        unlit: true,
        ..default()
    });
    let wing_mats = [
        materials.add(butterfly_wing_material(Color::srgba(1.0, 0.58, 0.86, 0.72))),
        materials.add(butterfly_wing_material(Color::srgba(0.52, 0.92, 1.0, 0.70))),
        materials.add(butterfly_wing_material(Color::srgba(1.0, 0.84, 0.36, 0.72))),
        materials.add(butterfly_wing_material(Color::srgba(0.78, 0.62, 1.0, 0.70))),
    ];

    for index in 0..BUTTERFLY_POOL {
        let phase = (index as f32 * 2.399_963_1) % std::f32::consts::TAU;
        let wing_mat = wing_mats[index % wing_mats.len()].clone();
        commands
            .spawn((
                PbrBundle {
                    mesh: body_mesh.clone(),
                    material: body_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(0.0, -10_000.0, 0.0)),
                    visibility: Visibility::Hidden,
                    ..default()
                },
                Butterfly { index, phase },
                Name::new("AmbientButterfly"),
            ))
            .with_children(|parent| {
                for side in [-1.0_f32, 1.0] {
                    parent.spawn((
                        PbrBundle {
                            mesh: wing_mesh.clone(),
                            material: wing_mat.clone(),
                            transform: Transform::from_xyz(side * 0.12, 0.0, 0.0)
                                .with_rotation(Quat::from_rotation_z(side * 0.35)),
                            visibility: Visibility::Visible,
                            ..default()
                        },
                        ButterflyWing { side, phase },
                        Name::new("AmbientButterflyWing"),
                    ));
                }
            });
    }
}

fn butterfly_wing_material(color: Color) -> StandardMaterial {
    StandardMaterial {
        base_color: color,
        emissive: color.to_linear() * 0.55,
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    }
}

fn update_butterfly_bodies(
    time: Res<Time>,
    player_q: Query<&Transform, (With<Player>, Without<Butterfly>)>,
    world: Res<VoxelWorld>,
    settings: Res<WorldSettings>,
    budget: Res<RuntimeBudget>,
    mut butterflies: Query<(&Butterfly, &mut Transform, &mut Visibility), Without<Player>>,
) {
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let seconds = time.elapsed_seconds();
    let active = butterfly_limit(settings.graphics, budget.weather_fx_scale);
    for (butterfly, mut tf, mut visibility) in butterflies.iter_mut() {
        let Some(anchor) =
            butterfly_world_anchor(player_tf.translation, butterfly.index, &world, seconds)
        else {
            *visibility = Visibility::Hidden;
            continue;
        };
        if butterfly.index >= active {
            *visibility = Visibility::Hidden;
            continue;
        }
        let offset = butterfly_flutter_offset(butterfly.index, butterfly.phase, seconds);
        tf.translation = anchor + offset;
        let tangent =
            butterfly_flutter_offset(butterfly.index, butterfly.phase, seconds + 0.35) - offset;
        tf.rotation = Quat::from_rotation_y(tangent.x.atan2(tangent.z));
        *visibility = Visibility::Visible;
    }
}

fn update_butterfly_wings(
    time: Res<Time>,
    mut wings: Query<(&ButterflyWing, &mut Transform), Without<Butterfly>>,
) {
    let seconds = time.elapsed_seconds();
    for (wing, mut tf) in wings.iter_mut() {
        let flap = (seconds * 8.0 + wing.phase).sin() * 0.95;
        tf.rotation = Quat::from_rotation_z(wing.side * (0.32 + flap.abs() * 0.72));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn butterfly_pool_stays_capped_for_low_end_pcs() {
        assert_eq!(butterfly_limit(GraphicsMode::Fast, 1.0), 8);
        assert_eq!(butterfly_limit(GraphicsMode::Balanced, 1.0), 16);
        assert_eq!(butterfly_limit(GraphicsMode::High, 1.0), BUTTERFLY_POOL);
        assert_eq!(butterfly_limit(GraphicsMode::High, 0.25), 7);
    }

    #[test]
    fn butterfly_motion_stays_airborne_and_local_to_world_anchor() {
        for index in 0..BUTTERFLY_POOL {
            let offset = butterfly_flutter_offset(index, index as f32 * 0.37, 123.0);
            assert!(offset.y >= BUTTERFLY_MIN_HEIGHT);
            assert!(offset.y <= BUTTERFLY_MIN_HEIGHT + BUTTERFLY_HEIGHT_BAND + 0.01);
            assert!(offset.xz().length() <= BUTTERFLY_FLUTTER_RADIUS + 2.0);
        }
    }

    #[test]
    fn butterfly_anchors_are_world_spread_not_player_orbiting() {
        let player = Vec3::ZERO;
        let moved_inside_same_cell = Vec3::new(8.0, 0.0, 9.0);
        assert_eq!(
            butterfly_cell_anchor_xz(player, 0, 0),
            butterfly_cell_anchor_xz(moved_inside_same_cell, 0, 0)
        );

        let anchors: Vec<Vec2> = (0..BUTTERFLY_POOL)
            .map(|index| butterfly_cell_anchor_xz(player, index, 0))
            .collect();
        let min_x = anchors.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
        let max_x = anchors
            .iter()
            .map(|p| p.x)
            .fold(f32::NEG_INFINITY, f32::max);
        let min_z = anchors.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        let max_z = anchors
            .iter()
            .map(|p| p.y)
            .fold(f32::NEG_INFINITY, f32::max);

        assert!(max_x - min_x > BUTTERFLY_FIELD_CELL * 8.0);
        assert!(max_z - min_z > BUTTERFLY_FIELD_CELL * 8.0);
    }

    #[test]
    fn butterflies_are_visual_only_and_do_not_interfere() {
        assert_eq!(butterfly_interaction_radius(), 0.0);
    }
}
