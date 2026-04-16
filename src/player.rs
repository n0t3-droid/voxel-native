//! First-person fly-camera.
//!
//! Port target: `components/Player.tsx` + `lib/voxel/physics.ts`.
//! Scaffold = a fly camera (WASD + mouse look, Space/Shift for up/down,
//! no gravity/collision yet). Gravity, jumping and block collision will
//! land once the world streams around the player.

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_camera)
            .add_systems(Update, (grab_cursor_on_click, fly_camera));
    }
}

#[derive(Component)]
struct FlyCamera {
    yaw: f32,
    pitch: f32,
    speed: f32,
    sensitivity: f32,
}

fn spawn_camera(mut commands: Commands) {
    commands.spawn((
        Camera3dBundle {
            transform: Transform::from_xyz(0.0, 80.0, 0.0)
                .looking_at(Vec3::new(10.0, 60.0, 10.0), Vec3::Y),
            ..default()
        },
        FlyCamera {
            yaw: 0.0,
            pitch: -0.3,
            speed: 20.0,
            sensitivity: 0.0025,
        },
    ));
}

fn grab_cursor_on_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    let Ok(mut window) = windows.get_single_mut() else {
        return;
    };

    if mouse.just_pressed(MouseButton::Left) {
        window.cursor.grab_mode = CursorGrabMode::Locked;
        window.cursor.visible = false;
    }

    if keys.just_pressed(KeyCode::Escape) {
        window.cursor.grab_mode = CursorGrabMode::None;
        window.cursor.visible = true;
    }
}

fn fly_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut motion_evr: EventReader<MouseMotion>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut query: Query<(&mut Transform, &mut FlyCamera)>,
) {
    let Ok((mut transform, mut cam)) = query.get_single_mut() else {
        return;
    };

    // Only look around while the cursor is locked.
    let cursor_locked = windows
        .get_single()
        .map(|w| w.cursor.grab_mode == CursorGrabMode::Locked)
        .unwrap_or(false);

    if cursor_locked {
        for ev in motion_evr.read() {
            cam.yaw -= ev.delta.x * cam.sensitivity;
            cam.pitch = (cam.pitch - ev.delta.y * cam.sensitivity)
                .clamp(-1.54, 1.54);
        }
    } else {
        motion_evr.clear();
    }

    transform.rotation =
        Quat::from_axis_angle(Vec3::Y, cam.yaw) * Quat::from_axis_angle(Vec3::X, cam.pitch);

    let forward = *transform.forward();
    let right = *transform.right();
    let mut dir = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        dir += forward;
    }
    if keys.pressed(KeyCode::KeyS) {
        dir -= forward;
    }
    if keys.pressed(KeyCode::KeyA) {
        dir -= right;
    }
    if keys.pressed(KeyCode::KeyD) {
        dir += right;
    }
    if keys.pressed(KeyCode::Space) {
        dir += Vec3::Y;
    }
    if keys.pressed(KeyCode::ShiftLeft) {
        dir -= Vec3::Y;
    }

    if dir.length_squared() > 0.0 {
        let boost = if keys.pressed(KeyCode::ControlLeft) {
            3.0
        } else {
            1.0
        };
        transform.translation += dir.normalize() * cam.speed * boost * time.delta_seconds();
    }
}
