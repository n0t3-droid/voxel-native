//! Player — first-person camera with gravity, walking, jumping and
//! block-aware collision. `F` toggles fly mode (useful for exploring).
//!
//! Port target: `components/Player.tsx` + `lib/voxel/physics.ts`.

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};

use crate::world::{ChunkAnchor, VoxelWorld};

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_player).add_systems(
            Update,
            (
                grab_cursor,
                update_look,
                update_movement,
            )
                .chain(),
        );
    }
}

#[derive(Component)]
pub struct Player {
    pub yaw: f32,
    pub pitch: f32,
    pub velocity: Vec3,
    pub on_ground: bool,
    pub flying: bool,
    pub walk_speed: f32,
    pub fly_speed: f32,
    pub sensitivity: f32,
}

/// Standard Minecraft-ish hitbox: 0.6×1.8×0.6 blocks, eyes at 1.62.
pub const PLAYER_HALF_WIDTH: f32 = 0.3;
pub const PLAYER_HEIGHT: f32 = 1.8;
pub const PLAYER_EYE_HEIGHT: f32 = 1.62;

fn spawn_player(mut commands: Commands) {
    commands.spawn((
        Camera3dBundle {
            transform: Transform::from_xyz(0.0, 120.0, 0.0),
            ..default()
        },
        Player {
            yaw: 0.0,
            pitch: -0.25,
            velocity: Vec3::ZERO,
            on_ground: false,
            flying: true, // start flying so you can find the ground visually
            walk_speed: 5.5,
            fly_speed: 24.0,
            sensitivity: 0.0025,
        },
        ChunkAnchor,
    ));
}

fn grab_cursor(
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

fn update_look(
    mut motion_evr: EventReader<MouseMotion>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut query: Query<(&mut Transform, &mut Player)>,
) {
    let Ok((mut transform, mut player)) = query.get_single_mut() else {
        return;
    };

    let cursor_locked = windows
        .get_single()
        .map(|w| w.cursor.grab_mode == CursorGrabMode::Locked)
        .unwrap_or(false);

    if cursor_locked {
        for ev in motion_evr.read() {
            player.yaw -= ev.delta.x * player.sensitivity;
            player.pitch =
                (player.pitch - ev.delta.y * player.sensitivity).clamp(-1.54, 1.54);
        }
    } else {
        motion_evr.clear();
    }

    transform.rotation =
        Quat::from_axis_angle(Vec3::Y, player.yaw) * Quat::from_axis_angle(Vec3::X, player.pitch);
}

fn update_movement(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    world: Res<VoxelWorld>,
    mut query: Query<(&mut Transform, &mut Player)>,
) {
    let Ok((mut transform, mut player)) = query.get_single_mut() else {
        return;
    };
    let dt = time.delta_seconds().min(1.0 / 20.0); // clamp long frames

    if keys.just_pressed(KeyCode::KeyF) {
        player.flying = !player.flying;
        player.velocity.y = 0.0;
    }

    // Horizontal input vector in camera yaw frame.
    let yaw_rot = Quat::from_axis_angle(Vec3::Y, player.yaw);
    let forward_h = yaw_rot * -Vec3::Z;
    let right_h = yaw_rot * Vec3::X;

    let mut wish = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        wish += forward_h;
    }
    if keys.pressed(KeyCode::KeyS) {
        wish -= forward_h;
    }
    if keys.pressed(KeyCode::KeyA) {
        wish -= right_h;
    }
    if keys.pressed(KeyCode::KeyD) {
        wish += right_h;
    }
    if wish.length_squared() > 0.0 {
        wish = wish.normalize();
    }

    let sprint = keys.pressed(KeyCode::ControlLeft);
    let speed = if player.flying {
        if sprint {
            player.fly_speed * 2.5
        } else {
            player.fly_speed
        }
    } else if sprint {
        player.walk_speed * 1.6
    } else {
        player.walk_speed
    };

    if player.flying {
        // Direct velocity in fly mode (no gravity, no friction).
        player.velocity.x = wish.x * speed;
        player.velocity.z = wish.z * speed;
        player.velocity.y = 0.0;
        if keys.pressed(KeyCode::Space) {
            player.velocity.y += speed;
        }
        if keys.pressed(KeyCode::ShiftLeft) {
            player.velocity.y -= speed;
        }
    } else {
        // Ground movement + gravity.
        let target = wish * speed;
        let accel = 40.0;
        player.velocity.x += (target.x - player.velocity.x) * (accel * dt).min(1.0);
        player.velocity.z += (target.z - player.velocity.z) * (accel * dt).min(1.0);
        player.velocity.y -= 28.0 * dt; // gravity
        if keys.just_pressed(KeyCode::Space) && player.on_ground {
            player.velocity.y = 9.2;
        }
    }

    // Integrate with per-axis collision (move X, then Y, then Z so sliding
    // against walls works correctly).
    let mut pos = transform.translation;
    let mut grounded = false;

    let delta = player.velocity * dt;
    pos.x = move_axis(pos, delta.x, Axis::X, &world);
    let new_y = move_axis(pos, delta.y, Axis::Y, &world);
    if delta.y <= 0.0 && (new_y - pos.y).abs() < delta.y.abs() - 1e-4 {
        // We hit ground while moving down.
        grounded = true;
        player.velocity.y = 0.0;
    } else if delta.y > 0.0 && (new_y - pos.y).abs() < delta.y.abs() - 1e-4 {
        // Bumped our head.
        player.velocity.y = 0.0;
    }
    pos.y = new_y;
    pos.z = move_axis(pos, delta.z, Axis::Z, &world);

    // Kill horizontal velocity when collision clamped movement (otherwise the
    // player would keep accelerating into a wall).
    let moved_x = pos.x - transform.translation.x;
    let moved_z = pos.z - transform.translation.z;
    if moved_x.abs() < delta.x.abs() - 1e-4 {
        player.velocity.x = 0.0;
    }
    if moved_z.abs() < delta.z.abs() - 1e-4 {
        player.velocity.z = 0.0;
    }

    transform.translation = pos;
    player.on_ground = grounded;
}

#[derive(Copy, Clone)]
enum Axis {
    X,
    Y,
    Z,
}

/// Move one axis, stopping at the first block the player's AABB collides with.
fn move_axis(pos: Vec3, delta: f32, axis: Axis, world: &VoxelWorld) -> f32 {
    if delta == 0.0 {
        return match axis {
            Axis::X => pos.x,
            Axis::Y => pos.y,
            Axis::Z => pos.z,
        };
    }

    let target = match axis {
        Axis::X => pos.x + delta,
        Axis::Y => pos.y + delta,
        Axis::Z => pos.z + delta,
    };

    // Build the AABB at the target position.
    let (min, max) = player_aabb(Vec3::new(
        if matches!(axis, Axis::X) { target } else { pos.x },
        if matches!(axis, Axis::Y) { target } else { pos.y },
        if matches!(axis, Axis::Z) { target } else { pos.z },
    ));

    // Scan every block cell overlapped by the new AABB. If any is solid, we
    // clamp to the nearest block boundary along the axis of motion.
    let x0 = min.x.floor() as i32;
    let x1 = (max.x - 1e-4).floor() as i32;
    let y0 = min.y.floor() as i32;
    let y1 = (max.y - 1e-4).floor() as i32;
    let z0 = min.z.floor() as i32;
    let z1 = (max.z - 1e-4).floor() as i32;

    for bx in x0..=x1 {
        for by in y0..=y1 {
            for bz in z0..=z1 {
                if world.is_solid(bx, by, bz) {
                    // Clamp to the face of this block along the moving axis.
                    return match axis {
                        Axis::X => {
                            if delta > 0.0 {
                                (bx as f32) - PLAYER_HALF_WIDTH - 1e-3
                            } else {
                                (bx as f32) + 1.0 + PLAYER_HALF_WIDTH + 1e-3
                            }
                        }
                        Axis::Y => {
                            if delta > 0.0 {
                                (by as f32) - PLAYER_HEIGHT - 1e-3
                            } else {
                                (by as f32) + 1.0 + 1e-3
                            }
                        }
                        Axis::Z => {
                            if delta > 0.0 {
                                (bz as f32) - PLAYER_HALF_WIDTH - 1e-3
                            } else {
                                (bz as f32) + 1.0 + PLAYER_HALF_WIDTH + 1e-3
                            }
                        }
                    };
                }
            }
        }
    }
    target
}

/// Player AABB. `pos` is the player's FEET position (world-space). Eye
/// height is `pos.y + PLAYER_EYE_HEIGHT`, which matches the camera
/// transform since Bevy's camera is at the transform origin — so we model
/// the camera position AS the eye, and derive the feet from it.
fn player_aabb(camera_pos: Vec3) -> (Vec3, Vec3) {
    let feet = Vec3::new(camera_pos.x, camera_pos.y - PLAYER_EYE_HEIGHT, camera_pos.z);
    let min = Vec3::new(
        feet.x - PLAYER_HALF_WIDTH,
        feet.y,
        feet.z - PLAYER_HALF_WIDTH,
    );
    let max = Vec3::new(
        feet.x + PLAYER_HALF_WIDTH,
        feet.y + PLAYER_HEIGHT,
        feet.z + PLAYER_HALF_WIDTH,
    );
    (min, max)
}
