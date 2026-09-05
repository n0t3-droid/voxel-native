use bevy::prelude::*;

use crate::menu::{GameState, PendingWorldLoad};
use crate::neurocore::{RuntimeBudget, RuntimeProfile};
use crate::player::Player;
use crate::settings::{ActiveWorld, GraphicsMode, WorldSettings};
use crate::ships::new_world_look_basis;
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
            .add_systems(OnEnter(GameState::MainMenu), cleanup_colony_life)
            .add_systems(OnEnter(GameState::InGame), spawn_colony_life_once)
            .add_systems(
                Update,
                (
                    update_butterfly_bodies,
                    update_butterfly_wings,
                    update_colony_walkers,
                    update_skyway_trams,
                )
                    .run_if(in_state(GameState::InGame)),
            );
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

#[derive(Component)]
struct ColonyWalker {
    t: f32,
    speed: f32,
    origin: Vec3,
    span: Vec3,
}

#[derive(Component)]
struct SkywayTram {
    t: f32,
    speed: f32,
    origin: Vec3,
    span: Vec3,
}

fn colony_figure_count(graphics: GraphicsMode, cinematic: bool) -> usize {
    match graphics {
        GraphicsMode::Fast => 0,
        GraphicsMode::Balanced => 3,
        GraphicsMode::High if cinematic => 5,
        GraphicsMode::High => 4,
    }
}

fn skyway_tram_count(graphics: GraphicsMode, cinematic: bool) -> usize {
    match graphics {
        GraphicsMode::Fast => 1,
        GraphicsMode::Balanced => 1,
        GraphicsMode::High if cinematic => 2,
        GraphicsMode::High => 1,
    }
}

fn ping_pong(t: f32) -> f32 {
    let u = t.rem_euclid(1.0);
    if u < 0.5 {
        u * 2.0
    } else {
        2.0 - u * 2.0
    }
}

fn spawn_colony_life_once(
    pending: Res<PendingWorldLoad>,
    active: Option<Res<ActiveWorld>>,
    settings: Res<WorldSettings>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, Or<(With<ColonyWalker>, With<SkywayTram>)>>,
) {
    if !pending.0 {
        return;
    }
    for e in existing.iter() {
        if let Some(entity_commands) = commands.get_entity(e) {
            entity_commands.despawn_recursive();
        }
    }
    let Some(active) = active else {
        return;
    };
    let cinematic = settings.runtime_profile == RuntimeProfile::Cinematic;
    let generator = crate::terrain::TerrainGenerator::new(active.meta.seed);
    let mut anchor = Vec3::new(
        active.meta.player_pos[0],
        active.meta.player_pos[1],
        active.meta.player_pos[2],
    );
    if anchor.x.abs() < 280.0 && anchor.z.abs() < 280.0 {
        let (eye, _, _) = generator.scenic_frontier_spawn();
        anchor = Vec3::from(eye);
    }
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    spawn_colony_figures(
        &mut commands,
        &mut materials,
        &cube,
        &generator,
        anchor,
        settings.graphics,
        cinematic,
    );
    spawn_skyway_trams(
        &mut commands,
        &mut materials,
        &cube,
        anchor,
        settings.graphics,
        cinematic,
    );
}

fn spawn_colony_figures(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    cube: &Handle<Mesh>,
    generator: &crate::terrain::TerrainGenerator,
    origin: Vec3,
    graphics: GraphicsMode,
    cinematic: bool,
) {
    let count = colony_figure_count(graphics, cinematic);
    if count == 0 {
        return;
    }
    let (_fwd, fwd_h, right_h) = new_world_look_basis();
    let spots: [(f32, f32, f32, f32); 5] = [
        (22.0, 6.0, 7.0, 0.018),
        (28.0, -4.0, 5.5, 0.022),
        (18.0, 12.0, 6.0, 0.016),
        (32.0, 2.0, 8.0, 0.020),
        (24.0, -10.0, 5.0, 0.024),
    ];
    let suits = [
        Color::srgb(0.18, 0.42, 0.62),
        Color::srgb(0.22, 0.48, 0.28),
        Color::srgb(0.62, 0.32, 0.12),
        Color::srgb(0.72, 0.74, 0.78),
        Color::srgb(0.48, 0.22, 0.55),
    ];
    for (i, &(ahead, lat, walk, speed)) in spots.iter().take(count).enumerate() {
        let xz = origin + fwd_h * ahead + right_h * lat;
        let ground = generator.surface_height_at(xz.x.round() as i32, xz.z.round() as i32) as f32
            + 1.15;
        let desired = origin.y - 6.0;
        let y = if (ground - desired).abs() < 10.0 {
            ground
        } else {
            desired
        };
        let pos = Vec3::new(xz.x, y, xz.z);
        let span = right_h * walk;
        let suit = materials.add(StandardMaterial {
            base_color: suits[i],
            perceptual_roughness: 0.62,
            metallic: 0.12,
            emissive: LinearRgba::rgb(0.04, 0.05, 0.06),
            ..default()
        });
        let visor = materials.add(StandardMaterial {
            base_color: Color::srgb(0.08, 0.72, 0.92),
            emissive: LinearRgba::rgb(0.15, 1.4, 1.9),
            perceptual_roughness: 0.12,
            ..default()
        });
        let root = commands
            .spawn((
                SpatialBundle {
                    transform: Transform::from_translation(pos),
                    ..default()
                },
                ColonyWalker {
                    t: i as f32 * 0.17,
                    speed,
                    origin: pos,
                    span,
                },
                Name::new("ColonyWalker"),
            ))
            .id();
        commands.entity(root).with_children(|p| {
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: suit.clone(),
                transform: Transform::from_xyz(0.0, 0.10, 0.0).with_scale(Vec3::new(0.32, 0.48, 0.22)),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: suit.clone(),
                transform: Transform::from_xyz(0.0, 0.48, 0.0).with_scale(Vec3::new(0.26, 0.24, 0.26)),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: visor,
                transform: Transform::from_xyz(0.0, 0.50, -0.12).with_scale(Vec3::new(0.22, 0.10, 0.08)),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: suit,
                transform: Transform::from_xyz(0.0, -0.28, 0.0).with_scale(Vec3::new(0.28, 0.22, 0.20)),
                ..default()
            });
        });
    }
}

fn spawn_skyway_trams(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    cube: &Handle<Mesh>,
    origin: Vec3,
    graphics: GraphicsMode,
    cinematic: bool,
) {
    let count = skyway_tram_count(graphics, cinematic);
    let (_fwd, fwd_h, right_h) = new_world_look_basis();
    let hull = materials.add(StandardMaterial {
        base_color: Color::srgb(0.62, 0.66, 0.72),
        perceptual_roughness: 0.45,
        metallic: 0.22,
        emissive: LinearRgba::rgb(0.08, 0.10, 0.14),
        ..default()
    });
    let glow = materials.add(StandardMaterial {
        base_color: Color::srgb(0.05, 0.78, 1.0),
        emissive: LinearRgba::rgb(0.2, 4.5, 6.0),
        alpha_mode: AlphaMode::Add,
        ..default()
    });
    let lanes: [(f32, f32, f32, f32, f32, f32, f32, f32); 2] = [
        (36.0, 14.0, -32.0, 10.0, 48.0, 2.0, 0.030, 0.08),
        (52.0, 18.0, 18.0, -8.0, -40.0, 3.0, 0.022, 0.55),
    ];
    for (i, &(ahead, height, lat, da, dl, du, speed, t0)) in lanes.iter().take(count).enumerate() {
        let lane_origin = origin + fwd_h * ahead + Vec3::Y * height + right_h * lat;
        let span = fwd_h * da + right_h * dl + Vec3::Y * du;
        let u = t0.rem_euclid(1.0);
        let pos = lane_origin + span * u;
        let yaw = (-span.x).atan2(-span.z);
        let root = commands
            .spawn((
                SpatialBundle {
                    transform: Transform::from_translation(pos)
                        .with_rotation(Quat::from_rotation_y(yaw)),
                    ..default()
                },
                SkywayTram {
                    t: t0,
                    speed,
                    origin: lane_origin,
                    span,
                },
                Name::new("SkywayTram"),
            ))
            .id();
        let scale = if i == 0 { 1.0 } else { 0.82 };
        commands.entity(root).with_children(|p| {
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: hull.clone(),
                transform: Transform::from_scale(Vec3::new(1.15 * scale, 0.55 * scale, 2.4 * scale)),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: glow.clone(),
                transform: Transform::from_xyz(0.0, 0.18 * scale, 0.0)
                    .with_scale(Vec3::new(0.95 * scale, 0.18 * scale, 1.8 * scale)),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: glow.clone(),
                transform: Transform::from_xyz(0.0, -0.12 * scale, -1.35 * scale)
                    .with_scale(Vec3::new(0.22 * scale, 0.10 * scale, 0.55 * scale)),
                ..default()
            });
        });
    }
}

fn update_colony_walkers(time: Res<Time>, mut q: Query<(&mut Transform, &mut ColonyWalker)>) {
    let dt = time.delta_seconds();
    for (mut tf, mut walker) in q.iter_mut() {
        walker.t = (walker.t + dt * walker.speed).rem_euclid(1.0);
        let u = ping_pong(walker.t);
        let pos = walker.origin + walker.span * (u - 0.5);
        let dir = if walker.t.rem_euclid(1.0) < 0.5 {
            walker.span
        } else {
            -walker.span
        };
        tf.translation = pos;
        if dir.length_squared() > 0.01 {
            tf.rotation = Quat::from_rotation_y((-dir.x).atan2(-dir.z));
        }
        tf.translation.y = walker.origin.y + (walker.t * std::f32::consts::TAU * 2.0).sin().abs() * 0.06;
    }
}

fn update_skyway_trams(time: Res<Time>, mut q: Query<(&mut Transform, &mut SkywayTram)>) {
    let dt = time.delta_seconds();
    for (mut tf, mut tram) in q.iter_mut() {
        tram.t = (tram.t + dt * tram.speed).rem_euclid(1.0);
        let pos = tram.origin + tram.span * tram.t;
        tf.translation = pos;
        tf.rotation = Quat::from_rotation_y((-tram.span.x).atan2(-tram.span.z));
    }
}

fn cleanup_colony_life(
    mut commands: Commands,
    entities: Query<Entity, Or<(With<ColonyWalker>, With<SkywayTram>)>>,
) {
    for entity in entities.iter() {
        if let Some(entity_commands) = commands.get_entity(entity) {
            entity_commands.despawn_recursive();
        }
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

    #[test]
    fn colony_life_is_bounded_and_fast_skips_figures() {
        assert_eq!(colony_figure_count(GraphicsMode::Fast, false), 0);
        assert_eq!(colony_figure_count(GraphicsMode::Balanced, false), 3);
        assert_eq!(colony_figure_count(GraphicsMode::High, true), 5);
        assert_eq!(skyway_tram_count(GraphicsMode::Fast, false), 1);
        assert_eq!(skyway_tram_count(GraphicsMode::High, true), 2);
        assert!((ping_pong(0.0) - ping_pong(1.0)).abs() < 1e-5);
        assert!((ping_pong(0.25) - 0.5).abs() < 1e-5);
        assert!(ping_pong(0.0) >= 0.0 && ping_pong(0.0) <= 1.0);
    }
}
