//! Defense turrets that fire red laser beams at hostile drones.

use bevy::prelude::*;

use crate::menu::GameState;
use crate::player::Player;
use crate::settings::WorldSettings;
use crate::ships::EnemyDrone;
use crate::world::VoxelWorld;

pub struct DefensePlugin;

impl Plugin for DefensePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(DefenseRuntime::default())
            .add_systems(OnEnter(GameState::MainMenu), cleanup_defense_runtime)
            .add_systems(
                Update,
                (spawn_nearby_defense_turrets, update_defense_turrets, update_turret_beams)
                    .chain()
                    .run_if(in_state(GameState::InGame)),
            );
    }
}

#[derive(Resource, Default)]
struct DefenseRuntime {
    turrets: Vec<Entity>,
    beam_mesh: Option<Handle<Mesh>>,
    beam_material: Option<Handle<StandardMaterial>>,
}

#[derive(Component)]
struct DefenseTurret {
    origin: Vec3,
    cooldown: f32,
}

#[derive(Component)]
struct TurretBeam {
    velocity: Vec3,
    life: f32,
}

fn cleanup_defense_runtime(
    mut commands: Commands,
    turrets: Query<Entity, With<DefenseTurret>>,
    beams: Query<Entity, With<TurretBeam>>,
) {
    for entity in turrets.iter().chain(beams.iter()) {
        commands.entity(entity).despawn_recursive();
    }
}

fn spawn_nearby_defense_turrets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut runtime: ResMut<DefenseRuntime>,
    world: Res<VoxelWorld>,
    player_q: Query<&Transform, With<Player>>,
) {
    runtime.turrets.retain(|entity| commands.get_entity(*entity).is_some());
    if runtime.turrets.len() >= 8 {
        return;
    }
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let px = player_tf.translation.x.round() as i32;
    let pz = player_tf.translation.z.round() as i32;
    let mesh = meshes.add(Cylinder::new(0.55, 1.8));
    let mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.35, 0.38, 0.42),
        emissive: LinearRgba::rgb(0.4, 0.1, 0.1),
        metallic: 0.9,
        perceptual_roughness: 0.25,
        ..default()
    });

    for gx in ((px - 320)..=(px + 320)).step_by(96) {
        for gz in ((pz - 320)..=(pz + 320)).step_by(96) {
            if runtime.turrets.len() >= 8 {
                return;
            }
            let Some(site) = world.generator.defense_turret_site(gx, gz) else {
                continue;
            };
            let origin = Vec3::new(gx as f32 + 0.5, site.base_y as f32 + 5.5, gz as f32 + 0.5);
            let entity = commands
                .spawn((
                    PbrBundle {
                        mesh: mesh.clone(),
                        material: mat.clone(),
                        transform: Transform::from_translation(origin),
                        ..default()
                    },
                    DefenseTurret {
                        origin,
                        cooldown: 0.0,
                    },
                    Name::new("DefenseTurret"),
                ))
                .id();
            runtime.turrets.push(entity);
        }
    }
}

fn update_defense_turrets(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut runtime: ResMut<DefenseRuntime>,
    mut turrets: Query<(&mut DefenseTurret, &mut Transform)>,
    drones: Query<&Transform, With<EnemyDrone>>,
) {
    if !settings.ship_skirmish_ai {
        return;
    }
    let dt = time.delta_seconds().min(1.0 / 20.0);
    let beam_mesh = runtime
        .beam_mesh
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.18, 0.18, 1.0)))
        .clone();
    let beam_material = runtime
        .beam_material
        .get_or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: Color::srgb(1.0, 0.12, 0.08),
                emissive: LinearRgba::rgb(8.0, 0.6, 0.2),
                alpha_mode: AlphaMode::Add,
                ..default()
            })
        })
        .clone();

    for (mut turret, mut tf) in turrets.iter_mut() {
        turret.cooldown -= dt;
        let Some(target) = nearest_drone(turret.origin, &drones, 96.0) else {
            continue;
        };
        let dir = (target - turret.origin).normalize_or_zero();
        if dir.length_squared() < 1e-4 {
            continue;
        }
        tf.rotation = Quat::from_rotation_arc(Vec3::Y, dir);
        if turret.cooldown > 0.0 {
            continue;
        }
        turret.cooldown = 0.55;
        commands.spawn((
            PbrBundle {
                mesh: beam_mesh.clone(),
                material: beam_material.clone(),
                transform: Transform::from_translation(turret.origin + dir * 1.2)
                    .with_rotation(Quat::from_rotation_arc(Vec3::Z, dir))
                    .with_scale(Vec3::new(1.0, 1.0, 2.4)),
                ..default()
            },
            TurretBeam {
                velocity: dir * 120.0,
                life: 1.2,
            },
            Name::new("TurretBeam"),
        ));
    }
}

fn update_turret_beams(
    time: Res<Time>,
    mut commands: Commands,
    mut beams: Query<(Entity, &mut TurretBeam, &mut Transform)>,
    mut drones: Query<(Entity, &Transform, &mut EnemyDrone)>,
) {
    let dt = time.delta_seconds().min(1.0 / 20.0);
    for (entity, mut beam, mut tf) in beams.iter_mut() {
        beam.life -= dt;
        tf.translation += beam.velocity * dt;
        if beam.life <= 0.0 {
            commands.entity(entity).despawn_recursive();
            continue;
        }
        for (drone_e, drone_tf, mut drone) in drones.iter_mut() {
            if drone_tf.translation.distance(tf.translation) <= 1.4 {
                drone.hp -= 18.0;
                commands.entity(entity).despawn_recursive();
                if drone.hp <= 0.0 {
                    commands.entity(drone_e).despawn_recursive();
                }
                break;
            }
        }
    }
}

pub(crate) fn nearest_drone(
    origin: Vec3,
    drones: &Query<&Transform, With<EnemyDrone>>,
    range: f32,
) -> Option<Vec3> {
    let mut best: Option<(f32, Vec3)> = None;
    for tf in drones.iter() {
        let dist = origin.distance(tf.translation);
        if dist > range {
            continue;
        }
        if best.map_or(true, |(best_dist, _)| dist < best_dist) {
            best = Some((dist, tf.translation));
        }
    }
    best.map(|(_, pos)| pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_drone_prefers_closest_target() {
        let origin = Vec3::new(0.0, 10.0, 0.0);
        let positions = [Vec3::new(40.0, 10.0, 0.0), Vec3::new(8.0, 10.0, 0.0)];
        let mut best: Option<(f32, Vec3)> = None;
        for pos in positions {
            let dist = origin.distance(pos);
            if best.map_or(true, |(best_dist, _)| dist < best_dist) {
                best = Some((dist, pos));
            }
        }
        assert_eq!(best.map(|(_, p)| p), Some(Vec3::new(8.0, 10.0, 0.0)));
    }
}
