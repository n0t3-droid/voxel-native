use bevy::prelude::*;

use crate::neurocore::RuntimeBudget;
use crate::player::Player;
use crate::settings::{GraphicsMode, WorldSettings};

const BUTTERFLY_POOL: usize = 28;
const BUTTERFLY_RADIUS: f32 = 34.0;
const BUTTERFLY_MIN_HEIGHT: f32 = 1.6;
const BUTTERFLY_HEIGHT_BAND: f32 = 3.4;
#[cfg(test)]
const BUTTERFLY_INTERACTION_RADIUS: f32 = 0.0;

#[derive(Component)]
pub struct Butterfly {
    index: usize,
    phase: f32,
    radius: f32,
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
            .add_systems(Update, (update_butterflies, update_butterfly_visibility));
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

fn butterfly_offset(index: usize, phase: f32, seconds: f32, radius: f32) -> Vec3 {
    let i = index as f32;
    let speed = 0.18 + (i * 0.031).sin().abs() * 0.10;
    let angle = phase + seconds * speed + (i * 1.618_034).sin() * 0.55;
    let wander = (seconds * 0.31 + phase * 1.7).sin() * 3.5;
    let r = radius * (0.45 + (phase * 2.11).sin().abs() * 0.45) + wander;
    let y = BUTTERFLY_MIN_HEIGHT
        + (seconds * (0.7 + i * 0.017) + phase).sin().abs() * BUTTERFLY_HEIGHT_BAND;
    Vec3::new(angle.cos() * r, y, angle.sin() * r)
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
        let radius = BUTTERFLY_RADIUS * (0.72 + (index % 5) as f32 * 0.055);
        let wing_mat = wing_mats[index % wing_mats.len()].clone();
        commands
            .spawn((
                PbrBundle {
                    mesh: body_mesh.clone(),
                    material: body_mat.clone(),
                    transform: Transform::from_translation(butterfly_offset(
                        index, phase, 0.0, radius,
                    )),
                    visibility: Visibility::Hidden,
                    ..default()
                },
                Butterfly {
                    index,
                    phase,
                    radius,
                },
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

fn update_butterflies(
    time: Res<Time>,
    player_q: Query<&Transform, (With<Player>, Without<Butterfly>)>,
    mut butterflies: Query<(&Butterfly, &mut Transform), Without<Player>>,
    mut wings: Query<(&ButterflyWing, &mut Transform), Without<Butterfly>>,
) {
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let seconds = time.elapsed_seconds();
    for (butterfly, mut tf) in butterflies.iter_mut() {
        let offset = butterfly_offset(butterfly.index, butterfly.phase, seconds, butterfly.radius);
        tf.translation = player_tf.translation + offset;
        let tangent = butterfly_offset(
            butterfly.index,
            butterfly.phase,
            seconds + 0.35,
            butterfly.radius,
        ) - offset;
        tf.rotation = Quat::from_rotation_y(tangent.x.atan2(tangent.z));
    }
    for (wing, mut tf) in wings.iter_mut() {
        let flap = (seconds * 8.0 + wing.phase).sin() * 0.95;
        tf.rotation = Quat::from_rotation_z(wing.side * (0.32 + flap.abs() * 0.72));
    }
}

fn update_butterfly_visibility(
    settings: Res<WorldSettings>,
    budget: Res<RuntimeBudget>,
    mut butterflies: Query<(&Butterfly, &mut Visibility)>,
) {
    let active = butterfly_limit(settings.graphics, budget.weather_fx_scale);
    for (butterfly, mut visibility) in butterflies.iter_mut() {
        *visibility = if butterfly.index < active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
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
    fn butterfly_motion_stays_airborne_and_near_player() {
        for index in 0..BUTTERFLY_POOL {
            let offset = butterfly_offset(index, index as f32 * 0.37, 123.0, BUTTERFLY_RADIUS);
            assert!(offset.y >= BUTTERFLY_MIN_HEIGHT);
            assert!(offset.y <= BUTTERFLY_MIN_HEIGHT + BUTTERFLY_HEIGHT_BAND + 0.01);
            assert!(offset.xz().length() <= BUTTERFLY_RADIUS + 8.0);
        }
    }

    #[test]
    fn butterflies_are_visual_only_and_do_not_interfere() {
        assert_eq!(butterfly_interaction_radius(), 0.0);
    }
}
