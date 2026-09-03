//! Monorail carts that ride the terrain-stamped showcase track grid.

use bevy::prelude::*;

use crate::menu::GameState;
use crate::player::Player;
use crate::settings::WorldSettings;
use crate::terrain::{MonorailAxis, MonorailSite};
use crate::world::VoxelWorld;

pub struct RailsPlugin;

impl Plugin for RailsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(MonorailRuntime::default())
            .add_systems(OnEnter(GameState::MainMenu), cleanup_monorail_carts)
            .add_systems(
                Update,
                (spawn_nearby_monorail_carts, update_monorail_carts)
                    .chain()
                    .run_if(in_state(GameState::InGame)),
            );
    }
}

#[derive(Resource, Default)]
struct MonorailRuntime {
    carts: Vec<Entity>,
}

#[derive(Component)]
struct MonorailCart {
    axis: MonorailAxis,
    line_coord: i32,
    base: i32,
    travel: f32,
    speed: f32,
}

fn cleanup_monorail_carts(mut commands: Commands, carts: Query<Entity, With<MonorailCart>>) {
    for entity in carts.iter() {
        commands.entity(entity).despawn_recursive();
    }
}

fn spawn_nearby_monorail_carts(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut runtime: ResMut<MonorailRuntime>,
    settings: Res<WorldSettings>,
    world: Res<VoxelWorld>,
    player_q: Query<&Transform, With<Player>>,
) {
    runtime.carts.retain(|entity| commands.get_entity(*entity).is_some());
    if runtime.carts.len() >= 6 {
        return;
    }
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let px = player_tf.translation.x.round() as i32;
    let pz = player_tf.translation.z.round() as i32;
    let mesh = meshes.add(Cuboid::new(1.4, 0.9, 2.2));
    let body_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.72, 0.76, 0.82),
        metallic: 0.85,
        perceptual_roughness: 0.28,
        ..default()
    });
    let glow_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.0, 0.95, 1.0),
        emissive: LinearRgba::rgb(0.0, 3.5, 4.5),
        ..default()
    });

    for gx in ((px - 240)..=(px + 240)).step_by(48) {
        for gz in ((pz - 240)..=(pz + 240)).step_by(48) {
            if runtime.carts.len() >= 6 {
                return;
            }
            let Some(site) = world.generator.monorail_at(gx, gz) else {
                continue;
            };
            let (base, wx, wz) = match site.axis {
                MonorailAxis::AlongX => (gx, gx, site.line_coord),
                MonorailAxis::AlongZ => (gz, site.line_coord, gz),
            };
            let surface = world.generator.surface_height_at(wx, wz) as f32;
            let travel = column_phase(settings.seed, wx, wz) * 48.0;
            let cart = commands
                .spawn((
                    PbrBundle {
                        mesh: mesh.clone(),
                        material: body_mat.clone(),
                        transform: cart_transform(site, base, surface, travel),
                        ..default()
                    },
                    MonorailCart {
                        axis: site.axis,
                        line_coord: site.line_coord,
                        base,
                        travel,
                        speed: 7.5 + (column_phase(settings.seed, wx, wz + 17) * 3.0),
                    },
                    Name::new("MonorailCart"),
                ))
                .id();
            commands.entity(cart).with_children(|parent| {
                parent.spawn(PbrBundle {
                    mesh: meshes.add(Cuboid::new(0.5, 0.35, 0.5)),
                    material: glow_mat.clone(),
                    transform: Transform::from_xyz(0.0, 0.35, -0.9),
                    ..default()
                });
            });
            runtime.carts.push(cart);
        }
    }
}

fn update_monorail_carts(
    time: Res<Time>,
    world: Res<VoxelWorld>,
    mut carts: Query<(&mut MonorailCart, &mut Transform)>,
) {
    let dt = time.delta_seconds().min(1.0 / 20.0);
    for (mut cart, mut tf) in carts.iter_mut() {
        cart.travel += cart.speed * dt;
        if cart.travel > 48.0 {
            cart.travel -= 48.0;
        }
        let (wx, wz) = match cart.axis {
            MonorailAxis::AlongX => (cart.base + cart.travel.round() as i32, cart.line_coord),
            MonorailAxis::AlongZ => (cart.line_coord, cart.base + cart.travel.round() as i32),
        };
        let surface = world.generator.surface_height_at(wx, wz) as f32;
        *tf = cart_transform(
            MonorailSite {
                axis: cart.axis,
                line_coord: cart.line_coord,
            },
            cart.base,
            surface,
            cart.travel,
        );
    }
}

fn cart_transform(site: MonorailSite, base: i32, surface: f32, travel: f32) -> Transform {
    let y = surface + 2.4;
    let (x, z, yaw) = match site.axis {
        MonorailAxis::AlongX => (
            base as f32 + travel,
            site.line_coord as f32,
            0.0,
        ),
        MonorailAxis::AlongZ => (
            site.line_coord as f32,
            base as f32 + travel,
            std::f32::consts::FRAC_PI_2,
        ),
    };
    Transform::from_xyz(x, y, z).with_rotation(Quat::from_rotation_y(yaw))
}

fn column_phase(seed: u32, x: i32, z: i32) -> f32 {
    let mut h = seed as u64;
    h ^= (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h = h.rotate_left(27).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= (z as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((h >> 11) as f32) / ((1u64 << 21) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monorail_cart_transform_follows_x_axis() {
        let site = MonorailSite {
            axis: MonorailAxis::AlongX,
            line_coord: 96,
        };
        let tf = cart_transform(site, 100, 72.0, 4.0);
        assert!((tf.translation.x - 104.0).abs() < 0.01);
        assert!((tf.translation.z - 96.0).abs() < 0.01);
        assert!((tf.translation.y - 74.4).abs() < 0.01);
    }
}
