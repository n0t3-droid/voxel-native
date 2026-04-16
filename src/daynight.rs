//! Day/night cycle — moves a directional "sun" light around the player,
//! swings sky colour + fog between day and night, and drops intensity at
//! dawn/dusk. Port target: the `DayNightCycle` component from
//! `components/VoxelEngine.tsx`.

use bevy::prelude::*;

use crate::settings::{TimeMode, WorldSettings};

pub struct DayNightPlugin;

impl Plugin for DayNightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_sun)
            .add_systems(Update, (advance_time, update_sun).chain());
    }
}

#[derive(Component)]
pub struct Sun;

fn spawn_sun(mut commands: Commands) {
    commands.spawn((
        DirectionalLightBundle {
            directional_light: DirectionalLight {
                illuminance: 10_000.0,
                shadows_enabled: true,
                ..default()
            },
            transform: Transform::from_xyz(50.0, 200.0, 50.0).looking_at(Vec3::ZERO, Vec3::Y),
            ..default()
        },
        Sun,
    ));

    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.7, 0.8, 1.0),
        brightness: 200.0,
    });
}

fn advance_time(time: Res<Time>, mut settings: ResMut<WorldSettings>) {
    if settings.time_mode == TimeMode::Cycle {
        settings.time_of_day =
            (settings.time_of_day + settings.cycle_speed * time.delta_seconds() * 60.0) % 24.0;
    }
}

fn update_sun(
    settings: Res<WorldSettings>,
    mut clear_color: ResMut<ClearColor>,
    mut ambient: ResMut<AmbientLight>,
    mut sun: Query<(&mut Transform, &mut DirectionalLight), With<Sun>>,
) {
    let Ok((mut transform, mut light)) = sun.get_single_mut() else {
        return;
    };

    // hour in radians, noon = π/2
    let t = (settings.time_of_day / 24.0) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
    let sun_dir = Vec3::new(t.cos(), t.sin(), 0.3).normalize();

    // Directional lights in Bevy shine along their -Z. Orient so -Z == -sun_dir.
    let forward = -sun_dir;
    *transform = Transform::from_xyz(sun_dir.x * 400.0, sun_dir.y * 400.0, sun_dir.z * 400.0)
        .looking_to(forward, Vec3::Y);

    // Day factor 0..1 where 1 = high noon, 0 = deep night.
    let day = sun_dir.y.max(0.0);
    light.illuminance = 1_500.0 + day * 10_000.0;

    // Ambient gets a cool tint at night, warm at sunrise/sunset.
    let sunset = (1.0 - (sun_dir.y.abs()).clamp(0.0, 1.0)).powf(3.0);
    let day_color = Color::srgb(0.7, 0.82, 1.0).to_linear();
    let night_color = Color::srgb(0.10, 0.12, 0.22).to_linear();
    let sunset_color = Color::srgb(1.0, 0.55, 0.35).to_linear();

    let base = if day > 0.0 { day_color } else { night_color };
    let amb_lin = base.mix(&sunset_color, sunset * 0.5);
    ambient.color = Color::LinearRgba(amb_lin);
    ambient.brightness = 100.0 + day * 250.0;

    // Sky (clear colour) interpolates similarly.
    let sky_day = Color::srgb(0.53, 0.80, 0.98).to_linear();
    let sky_night = Color::srgb(0.02, 0.04, 0.10).to_linear();
    let sky = sky_night.mix(&sky_day, day);
    let sky = sky.mix(&sunset_color, sunset * 0.35);
    clear_color.0 = Color::LinearRgba(sky);
}
