//! Player — first-person camera with gravity, walking, jumping and
//! block-aware collision. `F` toggles fly mode (useful for exploring).
//!
//! Port target: `components/Player.tsx` + `lib/voxel/physics.ts`.

use bevy::input::mouse::MouseMotion;
use bevy::pbr::{FogFalloff, FogSettings};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};

use crate::settings::WorldSettings;
use crate::world::{ChunkAnchor, VoxelWorld};

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_player).add_systems(
            Update,
            (
                grab_cursor,
                update_look,
                place_on_surface_once,
                update_movement,
                update_camera_fov,
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
    /// Once we've loaded the chunk under the spawn position we teleport
    /// the player onto the terrain surface. Set to `true` after the first
    /// successful placement so it doesn't repeat.
    pub placed_on_surface: bool,
    /// Remaining window (seconds) during which a queued jump press will
    /// fire as soon as we touch ground — makes jumps feel instant even if
    /// pressed a frame before landing.
    pub jump_buffer: f32,
    /// Remaining window (seconds) during which we are allowed to jump
    /// after walking off a ledge — classic platformer "coyote time".
    pub coyote_time: f32,
    /// Smoothed FOV bonus applied on top of `settings.fov_deg` — pushed
    /// up while sprinting for a kinetic speed-rush feel.
    pub fov_bonus: f32,
}

/// Standard Minecraft-ish hitbox: 0.6×1.8×0.6 blocks, eyes at 1.62.
pub const PLAYER_HALF_WIDTH: f32 = 0.3;
pub const PLAYER_HEIGHT: f32 = 1.8;
pub const PLAYER_EYE_HEIGHT: f32 = 1.62;

fn spawn_player(mut commands: Commands) {
    commands.spawn((
        Camera3dBundle {
            transform: Transform::from_xyz(0.0, 120.0, 0.0),
            projection: Projection::Perspective(PerspectiveProjection {
                fov: 75.0f32.to_radians(),
                ..default()
            }),
            ..default()
        },
        FogSettings {
            color: Color::srgba(0.53, 0.80, 0.98, 1.0),
            falloff: FogFalloff::Linear {
                start: 10_000.0,
                end: 10_000.0,
            },
            ..default()
        },
        Player {
            yaw: 0.0,
            pitch: -0.25,
            velocity: Vec3::ZERO,
            on_ground: false,
            flying: true, // start flying so terrain has time to stream in
            walk_speed: 5.5,
            fly_speed: 24.0,
            sensitivity: 0.0025,
            placed_on_surface: false,
            jump_buffer: 0.0,
            coyote_time: 0.0,
            fov_bonus: 0.0,
        },
        ChunkAnchor,
    ));
}

fn update_camera_fov(
    settings: Res<WorldSettings>,
    mut q: Query<(&mut Projection, &Player)>,
) {
    if let Ok((mut proj, player)) = q.get_single_mut() {
        if let Projection::Perspective(ref mut persp) = *proj {
            let base = settings.fov_deg.clamp(30.0, 120.0);
            persp.fov = (base + player.fov_bonus).clamp(30.0, 140.0).to_radians();
        }
    }
}

fn grab_cursor(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    editor: Option<Res<crate::editor::EditorState>>,
) {
    let Ok(mut window) = windows.get_single_mut() else {
        return;
    };
    // Don't grab while the editor panel is open.
    let editor_open = editor.map(|e| e.open).unwrap_or(false);
    if !editor_open && mouse.just_pressed(MouseButton::Left) {
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

    // Sprint FOV kick -- smoothly push FOV a few degrees up while sprinting
    // and actually moving, then ease back when you stop.
    let is_moving = wish.length_squared() > 0.001;
    let target_fov_bonus = if sprint && is_moving && !player.flying { 7.0 } else { 0.0 };
    let fov_lerp = (dt * 10.0).min(1.0);
    player.fov_bonus += (target_fov_bonus - player.fov_bonus) * fov_lerp;

    // Jump buffer + coyote time -- queue jumps and allow grace jumps after
    // walking off ledges so input always feels instant.
    if keys.just_pressed(KeyCode::Space) {
        player.jump_buffer = 0.15;
    }
    player.jump_buffer = (player.jump_buffer - dt).max(0.0);
    if player.on_ground {
        player.coyote_time = 0.12;
    } else {
        player.coyote_time = (player.coyote_time - dt).max(0.0);
    }

    // If the world hasn't streamed a chunk around the player yet, freeze
    // gravity + collision so we don't fall infinitely through AIR.
    let world_ready = world.is_column_loaded(
        transform.translation.x.floor() as i32,
        transform.translation.z.floor() as i32,
    );

    if player.flying || !world_ready {
        // Direct velocity in fly mode (or while world streams in).
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
        // Ground movement + asymmetric gravity (falls faster than it rises
        // -- gives the jump that snappy platformer feel).
        let target = wish * speed;
        let accel = 40.0;
        player.velocity.x += (target.x - player.velocity.x) * (accel * dt).min(1.0);
        player.velocity.z += (target.z - player.velocity.z) * (accel * dt).min(1.0);
        let gravity = if player.velocity.y > 0.0 { 28.0 } else { 40.0 };
        player.velocity.y -= gravity * dt;
        // Terminal velocity so we can never punch through terrain in a frame.
        if player.velocity.y < -55.0 {
            player.velocity.y = -55.0;
        }
        // Instant jump if we have a buffered press AND are grounded (or
        // still within the coyote-time window).
        if player.jump_buffer > 0.0 && player.coyote_time > 0.0 && player.velocity.y <= 1.0 {
            player.velocity.y = 9.6;
            player.jump_buffer = 0.0;
            player.coyote_time = 0.0;
            player.on_ground = false;
        }
    }

    // Auto-unstuck: if the camera is somehow inside solid terrain (e.g. we
    // just landed on a freshly-generated chunk) push straight up until clear.
    // Only runs outside fly mode — while flying, clipping through blocks is
    // fine.
    if !player.flying && world_ready {
        let mut safety = 0;
        while safety < 32 && aabb_overlaps_solid(transform.translation, &world) {
            transform.translation.y += 0.25;
            safety += 1;
        }
    }

    // Integrate with per-axis collision (move X, then Y, then Z so sliding
    // against walls works correctly).
    let mut pos = transform.translation;
    let mut grounded = false;

    let delta = player.velocity * dt;

    let (new_x, hit_x) = move_axis(pos, delta.x, Axis::X, &world);
    pos.x = new_x;
    if hit_x {
        player.velocity.x = 0.0;
    }

    let (new_y, hit_y) = move_axis(pos, delta.y, Axis::Y, &world);
    pos.y = new_y;
    if hit_y {
        if delta.y <= 0.0 {
            grounded = true;
        }
        player.velocity.y = 0.0;
    }

    let (new_z, hit_z) = move_axis(pos, delta.z, Axis::Z, &world);
    pos.z = new_z;
    if hit_z {
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

/// Move one axis, stopping at the first block the player's AABB collides
/// with. Returns the resulting coordinate along `axis` and whether a
/// collision clamped the movement.
fn move_axis(pos: Vec3, delta: f32, axis: Axis, world: &VoxelWorld) -> (f32, bool) {
    let current = match axis {
        Axis::X => pos.x,
        Axis::Y => pos.y,
        Axis::Z => pos.z,
    };
    if delta == 0.0 {
        return (current, false);
    }

    let target = current + delta;

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
                    // Camera = eye. Feet = camera.y - EYE_HEIGHT.
                    // AABB min.y = camera.y - EYE_HEIGHT
                    // AABB max.y = camera.y + (HEIGHT - EYE_HEIGHT)
                    let clamped = match axis {
                        Axis::X => {
                            if delta > 0.0 {
                                (bx as f32) - PLAYER_HALF_WIDTH - 1e-3
                            } else {
                                (bx as f32) + 1.0 + PLAYER_HALF_WIDTH + 1e-3
                            }
                        }
                        Axis::Y => {
                            if delta > 0.0 {
                                // Head hits block bottom (y = by).
                                (by as f32) - (PLAYER_HEIGHT - PLAYER_EYE_HEIGHT) - 1e-3
                            } else {
                                // Feet land on block top (y = by + 1).
                                (by as f32) + 1.0 + PLAYER_EYE_HEIGHT + 1e-3
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
                    return (clamped, true);
                }
            }
        }
    }
    (target, false)
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

/// Does the player's AABB at `camera_pos` overlap any solid block? Used
/// for the auto-unstuck nudge.
fn aabb_overlaps_solid(camera_pos: Vec3, world: &VoxelWorld) -> bool {
    let (min, max) = player_aabb(camera_pos);
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
                    return true;
                }
            }
        }
    }
    false
}

/// Runs every frame until the chunk under the spawn position has streamed
/// in — then drops the player onto the terrain surface and disables
/// fly-mode so gameplay can start.
fn place_on_surface_once(
    world: Res<VoxelWorld>,
    mut query: Query<(&mut Transform, &mut Player)>,
) {
    let Ok((mut transform, mut player)) = query.get_single_mut() else {
        return;
    };
    if player.placed_on_surface {
        return;
    }
    let wx = transform.translation.x.floor() as i32;
    let wz = transform.translation.z.floor() as i32;
    if !world.is_column_loaded(wx, wz) {
        return;
    }
    let surface_y = world.surface_height_at(wx, wz);
    // Put the camera 2 blocks above the surface so gravity settles us
    // cleanly onto the top face without clipping.
    transform.translation.y = (surface_y as f32) + 2.0 + PLAYER_EYE_HEIGHT;
    player.velocity = Vec3::ZERO;
    player.placed_on_surface = true;
    player.flying = false;
    player.on_ground = false;
}
