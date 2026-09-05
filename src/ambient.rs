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
                    update_frontier_outposts,
                    update_frontier_drones,
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

#[derive(Component)]
struct ColonyPad;

#[derive(Component)]
struct FrontierOutpost {
    index: usize,
    cell: IVec2,
}

#[derive(Component)]
struct FrontierDrone {
    index: usize,
    t: f32,
    speed: f32,
    origin: Vec3,
    span: Vec3,
}

/// Postcard-readable suit: ~3.1 m tall, ~1.05 m at the shoulders.
/// NASA EMU standing height is ~1.88 m (ISS EVA); these are staged larger
/// so 2–3 figures still read at ~9 m in the 78° vertical spawn look
/// instead of clipping under the frame or collapsing to specks.
const FIGURE_TORSO: Vec3 = Vec3::new(1.05, 1.65, 0.68);
const FIGURE_HEAD: Vec3 = Vec3::new(0.78, 0.72, 0.78);
const FIGURE_LEG: Vec3 = Vec3::new(0.90, 0.70, 0.58);
const FIGURE_VISOR: Vec3 = Vec3::new(0.62, 0.32, 0.18);
const TRAM_HULL: Vec3 = Vec3::new(3.40, 2.55, 9.20);
const OUTPOST_POOL: usize = 8;
/// World lattice — one candidate landing every this many metres, then a
/// hash keeps occupancy sparse. 144 m sits inside Fast RD 12 (192 m)
/// for the nearest neighbour and inside cinematic RD 32 for the ring.
const OUTPOST_CELL: f32 = 144.0;
const OUTPOST_SEARCH: i32 = 3;
const OUTPOST_OCCUPANCY: f32 = 0.40;

fn colony_figure_count(graphics: GraphicsMode, cinematic: bool) -> usize {
    match graphics {
        GraphicsMode::Fast => 0,
        GraphicsMode::Balanced => 2,
        GraphicsMode::High if cinematic => 3,
        GraphicsMode::High => 3,
    }
}

fn skyway_tram_count(graphics: GraphicsMode, _cinematic: bool) -> usize {
    // One readable car on the left skyway. A second distant tram just
    // reads as noise at postcard scale; Fast still keeps the single car.
    match graphics {
        GraphicsMode::Fast | GraphicsMode::Balanced | GraphicsMode::High => 1,
    }
}

fn frontier_outpost_count(graphics: GraphicsMode, cinematic: bool) -> usize {
    match graphics {
        GraphicsMode::Fast => 2,
        GraphicsMode::Balanced => 4,
        GraphicsMode::High if cinematic => 6,
        GraphicsMode::High => 5,
    }
}

fn frontier_outpost_figures(graphics: GraphicsMode) -> usize {
    match graphics {
        GraphicsMode::Fast => 0,
        GraphicsMode::Balanced => 1,
        GraphicsMode::High => 2,
    }
}

fn frontier_drone_count(graphics: GraphicsMode, cinematic: bool) -> usize {
    match graphics {
        GraphicsMode::Fast => 1,
        GraphicsMode::Balanced => 2,
        GraphicsMode::High if cinematic => 3,
        GraphicsMode::High => 2,
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
    existing: Query<
        Entity,
        Or<(
            With<ColonyWalker>,
            With<SkywayTram>,
            With<ColonyPad>,
            With<FrontierOutpost>,
            With<FrontierDrone>,
        )>,
    >,
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
    spawn_frontier_outposts(
        &mut commands,
        &mut materials,
        &cube,
        settings.graphics,
        cinematic,
    );
}

/// Near pad in the lower third of the authored spawn look. Hovered a
/// few metres above the cyan river so 78° vertical FOV keeps the suits
/// on-screen (origin.y-10 at 12 m was below the bottom of the frame).
fn postcard_pad_anchor(origin: Vec3) -> (Vec3, Vec3, Vec3) {
    let (_fwd, fwd_h, right_h) = new_world_look_basis();
    let pos = origin + fwd_h * 8.5 + right_h * 2.4 + Vec3::Y * -5.0;
    (pos, fwd_h, right_h)
}

/// Side-on tram on the dark left T. The hero deck runs along look (-X),
/// so a car on that rail is end-on and reads as a cyan streak; slide
/// along camera-right instead so the 9 m hull is a visible boxcar.
fn postcard_tram_lane(origin: Vec3) -> (Vec3, Vec3) {
    let (_fwd, fwd_h, right_h) = new_world_look_basis();
    let lane_origin = origin + fwd_h * 18.0 + right_h * (-19.0) + Vec3::Y * 15.0;
    let span = right_h * -18.0;
    (lane_origin, span)
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
    let (mut pad, fwd_h, right_h) = postcard_pad_anchor(origin);
    let desired_y = pad.y;
    let mut best_score = f32::INFINITY;
    for lat in [2.4, 0.8, -0.6, 3.6] {
        let xz = origin + fwd_h * 8.5 + right_h * lat;
        let ground = generator.surface_height_at(xz.x.round() as i32, xz.z.round() as i32) as f32;
        // Keep the pad in the open canyon volume, never on a mesa that
        // would hide the suits or drop them under the 78° frame.
        let score = if ground > origin.y - 4.0 {
            80.0 + (ground - desired_y).abs()
        } else {
            (ground - desired_y).abs()
        };
        if score < best_score {
            best_score = score;
            pad = Vec3::new(xz.x, desired_y, xz.z);
        }
    }
    let ground = generator.surface_height_at(pad.x.round() as i32, pad.z.round() as i32) as f32;
    if ground < origin.y - 4.0 && ground > desired_y - 2.0 && ground < desired_y + 3.0 {
        pad.y = ground + 0.22;
    }
    pad.y = pad.y.max(origin.y - 6.0);
    let deck = materials.add(StandardMaterial {
        base_color: Color::srgb(0.18, 0.20, 0.24),
        perceptual_roughness: 0.52,
        metallic: 0.16,
        emissive: LinearRgba::rgb(0.05, 0.06, 0.07),
        ..default()
    });
    let rim = materials.add(StandardMaterial {
        base_color: Color::srgb(0.05, 0.78, 1.0),
        emissive: LinearRgba::rgb(0.08, 1.35, 1.85),
        perceptual_roughness: 0.22,
        ..default()
    });
    let pad_root = commands
        .spawn((
            SpatialBundle {
                transform: Transform::from_translation(pad),
                ..default()
            },
            ColonyPad,
            Name::new("ColonyPad"),
        ))
        .id();
    commands.entity(pad_root).with_children(|p| {
        p.spawn(PbrBundle {
            mesh: cube.clone(),
            material: deck,
            transform: Transform::from_scale(Vec3::new(6.4, 0.46, 8.6)),
            ..default()
        });
        p.spawn(PbrBundle {
            mesh: cube.clone(),
            material: rim.clone(),
            transform: Transform::from_xyz(0.0, 0.26, 4.15).with_scale(Vec3::new(6.4, 0.12, 0.20)),
            ..default()
        });
        p.spawn(PbrBundle {
            mesh: cube.clone(),
            material: rim,
            transform: Transform::from_xyz(0.0, 0.26, -4.15).with_scale(Vec3::new(6.4, 0.12, 0.20)),
            ..default()
        });
    });

    // Local offsets on the pad (ahead, lat, walk, speed). Keep the
    // patrol inside the 7×5 deck so figures never walk off into air.
    let spots: [(f32, f32, f32, f32); 3] = [
        (0.8, -1.7, 1.6, 0.020),
        (-0.4, 1.5, 1.4, 0.024),
        (1.2, 0.2, 1.2, 0.018),
    ];
    let suits = [
        Color::srgb(0.16, 0.46, 0.72),
        Color::srgb(0.18, 0.52, 0.24),
        Color::srgb(0.78, 0.78, 0.82),
    ];
    let stand_y = pad.y + 0.19;
    for (i, &(ahead, lat, walk, speed)) in spots.iter().take(count).enumerate() {
        let pos = Vec3::new(pad.x, stand_y, pad.z) + fwd_h * ahead + right_h * lat;
        let span = right_h * walk;
        let suit = materials.add(StandardMaterial {
            base_color: suits[i],
            perceptual_roughness: 0.55,
            metallic: 0.16,
            emissive: LinearRgba::rgb(0.06, 0.07, 0.08),
            ..default()
        });
        let visor = materials.add(StandardMaterial {
            base_color: Color::srgb(0.08, 0.78, 0.96),
            emissive: LinearRgba::rgb(0.18, 1.55, 2.05),
            perceptual_roughness: 0.10,
            ..default()
        });
        let root = commands
            .spawn((
                SpatialBundle {
                    transform: Transform::from_translation(pos),
                    ..default()
                },
                ColonyWalker {
                    t: i as f32 * 0.21,
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
                transform: Transform::from_xyz(0.0, FIGURE_LEG.y * 0.5, 0.0).with_scale(FIGURE_LEG),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: suit.clone(),
                transform: Transform::from_xyz(0.0, FIGURE_LEG.y + FIGURE_TORSO.y * 0.5, 0.0)
                    .with_scale(FIGURE_TORSO),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: suit.clone(),
                transform: Transform::from_xyz(
                    0.0,
                    FIGURE_LEG.y + FIGURE_TORSO.y + FIGURE_HEAD.y * 0.5,
                    0.0,
                )
                .with_scale(FIGURE_HEAD),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: visor,
                transform: Transform::from_xyz(
                    0.0,
                    FIGURE_LEG.y + FIGURE_TORSO.y + FIGURE_HEAD.y * 0.52,
                    -FIGURE_HEAD.z * 0.42,
                )
                .with_scale(FIGURE_VISOR),
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
    if count == 0 {
        return;
    }
    let hull = materials.add(StandardMaterial {
        base_color: Color::srgb(0.78, 0.81, 0.86),
        perceptual_roughness: 0.38,
        metallic: 0.22,
        emissive: LinearRgba::rgb(0.10, 0.11, 0.13),
        ..default()
    });
    let stripe = materials.add(StandardMaterial {
        base_color: Color::srgb(0.92, 0.42, 0.10),
        perceptual_roughness: 0.40,
        metallic: 0.08,
        emissive: LinearRgba::rgb(0.55, 0.16, 0.02),
        ..default()
    });
    let glow = materials.add(StandardMaterial {
        base_color: Color::srgb(0.06, 0.82, 1.0),
        emissive: LinearRgba::rgb(0.12, 1.45, 1.90),
        perceptual_roughness: 0.16,
        ..default()
    });
    let (lane_origin, span) = postcard_tram_lane(origin);
    let t0 = 0.22;
    let pos = lane_origin + span * ping_pong(t0);
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
                speed: 0.038,
                origin: lane_origin,
                span,
            },
            Name::new("SkywayTram"),
        ))
        .id();
    commands.entity(root).with_children(|p| {
        p.spawn(PbrBundle {
            mesh: cube.clone(),
            material: hull,
            transform: Transform::from_scale(TRAM_HULL),
            ..default()
        });
        p.spawn(PbrBundle {
            mesh: cube.clone(),
            material: stripe,
            transform: Transform::from_xyz(0.0, 0.15, 0.0).with_scale(Vec3::new(
                TRAM_HULL.x + 0.12,
                0.42,
                TRAM_HULL.z * 0.92,
            )),
            ..default()
        });
        p.spawn(PbrBundle {
            mesh: cube.clone(),
            material: glow.clone(),
            transform: Transform::from_xyz(0.0, 0.35, 0.0).with_scale(Vec3::new(
                TRAM_HULL.x * 0.72,
                0.85,
                TRAM_HULL.z * 0.78,
            )),
            ..default()
        });
        p.spawn(PbrBundle {
            mesh: cube.clone(),
            material: glow,
            transform: Transform::from_xyz(0.0, -0.35, -TRAM_HULL.z * 0.52)
                .with_scale(Vec3::new(1.05, 0.32, 0.90)),
            ..default()
        });
    });
}

fn outpost_cell_center(cx: i32, cz: i32) -> Vec2 {
    let jitter_x = (hash01(cx, cz, 0x0B05_701) - 0.5) * OUTPOST_CELL * 0.30;
    let jitter_z = (hash01(cz, cx, 0x0B05_702) - 0.5) * OUTPOST_CELL * 0.30;
    Vec2::new(
        (cx as f32 + 0.5) * OUTPOST_CELL + jitter_x,
        (cz as f32 + 0.5) * OUTPOST_CELL + jitter_z,
    )
}

fn cell_hosts_outpost(cx: i32, cz: i32) -> bool {
    hash01(cx, cz, 0x0B05_700) < OUTPOST_OCCUPANCY
}

/// Nearest world-lattice outpost cells to the player. Occupancy is a
/// pure hash of the cell so flying into a new area shows the same pad
/// that was already "there"; the pool just retargets, it never grows.
fn nearest_outpost_cells(player_pos: Vec3, count: usize) -> Vec<IVec2> {
    let base_x = (player_pos.x / OUTPOST_CELL).floor() as i32;
    let base_z = (player_pos.z / OUTPOST_CELL).floor() as i32;
    let mut ranked: Vec<(f32, IVec2)> = Vec::new();
    for dz in -OUTPOST_SEARCH..=OUTPOST_SEARCH {
        for dx in -OUTPOST_SEARCH..=OUTPOST_SEARCH {
            let cx = base_x + dx;
            let cz = base_z + dz;
            if !cell_hosts_outpost(cx, cz) {
                continue;
            }
            let xz = outpost_cell_center(cx, cz);
            let wx = xz.x.round() as i32;
            let wz = xz.y.round() as i32;
            if crate::frontier::in_hero_postcard(wx, wz) {
                continue;
            }
            let dist = (xz - player_pos.xz()).length();
            ranked.push((dist, IVec2::new(cx, cz)));
        }
    }
    ranked.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    ranked
        .into_iter()
        .take(count.max(1))
        .map(|(_, cell)| cell)
        .collect()
}

fn spawn_frontier_outposts(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    cube: &Handle<Mesh>,
    graphics: GraphicsMode,
    cinematic: bool,
) {
    let pads = frontier_outpost_count(graphics, cinematic);
    let figures = frontier_outpost_figures(graphics);
    let drones = frontier_drone_count(graphics, cinematic);
    let deck = materials.add(StandardMaterial {
        base_color: Color::srgb(0.16, 0.18, 0.22),
        perceptual_roughness: 0.55,
        metallic: 0.16,
        emissive: LinearRgba::rgb(0.04, 0.05, 0.06),
        ..default()
    });
    let rim = materials.add(StandardMaterial {
        base_color: Color::srgb(0.06, 0.78, 1.0),
        emissive: LinearRgba::rgb(0.08, 1.20, 1.65),
        perceptual_roughness: 0.22,
        ..default()
    });
    let suits = [Color::srgb(0.18, 0.46, 0.70), Color::srgb(0.78, 0.76, 0.72)];
    let visor = materials.add(StandardMaterial {
        base_color: Color::srgb(0.08, 0.78, 0.96),
        emissive: LinearRgba::rgb(0.16, 1.35, 1.80),
        perceptual_roughness: 0.12,
        ..default()
    });
    let hull = materials.add(StandardMaterial {
        base_color: Color::srgb(0.72, 0.74, 0.80),
        metallic: 0.20,
        perceptual_roughness: 0.40,
        emissive: LinearRgba::rgb(0.08, 0.09, 0.11),
        ..default()
    });
    let glow = materials.add(StandardMaterial {
        base_color: Color::srgb(0.06, 0.82, 1.0),
        emissive: LinearRgba::rgb(0.10, 1.40, 1.85),
        perceptual_roughness: 0.16,
        ..default()
    });
    for i in 0..pads {
        let root = commands
            .spawn((
                SpatialBundle {
                    transform: Transform::from_translation(Vec3::new(0.0, -10_000.0, 0.0)),
                    visibility: Visibility::Hidden,
                    ..default()
                },
                FrontierOutpost {
                    index: i,
                    cell: IVec2::new(i32::MIN, i32::MIN),
                },
                Name::new("FrontierOutpost"),
            ))
            .id();
        commands.entity(root).with_children(|p| {
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: deck.clone(),
                transform: Transform::from_scale(Vec3::new(6.2, 0.34, 6.2)),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: rim.clone(),
                transform: Transform::from_xyz(0.0, 0.22, 3.05)
                    .with_scale(Vec3::new(6.2, 0.10, 0.18)),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: rim.clone(),
                transform: Transform::from_xyz(0.0, 0.22, -3.05)
                    .with_scale(Vec3::new(6.2, 0.10, 0.18)),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: deck.clone(),
                transform: Transform::from_xyz(-2.4, 2.4, -2.4)
                    .with_scale(Vec3::new(0.28, 4.8, 0.28)),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: rim.clone(),
                transform: Transform::from_xyz(-2.4, 4.95, -2.4)
                    .with_scale(Vec3::new(0.55, 0.40, 0.55)),
                ..default()
            });
            for f in 0..figures {
                let lat = if f == 0 { -1.1 } else { 1.15 };
                let suit = materials.add(StandardMaterial {
                    base_color: suits[f % suits.len()],
                    perceptual_roughness: 0.55,
                    metallic: 0.14,
                    emissive: LinearRgba::rgb(0.05, 0.06, 0.07),
                    ..default()
                });
                let figure_s = 0.72;
                let stand = Vec3::new(lat, 0.20, 0.4 - f as f32 * 0.8);
                p.spawn((
                    SpatialBundle {
                        transform: Transform::from_translation(stand),
                        ..default()
                    },
                    ColonyWalker {
                        t: f as f32 * 0.31 + i as f32 * 0.07,
                        speed: 0.018 + f as f32 * 0.004,
                        origin: stand,
                        span: Vec3::X * 1.4,
                    },
                ))
                .with_children(|body| {
                    body.spawn(PbrBundle {
                        mesh: cube.clone(),
                        material: suit.clone(),
                        transform: Transform::from_xyz(0.0, FIGURE_LEG.y * 0.5 * figure_s, 0.0)
                            .with_scale(FIGURE_LEG * figure_s),
                        ..default()
                    });
                    body.spawn(PbrBundle {
                        mesh: cube.clone(),
                        material: suit.clone(),
                        transform: Transform::from_xyz(
                            0.0,
                            (FIGURE_LEG.y + FIGURE_TORSO.y * 0.5) * figure_s,
                            0.0,
                        )
                        .with_scale(FIGURE_TORSO * figure_s),
                        ..default()
                    });
                    body.spawn(PbrBundle {
                        mesh: cube.clone(),
                        material: visor.clone(),
                        transform: Transform::from_xyz(
                            0.0,
                            (FIGURE_LEG.y + FIGURE_TORSO.y + FIGURE_HEAD.y * 0.52) * figure_s,
                            -FIGURE_HEAD.z * 0.42 * figure_s,
                        )
                        .with_scale(FIGURE_VISOR * figure_s),
                        ..default()
                    });
                });
            }
        });
    }
    for i in 0..drones {
        commands
            .spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: hull.clone(),
                    transform: Transform::from_translation(Vec3::new(0.0, -10_000.0, 0.0))
                        .with_scale(Vec3::new(1.6, 0.55, 3.4)),
                    visibility: Visibility::Hidden,
                    ..default()
                },
                FrontierDrone {
                    index: i,
                    t: i as f32 * 0.27,
                    speed: 0.030 + i as f32 * 0.006,
                    origin: Vec3::ZERO,
                    span: Vec3::X * 22.0,
                },
                Name::new("FrontierDrone"),
            ))
            .with_children(|d| {
                d.spawn(PbrBundle {
                    mesh: cube.clone(),
                    material: glow.clone(),
                    transform: Transform::from_xyz(0.0, 0.12, 0.0)
                        .with_scale(Vec3::new(0.55, 0.22, 0.72)),
                    ..default()
                });
            });
    }
}

fn place_outpost_at(
    tf: &mut Transform,
    visibility: &mut Visibility,
    outpost: &mut FrontierOutpost,
    world: &VoxelWorld,
    want: IVec2,
) {
    if want.x == i32::MIN {
        *visibility = Visibility::Hidden;
        return;
    }
    let xz = outpost_cell_center(want.x, want.y);
    let wx = xz.x.round() as i32;
    let wz = xz.y.round() as i32;
    if crate::frontier::in_hero_postcard(wx, wz) {
        *visibility = Visibility::Hidden;
        return;
    }
    let surface = world.surface_height_at(wx, wz);
    if surface <= WATER_LEVEL + 4 {
        *visibility = Visibility::Hidden;
        return;
    }
    let biome = world.biome_at(wx, wz);
    if matches!(biome, Biome::Ocean) {
        *visibility = Visibility::Hidden;
        return;
    }
    if want == outpost.cell && *visibility == Visibility::Visible {
        return;
    }
    outpost.cell = want;
    tf.translation = Vec3::new(xz.x, surface as f32 + 0.22, xz.y);
    *visibility = Visibility::Visible;
}

fn update_frontier_outposts(
    player_q: Query<&Transform, (With<Player>, Without<FrontierOutpost>)>,
    world: Res<VoxelWorld>,
    mut q: Query<(&mut Transform, &mut Visibility, &mut FrontierOutpost), Without<Player>>,
) {
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let cells = nearest_outpost_cells(player_tf.translation, OUTPOST_POOL);
    for (mut tf, mut vis, mut outpost) in q.iter_mut() {
        let want = cells
            .get(outpost.index)
            .copied()
            .unwrap_or(IVec2::new(i32::MIN, i32::MIN));
        if want == outpost.cell && *vis == Visibility::Visible {
            continue;
        }
        place_outpost_at(&mut tf, &mut vis, &mut outpost, &world, want);
    }
}

fn update_frontier_drones(
    time: Res<Time>,
    player_q: Query<&Transform, (With<Player>, Without<FrontierDrone>)>,
    world: Res<VoxelWorld>,
    mut q: Query<(&mut Transform, &mut Visibility, &mut FrontierDrone), Without<Player>>,
) {
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let dt = time.delta_seconds();
    let cells = nearest_outpost_cells(player_tf.translation, OUTPOST_POOL);
    for (mut tf, mut vis, mut drone) in q.iter_mut() {
        let want = cells
            .get(drone.index)
            .copied()
            .unwrap_or(IVec2::new(i32::MIN, i32::MIN));
        if want.x == i32::MIN {
            *vis = Visibility::Hidden;
            continue;
        }
        let xz = outpost_cell_center(want.x, want.y);
        let wx = xz.x.round() as i32;
        let wz = xz.y.round() as i32;
        if crate::frontier::in_hero_postcard(wx, wz) {
            *vis = Visibility::Hidden;
            continue;
        }
        let surface = world.surface_height_at(wx, wz) as f32;
        drone.origin = Vec3::new(xz.x - 11.0, surface + 9.0, xz.y);
        drone.span = Vec3::new(22.0, 1.5, 6.0);
        drone.t = (drone.t + dt * drone.speed).rem_euclid(1.0);
        let u = ping_pong(drone.t);
        tf.translation = drone.origin + drone.span * u;
        tf.rotation = Quat::from_rotation_y((-drone.span.x).atan2(-drone.span.z));
        *vis = Visibility::Visible;
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
        tf.translation.y =
            walker.origin.y + (walker.t * std::f32::consts::TAU * 2.0).sin().abs() * 0.08;
    }
}

fn update_skyway_trams(time: Res<Time>, mut q: Query<(&mut Transform, &mut SkywayTram)>) {
    let dt = time.delta_seconds();
    for (mut tf, mut tram) in q.iter_mut() {
        tram.t = (tram.t + dt * tram.speed).rem_euclid(1.0);
        let u = ping_pong(tram.t);
        tf.translation = tram.origin + tram.span * u;
        let dir = if tram.t.rem_euclid(1.0) < 0.5 {
            tram.span
        } else {
            -tram.span
        };
        if dir.length_squared() > 0.01 {
            tf.rotation = Quat::from_rotation_y((-dir.x).atan2(-dir.z));
        }
    }
}

fn cleanup_colony_life(
    mut commands: Commands,
    entities: Query<
        Entity,
        Or<(
            With<ColonyWalker>,
            With<SkywayTram>,
            With<ColonyPad>,
            With<FrontierOutpost>,
            With<FrontierDrone>,
        )>,
    >,
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
        assert_eq!(colony_figure_count(GraphicsMode::Balanced, false), 2);
        assert_eq!(colony_figure_count(GraphicsMode::High, true), 3);
        assert_eq!(skyway_tram_count(GraphicsMode::Fast, false), 1);
        assert_eq!(skyway_tram_count(GraphicsMode::High, true), 1);
        assert!((ping_pong(0.0) - ping_pong(1.0)).abs() < 1e-5);
        assert!((ping_pong(0.25) - 0.5).abs() < 1e-5);
        assert!(ping_pong(0.0) >= 0.0 && ping_pong(0.0) <= 1.0);
    }

    #[test]
    fn colony_life_reads_at_postcard_scale() {
        let origin = Vec3::new(64.0, 58.0, -79.0);
        let (pad, fwd_h, right_h) = postcard_pad_anchor(origin);
        let ahead = (pad - origin).dot(fwd_h);
        let right = (pad - origin).dot(right_h);
        assert!(
            ahead > 7.0 && ahead < 12.0,
            "pad should sit in the near field, ahead={ahead}"
        );
        assert!(
            pad.y < origin.y - 3.5 && pad.y > origin.y - 7.0,
            "pad should sit in the lower third of a 78° look, pad.y={} origin.y={}",
            pad.y,
            origin.y
        );
        let pitch = (pad.y - origin.y).atan2(ahead);
        assert!(
            pitch > -0.58 && pitch < -0.12,
            "pad pitch {pitch} should stay inside the lower third, not under the frame"
        );
        assert!(
            right > 1.0,
            "pad should sit right of centre so the left skyway stays clear"
        );

        let figure_h = FIGURE_LEG.y + FIGURE_TORSO.y + FIGURE_HEAD.y;
        assert!(
            figure_h > 2.8,
            "suited figures must be larger than a speck, height={figure_h}"
        );
        assert!(FIGURE_TORSO.x > 0.9, "shoulder width must read at 9 m");
        assert!(
            TRAM_HULL.z > 8.0,
            "tram car must be a short visible box, not a streak"
        );
        assert!(
            TRAM_HULL.y > 2.2,
            "tram must be tall enough to read against the dark T"
        );

        let (tram_o, tram_span) = postcard_tram_lane(origin);
        assert!(
            tram_o.y > origin.y + 8.0,
            "tram should sit on the upper-left T"
        );
        assert!(tram_span.length() > 12.0 && tram_span.length() < 28.0);
        let left = (tram_o - origin).dot(right_h);
        assert!(
            left < -8.0,
            "tram should sit on the left of the spawn look, lat={left}"
        );
        let along = tram_span.normalize_or_zero().dot(right_h).abs();
        assert!(
            along > 0.9,
            "tram travel must be side-on, not end-on along look"
        );
    }

    #[test]
    fn frontier_outposts_are_sparse_and_fast_skips_figures() {
        assert_eq!(frontier_outpost_count(GraphicsMode::Fast, false), 2);
        assert_eq!(frontier_outpost_figures(GraphicsMode::Fast), 0);
        assert_eq!(frontier_drone_count(GraphicsMode::Fast, false), 1);
        assert_eq!(frontier_outpost_count(GraphicsMode::High, true), 6);
        assert_eq!(frontier_outpost_figures(GraphicsMode::High), 2);
        assert!(frontier_outpost_count(GraphicsMode::High, true) <= OUTPOST_POOL);
        let player = Vec3::new(64.0, 58.0, -79.0);
        let cells = nearest_outpost_cells(player, 6);
        assert!(
            !cells.is_empty(),
            "spawn neighbourhood should host at least one non-postcard pad"
        );
        let a = outpost_cell_center(cells[0].x, cells[0].y);
        assert!(
            (a - player.xz()).length() < OUTPOST_CELL * 3.5,
            "nearest pad must sit inside cinematic render distance, dist={}",
            (a - player.xz()).length()
        );
        if cells.len() >= 2 {
            let b = outpost_cell_center(cells[1].x, cells[1].y);
            assert!((a - b).length() > 40.0, "outposts should not stack");
        }
        let wx = a.x.round() as i32;
        let wz = a.y.round() as i32;
        assert!(!crate::frontier::in_hero_postcard(wx, wz));
        let elsewhere = Vec3::new(900.0, 40.0, 700.0);
        let far = nearest_outpost_cells(elsewhere, 4);
        assert!(!far.is_empty());
        assert_ne!(
            far[0], cells[0],
            "a new area should pick a different world cell, not drag spawn pads along"
        );
    }
}
