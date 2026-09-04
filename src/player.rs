//! Player — first-person camera with gravity, walking, jumping and
//! block-aware collision. `F` toggles fly mode (useful for exploring).
//!
//! Port target: `components/Player.tsx` + `lib/voxel/physics.ts`.

use bevy::core_pipeline::bloom::{BloomCompositeMode, BloomSettings};
use bevy::input::mouse::MouseMotion;
use bevy::pbr::{FogFalloff, FogSettings};
use bevy::prelude::*;
use bevy::render::camera::{CameraOutputMode, Exposure};
use bevy::render::render_resource::BlendState;
use bevy::render::view::{ColorGrading, ColorGradingSection};
use bevy::window::PrimaryWindow;

use crate::daynight::{day_factor, sun_direction, sunset_factor, WorldIntelRuntime};
use crate::neurocore::RuntimeProfile;
use crate::settings::{ActiveWorld, PlayerMiningSave, SuitVitalsSave, WorldSettings};
use crate::weapons::DestructionStats;
use crate::world::{ChunkAnchor, VoxelWorld};

pub struct PlayerPlugin;

/// Latest player mining + suit snapshot for world saves (avoids huge Bevy system param lists).
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct PlayerProgressScratch {
    pub mining: PlayerMiningSave,
    pub suit: SuitVitalsSave,
}

/// Exosuit readouts aligned with the concept HUD (oxygen, laser drill, shield, health).
#[derive(Resource, Debug, Clone)]
pub struct SuitVitals {
    pub health: f32,
    pub shield: f32,
    pub oxygen: f32,
    /// 0–100: weapon/mining beam thermal budget (shown as "laser drill" in HUD).
    pub laser_drill_charge: f32,
}

impl Default for SuitVitals {
    fn default() -> Self {
        Self {
            health: 100.0,
            shield: 60.0,
            oxygen: 97.0,
            laser_drill_charge: 100.0,
        }
    }
}

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SuitVitals::default())
            .insert_resource(PlayerProgressScratch::default())
            .add_systems(Startup, spawn_player)
            .add_systems(
                OnEnter(crate::menu::GameState::InGame),
                load_player_from_world,
            )
            .add_systems(
                Update,
                (
                    hydrate_progress_from_world_save
                        .run_if(in_state(crate::menu::GameState::InGame)),
                    sync_player_progress_scratch.run_if(in_state(crate::menu::GameState::InGame)),
                    update_look.run_if(in_state(crate::menu::GameState::InGame)),
                    place_on_surface_once.run_if(in_state(crate::menu::GameState::InGame)),
                    neon_showcase_warp_input.run_if(in_state(crate::menu::GameState::InGame)),
                    update_movement.run_if(in_state(crate::menu::GameState::InGame)),
                    tick_suit_vitals.run_if(in_state(crate::menu::GameState::InGame)),
                    update_camera_fov,
                    update_bloom_by_graphics,
                    update_cinematic_exposure,
                )
                    .chain(),
            );
    }
}

/// First frame after a real world load: restore mining + suit from `WorldMeta`.
fn hydrate_progress_from_world_save(
    pending: Res<crate::menu::PendingWorldLoad>,
    active: Option<Res<ActiveWorld>>,
    mut stats: ResMut<DestructionStats>,
    mut suit: ResMut<SuitVitals>,
) {
    if !pending.0 {
        return;
    }
    let Some(active) = active else {
        return;
    };
    let m = &active.meta.player_mining;
    stats.luminite_units = m.luminite;
    stats.magnetite_units = m.magnetite;
    stats.iridium_units = m.iridium;
    let s = &active.meta.player_suit;
    suit.health = s.health;
    suit.shield = s.shield;
    suit.oxygen = s.oxygen;
    suit.laser_drill_charge = s.laser_drill_charge;
}

fn sync_player_progress_scratch(
    stats: Res<DestructionStats>,
    suit: Res<SuitVitals>,
    mut scratch: ResMut<PlayerProgressScratch>,
) {
    scratch.mining = PlayerMiningSave {
        luminite: stats.luminite_units,
        magnetite: stats.magnetite_units,
        iridium: stats.iridium_units,
    };
    scratch.suit = SuitVitalsSave {
        health: suit.health,
        shield: suit.shield,
        oxygen: suit.oxygen,
        laser_drill_charge: suit.laser_drill_charge,
    };
}

fn tick_suit_vitals(time: Res<Time>, mut vitals: ResMut<SuitVitals>, player_q: Query<&Player>) {
    let dt = time.delta_seconds();
    let moving = player_q
        .get_single()
        .map(|p| p.velocity.length_squared() > 0.15 * 0.15)
        .unwrap_or(false);
    let drain = if moving { 0.048_f32 } else { 0.024 };
    let gain = 0.041_f32;
    vitals.oxygen = (vitals.oxygen + (gain - drain) * dt).clamp(72.0_f32, 100.0);
    vitals.laser_drill_charge = (vitals.laser_drill_charge + 11.5 * dt).min(100.0);
}

/// When the player enters a world, teleport them to the saved position
/// (from `ActiveWorld`). Skipped when returning from Pause/Options so
/// tweaking settings mid-game doesn't yank the player back to spawn.
fn load_player_from_world(
    active: Option<Res<crate::settings::ActiveWorld>>,
    pending: Res<crate::menu::PendingWorldLoad>,
    _settings: Res<WorldSettings>,
    mut query: Query<(&mut Transform, &mut Player)>,
) {
    if !pending.0 {
        return;
    }
    let Some(active) = active else {
        return;
    };
    let Ok((mut tf, mut player)) = query.get_single_mut() else {
        return;
    };
    let pos = active.meta.player_pos;
    let mut translation = Vec3::new(pos[0], pos[1], pos[2]);
    let mut yaw = active.meta.player_yaw;
    let mut pitch = active.meta.player_pitch;
    let generator = crate::terrain::TerrainGenerator::new(active.meta.seed);
    let bx = crate::chunk::floor_to_i32_safe(translation.x);
    let bz = crate::chunk::floor_to_i32_safe(translation.z);
    let surface = generator.surface_height_at(bx, bz);
    // Only rescue a saved position that is genuinely unusable. Every
    // biome is part of the frontier now, so "you are standing somewhere
    // exotic" is no longer a reason to move the player - being stranded
    // in the void far above the terrain still is.
    if translation.y > surface as f32 + 160.0 || translation.y < 1.0 {
        if let Some(spawn) = generator.find_natural_spawn(bx, bz, 4096) {
            translation = Vec3::new(spawn.x as f32 + 0.5, spawn.y as f32, spawn.z as f32 + 0.5);
            yaw = 0.0;
            pitch = -0.12;
            info!(
                "Recovered stranded world entry to {:?} at {}, {}, {}",
                spawn.biome, spawn.x, spawn.y, spawn.z
            );
        }
    }
    // Keep an authored New World look (scenic yaw is never ~0). Only
    // retarget at the hero crystal when the save has the old default
    // heading, otherwise the mesa-top spawn got overwritten into a
    // tabletop stare and the canyon postcard never appeared.
    if translation.x.abs() < 280.0 && translation.z.abs() < 280.0 && yaw.abs() < 0.05 {
        let dx = crate::frontier::HERO_CRYSTAL_X as f32 + 0.5 - translation.x;
        let dz = crate::frontier::HERO_CRYSTAL_Z as f32 + 0.5 - translation.z;
        yaw = dx.atan2(-dz);
        pitch = -0.22;
    }
    tf.translation = translation;
    player.yaw = yaw;
    player.pitch = pitch;
    player.velocity = Vec3::ZERO;
    // Stream-in takes a moment; keep the player flying until terrain arrives.
    player.flying = true;
    // Placement only runs for fresh worlds (default y = 140 with no custom pos).
    player.placed_on_surface = pos[1] < 200.0 && pos[0].abs() > 0.5;
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
    /// Timer (seconds) used to detect a Space double-tap for fly-toggle.
    /// Counts down from 0.3s after each Space press; if Space is pressed
    /// again while > 0, we toggle fly-mode (Minecraft creative-style).
    pub space_tap_timer: f32,
    /// Same mechanic for W: a double-tap within 0.3s latches a sprint
    /// flag that stays active as long as W is held. This is the classic
    /// Minecraft sprint trigger — lets players sprint without needing
    /// Ctrl (which Windows may intercept for global shortcuts).
    pub w_tap_timer: f32,
    /// True while the W-double-tap sprint latch is active. Cleared when
    /// W is released or the player stops moving forward.
    pub sprint_latched: bool,
    /// Current eye height for crouching/sneaking interpolation
    pub current_eye_height: f32,
    /// Current hitbox height — shrinks to `CROUCH_HEIGHT` while sneaking
    /// so the player actually fits in a 1-block-tall gap and ducks
    /// below a standing shooter's line of fire.
    pub current_height: f32,
}

/// Standard Minecraft-ish hitbox: 0.6×1.8×0.6 blocks, eyes at 1.62.
pub const PLAYER_HALF_WIDTH: f32 = 0.3;
pub const PLAYER_HEIGHT: f32 = 1.8;
pub const PLAYER_EYE_HEIGHT: f32 = 1.62;
/// Hitbox height while sneaking — fits in a 1-block-tall gap, so a
/// standing opponent shooting over a 1-block wall cannot hit us.
pub const CROUCH_HEIGHT: f32 = 1.0;
pub const CROUCH_EYE_HEIGHT: f32 = 0.9;

fn spawn_player(mut commands: Commands) {
    commands.spawn((
        Camera3dBundle {
            // HDR + ACES tonemapping + ClearColorConfig::None lets the
            // separate sky pass (see `sky.rs`, order = -1) show through
            // wherever the world doesn't draw — that's what makes the
            // animated sun, moon and starfield visible behind the
            // streamed terrain. The fog below is unchanged: it still
            // hides the chunk-streaming edge against the sky gradient.
            camera: bevy::prelude::Camera {
                hdr: true,
                // Default composite: sky already blitted, world HDR not
                // cleared so leftover sky pixels show through. Cinematic
                // High switches to a transparent-HDR + alpha blit in
                // `update_cinematic_exposure` so world ColorGrading cannot
                // milk space.
                clear_color: bevy::prelude::ClearColorConfig::None,
                ..default()
            },
            tonemapping: bevy::core_pipeline::tonemapping::Tonemapping::AcesFitted,
            transform: Transform::from_xyz(0.0, 120.0, 0.0),
            projection: Projection::Perspective(PerspectiveProjection {
                fov: 75.0f32.to_radians(),
                ..default()
            }),
            ..default()
        },
        FogSettings {
            color: Color::srgba(0.42, 0.58, 0.78, 0.28),
            // Starting density is a placeholder: `update_sun()` in
            // daynight.rs owns the live ExponentialSquared aerial
            // perspective (thin at noon, warm inscatter at dusk, a
            // lifted fill at night). Keep this conservative so the
            // first frames before that system runs aren't milky.
            falloff: FogFalloff::ExponentialSquared { density: 0.00014 },
            directional_light_color: Color::srgba(0.72, 0.62, 0.48, 0.30),
            directional_light_exponent: 14.0,
            ..default()
        },
        // Bloom on the world camera. Combined with HDR-boosted vertex
        // colours on emissive blocks (lava, crystal, alien moss, glow
        // sand, ice — see `blocks.rs::voxel_color`) this gives every
        // neon block a real halo. Tuned intensity (0.15) and threshold
        // (~1.0 linear via OLD_SCHOOL preset) so non-emissive terrain
        // stays clean while molten channels and crystal spires glow.
        //
        // Bloom cost on integrated GPUs is ~1-2 ms at 720p — acceptable
        // for the visual payoff. Can be gated by GraphicsMode later.
        // Bloom on the world camera. Tuned CONSERVATIVE — only the
        // brightest emissives glow, the rest of the terrain stays
        // readable. Over-bloom was washing out VolcanicWaste vistas
        // and drowning surface detail. Still enough to halo lava
        // rivers, crystal spires and weapon accents without turning
        // the whole screen orange.
        BloomSettings {
            intensity: 0.10,
            low_frequency_boost: 0.35,
            high_pass_frequency: 1.4,
            prefilter_settings: bevy::core_pipeline::bloom::BloomPrefilterSettings {
                threshold: 0.8,
                threshold_softness: 0.4,
            },
            composite_mode: BloomCompositeMode::Additive,
            ..BloomSettings::OLD_SCHOOL
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
            space_tap_timer: 0.0,
            w_tap_timer: 0.0,
            sprint_latched: false,
            current_eye_height: PLAYER_EYE_HEIGHT,
            current_height: PLAYER_HEIGHT,
        },
        ChunkAnchor,
    ));
}

fn update_camera_fov(
    settings: Res<WorldSettings>,
    scope: Res<crate::weapons::ScopeState>,
    mut q: Query<(&mut Projection, &Player)>,
) {
    if let Ok((mut proj, player)) = q.get_single_mut() {
        if let Projection::Perspective(ref mut persp) = *proj {
            let base = settings.fov_deg.clamp(30.0, 120.0);
            let hip = (base + player.fov_bonus).clamp(30.0, 140.0);
            // Dividing by zoom turns a 75° FOV into 12.5° at 6× and 1.25°
            // at 60× (max sniper wheel), giving "telescopic" precision.
            let zoom = scope.current_zoom.max(1.0);
            let target = (hip / zoom).clamp(0.5, 140.0);
            persp.fov = target.to_radians();
        }
    }
}

/// React to GraphicsMode / runtime-profile / time-of-day band changes
/// by scaling bloom. Fast skips the pass entirely. Dusk gets a slightly
/// lusher halo so crystals glow; noon stays conservative so the hull
/// and horizon don't milk.
fn update_bloom_by_graphics(
    settings: Res<WorldSettings>,
    intel: Res<WorldIntelRuntime>,
    mut q: Query<&mut BloomSettings, With<Player>>,
    mut last: Local<Option<(crate::settings::GraphicsMode, RuntimeProfile, u8)>>,
) {
    let sun = sun_direction(settings.time_of_day);
    let dusk = sunset_factor(sun);
    let night = 1.0 - day_factor(sun);
    let band = if night > 0.55 {
        0u8
    } else if dusk > 0.40 {
        1
    } else {
        2
    };
    let key = (settings.graphics, settings.runtime_profile, band);
    if *last == Some(key) && !intel.is_changed() {
        return;
    }
    *last = Some(key);
    let cinematic = settings.runtime_profile == RuntimeProfile::Cinematic;
    let (mut intensity, mut threshold, lf_boost): (f32, f32, f32) = match settings.graphics {
        crate::settings::GraphicsMode::Fast => (0.0, 0.85, 0.20),
        crate::settings::GraphicsMode::Balanced => (0.10, 0.76, 0.28),
        crate::settings::GraphicsMode::High if cinematic => (0.15, 0.78, 0.28),
        crate::settings::GraphicsMode::High => (0.14, 0.70, 0.32),
    };
    if cinematic && settings.graphics != crate::settings::GraphicsMode::Fast {
        intensity += dusk * 0.055;
        threshold -= dusk * 0.07;
    }
    let target = (intensity * intel.profile.bloom_mul).clamp(0.0, 0.22);
    if let Ok(mut b) = q.get_single_mut() {
        b.intensity = target;
        b.low_frequency_boost = lf_boost;
        b.prefilter_settings.threshold = threshold.clamp(0.62, 0.88);
        b.prefilter_settings.threshold_softness = if cinematic { 0.30 } else { 0.28 };
    }
}

/// Cinematic High renders the world HDR buffer independently of the sky
/// pass: transparent clear, grade/tonemap only those pixels, alpha-blit
/// over the already-tonemapped sky. Fast/Balanced keep the legacy
/// uncleared composite so they stay cheap and unchanged.
fn world_pass_splits_from_sky(cinematic: bool, fast: bool) -> bool {
    cinematic && !fast
}

fn configure_world_camera_composite(camera: &mut Camera, split: bool) {
    if split {
        camera.clear_color = ClearColorConfig::Custom(Color::srgba(0.0, 0.0, 0.0, 0.0));
        camera.output_mode = CameraOutputMode::Write {
            blend_state: Some(BlendState::ALPHA_BLENDING),
            clear_color: ClearColorConfig::None,
        };
    } else {
        camera.clear_color = ClearColorConfig::None;
        camera.output_mode = CameraOutputMode::Write {
            blend_state: None,
            clear_color: ClearColorConfig::Default,
        };
    }
}

/// Pre-ACES look-pass for the WORLD camera only.
///
/// Combined-camera EV / shadow-lift washed the night sky grey because
/// leftover sky HDR sat in the uncleared world buffer and got re-graded.
/// Cinematic High now clears that buffer to transparent and alpha-blits,
/// so night lift recovers mesa faces without milking space. Fast stays
/// at the Blender default on the legacy composite.
fn update_cinematic_exposure(
    settings: Res<WorldSettings>,
    mut q: Query<(&mut Camera, &mut ColorGrading, &mut Exposure), With<Player>>,
    mut last: Local<Option<(u8, bool, bool)>>,
) {
    let sun = sun_direction(settings.time_of_day);
    let dusk = sunset_factor(sun);
    let night_amt = (1.0 - day_factor(sun)).powf(1.55);
    let band = if night_amt > 0.55 {
        0u8
    } else if dusk > 0.40 {
        1
    } else {
        2
    };
    let cinematic = settings.runtime_profile == RuntimeProfile::Cinematic;
    let fast = settings.graphics == crate::settings::GraphicsMode::Fast;
    let key = (band, cinematic, fast);
    if *last == Some(key) {
        return;
    }
    *last = Some(key);
    let split = world_pass_splits_from_sky(cinematic, fast);
    let grade = look_pass_grade(night_amt, dusk, cinematic, fast);
    if let Ok((mut camera, mut grading, mut exposure)) = q.get_single_mut() {
        configure_world_camera_composite(&mut camera, split);
        exposure.ev100 = grade.ev100;
        grading.global.exposure = grade.exposure;
        grading.global.temperature = grade.temperature;
        grading.shadows = ColorGradingSection {
            saturation: 1.0,
            contrast: 1.0,
            gamma: grade.shadow_gamma,
            gain: grade.shadow_gain,
            lift: grade.shadow_lift,
        };
        grading.midtones = ColorGradingSection {
            saturation: grade.mid_sat,
            contrast: 1.0,
            gamma: 1.0,
            gain: grade.mid_gain,
            lift: 0.0,
        };
        // Highlights stay neutral so HDR crystals/rivers do not bloom
        // into a white sheet when we open the shadows.
        grading.highlights = ColorGradingSection::default();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LookPassGrade {
    ev100: f32,
    exposure: f32,
    shadow_lift: f32,
    shadow_gain: f32,
    shadow_gamma: f32,
    mid_gain: f32,
    mid_sat: f32,
    temperature: f32,
}

fn look_pass_grade(night_amt: f32, dusk: f32, cinematic: bool, fast: bool) -> LookPassGrade {
    if fast {
        return LookPassGrade {
            ev100: Exposure::EV100_BLENDER,
            exposure: 0.0,
            shadow_lift: 0.0,
            shadow_gain: 1.0,
            shadow_gamma: 1.0,
            mid_gain: 1.0,
            mid_sat: 1.0,
            temperature: 0.0,
        };
    }
    let n = night_amt.clamp(0.0, 1.0);
    let mul = if cinematic { 1.0 } else { 0.55 };
    // Shadow lift + a mild EV drop are world-only. Fast never splits
    // the composite, so those levers stay at identity there.
    let split = world_pass_splits_from_sky(cinematic, fast);
    let night_lift = if split { n * 0.11 * mul } else { 0.0 };
    let night_ev = if split { n * 1.35 * mul } else { 0.0 };
    LookPassGrade {
        ev100: Exposure::EV100_BLENDER - night_ev,
        exposure: dusk * 0.08 * mul,
        shadow_lift: night_lift,
        shadow_gain: 1.0 + n * 0.16 * mul,
        shadow_gamma: 1.0 - n * 0.26 * mul,
        mid_gain: 1.0 + n * 0.12 * mul + dusk * 0.04,
        mid_sat: 1.0 + n * 0.08 + dusk * 0.10,
        temperature: dusk * 0.10,
    }
}

fn update_look(
    time: Res<Time>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut motion_evr: EventReader<MouseMotion>,
    windows: Query<&Window, With<PrimaryWindow>>,
    scope: Res<crate::weapons::ScopeState>,
    mode: Option<Res<crate::mode::ModeContext>>,
    gesture_lock: Option<Res<crate::mode::BuildGestureLock>>,
    agent: Option<Res<crate::agent_control::AgentControlState>>,
    mut query: Query<(&mut Transform, &mut Player)>,
) {
    let Ok((mut transform, mut player)) = query.get_single_mut() else {
        return;
    };

    let cursor_locked = windows
        .get_single()
        .map(crate::mode::cursor_is_captured)
        .unwrap_or(false);

    // When scoped, each pixel of mouse movement should correspond to
    // the same angular sweep as when hip-firing — otherwise tiny flicks
    // rip the crosshair off-target at high zoom. Scaling sensitivity by
    // 1/zoom keeps the feel consistent at every magnification level.
    let sens_scale = 1.0 / scope.current_zoom.max(1.0);

    if mode.as_deref().map(|m| m.is_ship_flight()).unwrap_or(false) {
        return;
    }

    let sketch_orbiting = mode
        .as_deref()
        .and_then(|m| m.build_tool())
        .is_some_and(|tool| tool == crate::toolbelt::ToolbeltTool::DrawRect)
        && mouse.pressed(MouseButton::Right);

    if gesture_lock.as_deref().map(|g| g.active).unwrap_or(false) && !sketch_orbiting {
        motion_evr.clear();
    } else if cursor_locked || sketch_orbiting {
        for ev in motion_evr.read() {
            player.yaw -= ev.delta.x * player.sensitivity * sens_scale;
            player.pitch =
                (player.pitch - ev.delta.y * player.sensitivity * sens_scale).clamp(-1.54, 1.54);
        }
    } else {
        motion_evr.clear();
    }

    if let Some(agent) = agent.as_deref().filter(|agent| agent.active()) {
        let dt = time.delta_seconds().min(0.05);
        if let Some(yaw) = agent.yaw {
            player.yaw = yaw;
        } else {
            player.yaw -= agent.look_x * dt;
        }
        if let Some(pitch) = agent.pitch {
            player.pitch = pitch.clamp(-1.54, 1.54);
        } else {
            player.pitch = (player.pitch - agent.look_y * dt).clamp(-1.54, 1.54);
        }
    }

    transform.rotation =
        Quat::from_axis_angle(Vec3::Y, player.yaw) * Quat::from_axis_angle(Vec3::X, player.pitch);
}

fn update_movement(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    world: Res<VoxelWorld>,
    mode: Option<Res<crate::mode::ModeContext>>,
    agent: Option<Res<crate::agent_control::AgentControlState>>,
    mut query: Query<(&mut Transform, &mut Player)>,
) {
    let Ok((mut transform, mut player)) = query.get_single_mut() else {
        return;
    };
    let dt = time.delta_seconds().min(1.0 / 20.0); // clamp long frames

    if mode.as_deref().map(|m| m.is_ship_flight()).unwrap_or(false) {
        player.velocity = Vec3::ZERO;
        player.flying = true;
        return;
    }

    let agent = agent.as_deref().filter(|agent| agent.active());
    if let Some(agent) = agent {
        if agent.fly {
            player.flying = true;
        }
    }

    let fly_toggle_allowed = mode
        .as_deref()
        .map(|m| {
            matches!(
                m.mode,
                crate::mode::ActiveMode::Combat | crate::mode::ActiveMode::BuildLive { .. }
            )
        })
        .unwrap_or(true);
    if fly_toggle_allowed && keys.just_pressed(KeyCode::KeyF) {
        player.flying = !player.flying;
        player.velocity.y = 0.0;
        player.space_tap_timer = 0.0;
    }

    // Space double-tap toggles fly-mode (Minecraft creative style).
    player.space_tap_timer = (player.space_tap_timer - dt).max(0.0);
    if keys.just_pressed(KeyCode::Space) {
        if player.space_tap_timer > 0.0 {
            player.flying = !player.flying;
            player.velocity.y = 0.0;
            player.space_tap_timer = 0.0;
        } else {
            player.space_tap_timer = 0.3;
        }
    }

    // W double-tap latches sprint (Minecraft-style). Stays on while W is
    // held and released when W is released. This avoids needing Ctrl,
    // which Windows can intercept (Sticky Keys / global shortcuts).
    player.w_tap_timer = (player.w_tap_timer - dt).max(0.0);
    if keys.just_pressed(KeyCode::KeyW) {
        if player.w_tap_timer > 0.0 {
            player.sprint_latched = true;
            player.w_tap_timer = 0.0;
        } else {
            player.w_tap_timer = 0.3;
        }
    }
    if !keys.pressed(KeyCode::KeyW) {
        player.sprint_latched = false;
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
    if let Some(agent) = agent {
        wish += forward_h * agent.forward;
        wish += right_h * agent.right;
    }
    if wish.length_squared() > 1.0 {
        wish = wish.normalize();
    }

    // Sprint is active while EITHER Ctrl is held OR the W double-tap
    // latch is engaged. The latch is the primary mechanism; Ctrl is kept
    // as a fallback for muscle memory.
    let sprint = keys.pressed(KeyCode::ControlLeft)
        || player.sprint_latched
        || agent.map(|agent| agent.sprint).unwrap_or(false);
    let sneak = keys.pressed(KeyCode::ShiftLeft) && !player.flying;

    let speed = if player.flying {
        if sprint {
            player.fly_speed * 2.5
        } else {
            player.fly_speed
        }
    } else if sneak {
        player.walk_speed * 0.35 // Sneak speed: slow and precise
    } else if sprint {
        player.walk_speed * 1.6
    } else {
        player.walk_speed
    };

    // Smooth crouch eye-height transition
    let target_eye_height = if sneak {
        CROUCH_EYE_HEIGHT
    } else {
        PLAYER_EYE_HEIGHT
    };

    // Sprint FOV kick -- smoothly push FOV a few degrees up while sprinting
    // and actually moving, then ease back when you stop.
    let is_moving = wish.length_squared() > 0.001;
    let target_fov_bonus = if sprint && is_moving && !player.flying {
        7.0
    } else {
        0.0
    };
    let fov_lerp = (dt * 10.0).min(1.0);
    player.fov_bonus += (target_fov_bonus - player.fov_bonus) * fov_lerp;

    // Smooth sneaking height interop
    let crouch_lerp = (dt * 15.0).min(1.0);
    let old_eye_height = player.current_eye_height;
    player.current_eye_height += (target_eye_height - player.current_eye_height) * crouch_lerp;

    // Hitbox height tracks the crouch state too: crouched = 1 block,
    // standing = 1.8 blocks. Standing up is BLOCKED if there's a solid
    // block directly above — prevents poking your head into the ceiling
    // and getting stuck or glitching through.
    let want_stand = !sneak;
    let can_stand = if want_stand && player.current_height < PLAYER_HEIGHT - 0.01 {
        let feet_y = transform.translation.y - player.current_eye_height;
        let test_pos = Vec3::new(
            transform.translation.x,
            feet_y + PLAYER_EYE_HEIGHT,
            transform.translation.z,
        );
        !aabb_overlaps_solid(test_pos, &world, PLAYER_EYE_HEIGHT, PLAYER_HEIGHT)
    } else {
        true
    };
    let target_height = if sneak || !can_stand {
        CROUCH_HEIGHT
    } else {
        PLAYER_HEIGHT
    };
    player.current_height += (target_height - player.current_height) * crouch_lerp;

    // Apply crouch visually to camera transform translation
    transform.translation.y += player.current_eye_height - old_eye_height;

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
        if let Some(agent) = agent {
            player.velocity.y += agent.up * speed;
        }
    } else {
        // Ground movement + asymmetric gravity. Tuned for a crisp,
        // Minecraft-style hop (clears exactly one block, short airtime):
        //   - jump velocity 8.6 + rising gravity 34 → peak ≈ 1.09 blocks
        //   - falling gravity 52 → you come down *snappy*, no moon feel
        //   - total airtime ≈ 0.4 s (was 0.56 s) → you no longer carry
        //     3–4 blocks of horizontal distance per jump.
        let target = wish * speed;
        let accel = 40.0;
        player.velocity.x += (target.x - player.velocity.x) * (accel * dt).min(1.0);
        player.velocity.z += (target.z - player.velocity.z) * (accel * dt).min(1.0);
        let gravity = if player.velocity.y > 0.0 { 34.0 } else { 52.0 };
        player.velocity.y -= gravity * dt;
        // Terminal velocity so we can never punch through terrain in a frame.
        if player.velocity.y < -55.0 {
            player.velocity.y = -55.0;
        }
        // Instant jump if we have a buffered press AND are grounded (or
        // still within the coyote-time window).
        if player.jump_buffer > 0.0 && player.coyote_time > 0.0 && player.velocity.y <= 1.0 {
            player.velocity.y = 8.6;
            player.jump_buffer = 0.0;
            player.coyote_time = 0.0;
            player.on_ground = false;
        }
    }

    // Auto-unstuck: if the camera is somehow inside solid terrain (e.g. we
    // just landed on a freshly-generated chunk, or flew down into a cave
    // and toggled fly off) push straight up until clear. Only runs outside
    // fly mode — while flying, clipping through blocks is fine. The old
    // cap of 32×0.25 = 8 blocks was too small to escape a cave ceiling;
    // 512×0.25 = 128 blocks is enough to surface from anywhere.
    if !player.flying && world_ready {
        let mut safety = 0;
        while safety < 512
            && aabb_overlaps_solid(
                transform.translation,
                &world,
                player.current_eye_height,
                player.current_height,
            )
        {
            transform.translation.y += 0.25;
            safety += 1;
        }
    }

    // Integrate with per-axis collision (move X, then Y, then Z so sliding
    // against walls works correctly).
    let mut pos = transform.translation;
    let mut grounded = false;

    let delta = player.velocity * dt;

    let (new_x, hit_x) = move_axis(
        pos,
        delta.x,
        Axis::X,
        &world,
        player.current_eye_height,
        player.current_height,
    );
    pos.x = new_x;
    if hit_x {
        player.velocity.x = 0.0;
    }

    let (new_y, hit_y) = move_axis(
        pos,
        delta.y,
        Axis::Y,
        &world,
        player.current_eye_height,
        player.current_height,
    );
    pos.y = new_y;
    if hit_y {
        if delta.y <= 0.0 {
            grounded = true;
        }
        player.velocity.y = 0.0;
    }

    let (new_z, hit_z) = move_axis(
        pos,
        delta.z,
        Axis::Z,
        &world,
        player.current_eye_height,
        player.current_height,
    );
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
///
/// The function segments large deltas into sub-steps no bigger than
/// `MAX_STEP` (below the player half-width) so that at terminal
/// velocity — e.g. ~2.75 blocks per 50ms frame when gravity spikes —
/// we cannot tunnel through a thin ceiling, floor or wall. Each sub-
/// step picks the block face *closest along the direction of motion*
/// (not just the first one in iteration order) so clamping a diagonal
/// fall against a ledge snaps you to the correct surface.
fn move_axis(
    pos: Vec3,
    delta: f32,
    axis: Axis,
    world: &VoxelWorld,
    eye_height: f32,
    total_height: f32,
) -> (f32, bool) {
    let current = match axis {
        Axis::X => pos.x,
        Axis::Y => pos.y,
        Axis::Z => pos.z,
    };
    if delta == 0.0 {
        return (current, false);
    }
    // PLAYER_HALF_WIDTH is 0.3, so 0.45 is just larger than one block's
    // gap-minus-body (never skips past a thin wall) while avoiding
    // unnecessary iterations for normal walking speeds.
    const MAX_STEP: f32 = 0.45;
    let mut remaining = delta;
    let mut cursor = current;
    loop {
        let step = if remaining.abs() <= MAX_STEP {
            remaining
        } else if remaining > 0.0 {
            MAX_STEP
        } else {
            -MAX_STEP
        };
        let (next, hit) = move_axis_step(
            pos_with(pos, axis, cursor),
            step,
            axis,
            world,
            eye_height,
            total_height,
        );
        cursor = next;
        if hit {
            return (cursor, true);
        }
        remaining -= step;
        if remaining.abs() < 1e-6 {
            break;
        }
    }
    (cursor, false)
}

/// Rebuild a Vec3 with `axis` replaced by `v` — used to thread the
/// segment cursor through a single-axis move.
#[inline]
fn pos_with(pos: Vec3, axis: Axis, v: f32) -> Vec3 {
    match axis {
        Axis::X => Vec3::new(v, pos.y, pos.z),
        Axis::Y => Vec3::new(pos.x, v, pos.z),
        Axis::Z => Vec3::new(pos.x, pos.y, v),
    }
}

/// One bounded sub-step for [`move_axis`]. Scans the AABB overlap at
/// the target and picks the closest block face along `axis` — guarding
/// against the old bug where iteration order (smallest index first)
/// would snap a backwards-moving player to the wrong face.
fn move_axis_step(
    pos: Vec3,
    delta: f32,
    axis: Axis,
    world: &VoxelWorld,
    eye_height: f32,
    total_height: f32,
) -> (f32, bool) {
    let current = match axis {
        Axis::X => pos.x,
        Axis::Y => pos.y,
        Axis::Z => pos.z,
    };
    if delta == 0.0 {
        return (current, false);
    }
    let target = current + delta;
    let (min, max) = player_aabb(
        Vec3::new(
            if matches!(axis, Axis::X) {
                target
            } else {
                pos.x
            },
            if matches!(axis, Axis::Y) {
                target
            } else {
                pos.y
            },
            if matches!(axis, Axis::Z) {
                target
            } else {
                pos.z
            },
        ),
        eye_height,
        total_height,
    );

    let x0 = min.x.floor() as i32;
    let x1 = (max.x - 1e-4).floor() as i32;
    let y0 = min.y.floor() as i32;
    let y1 = (max.y - 1e-4).floor() as i32;
    let z0 = min.z.floor() as i32;
    let z1 = (max.z - 1e-4).floor() as i32;

    // Track the closest-clamped coordinate along `axis` across all
    // overlapping blocks, then return that (not the first one found).
    // For positive delta we want the smallest clamped value (block
    // nearest to `current`), for negative delta the largest.
    let mut best: Option<f32> = None;
    for bx in x0..=x1 {
        for by in y0..=y1 {
            for bz in z0..=z1 {
                if !world.is_solid(bx, by, bz) {
                    continue;
                }
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
                            (by as f32) - (total_height - eye_height) - 1e-3
                        } else {
                            (by as f32) + 1.0 + eye_height + 1e-3
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
                best = Some(match best {
                    None => clamped,
                    Some(prev) => {
                        if delta > 0.0 {
                            prev.min(clamped)
                        } else {
                            prev.max(clamped)
                        }
                    }
                });
            }
        }
    }
    if let Some(c) = best {
        (c, true)
    } else {
        (target, false)
    }
}

/// Player AABB. `pos` is the player's FEET position (world-space). Eye
/// height is `pos.y + eye_height`, which matches the camera
/// transform since Bevy's camera is at the transform origin — so we model
/// the camera position AS the eye, and derive the feet from it.
fn player_aabb(camera_pos: Vec3, eye_height: f32, total_height: f32) -> (Vec3, Vec3) {
    let feet = Vec3::new(camera_pos.x, camera_pos.y - eye_height, camera_pos.z);
    let min = Vec3::new(
        feet.x - PLAYER_HALF_WIDTH,
        feet.y,
        feet.z - PLAYER_HALF_WIDTH,
    );
    let max = Vec3::new(
        feet.x + PLAYER_HALF_WIDTH,
        feet.y + total_height,
        feet.z + PLAYER_HALF_WIDTH,
    );
    (min, max)
}

/// Does the player's AABB at `camera_pos` overlap any solid block? Used
/// for the auto-unstuck nudge.
fn aabb_overlaps_solid(
    camera_pos: Vec3,
    world: &VoxelWorld,
    eye_height: f32,
    total_height: f32,
) -> bool {
    let (min, max) = player_aabb(camera_pos, eye_height, total_height);
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
///
/// Safety net: if no column loads within 30 seconds (e.g. vertical_chunks
/// too small for an exotic spawn y, or terrain generator stalled), we
/// give up waiting, mark the player as placed, and leave them flying.
/// This prevents the system from spin-checking forever and lets the
/// user manually explore instead of being frozen in sky-fly.
fn place_on_surface_once(
    time: Res<Time>,
    world: Res<VoxelWorld>,
    mut query: Query<(&mut Transform, &mut Player)>,
    mut wait_timer: Local<f32>,
) {
    let Ok((mut transform, mut player)) = query.get_single_mut() else {
        return;
    };
    if player.placed_on_surface {
        return;
    }
    let wx = crate::chunk::floor_to_i32_safe(transform.translation.x);
    let wz = crate::chunk::floor_to_i32_safe(transform.translation.z);
    if !world.is_column_loaded(wx, wz) {
        *wait_timer += time.delta_seconds();
        if *wait_timer > 30.0 {
            warn!(
                "place_on_surface_once: column ({wx}, {wz}) not loaded after 30s — giving up and leaving player in fly-mode"
            );
            player.placed_on_surface = true;
        }
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

fn neon_showcase_warp_input(
    keys: Res<ButtonInput<KeyCode>>,
    world: Res<VoxelWorld>,
    mut query: Query<(&mut Transform, &mut Player)>,
) {
    if !wants_neon_showcase_warp(&keys) {
        return;
    }
    let Ok((mut transform, mut player)) = query.get_single_mut() else {
        return;
    };
    let origin_x = crate::chunk::floor_to_i32_safe(transform.translation.x);
    let origin_z = crate::chunk::floor_to_i32_safe(transform.translation.z);
    let Some(spawn) = world
        .generator
        .find_neon_showcase_spawn(origin_x, origin_z, 16_000)
    else {
        warn!(
            "Shift+F9 neon warp: no AlienReef/CrystalSpires showcase found near current position"
        );
        return;
    };

    transform.translation = Vec3::new(spawn.x as f32 + 0.5, spawn.y as f32, spawn.z as f32 + 0.5);
    player.velocity = Vec3::ZERO;
    player.flying = true;
    player.placed_on_surface = true;
    player.yaw = -0.72;
    player.pitch = -0.18;
    transform.rotation =
        Quat::from_axis_angle(Vec3::Y, player.yaw) * Quat::from_axis_angle(Vec3::X, player.pitch);
    info!(
        "Shift+F9 neon warp: {:?} at {}, {}, {}",
        spawn.biome, spawn.x, spawn.y, spawn.z
    );
}

fn wants_neon_showcase_warp(keys: &ButtonInput<KeyCode>) -> bool {
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    shift && keys.just_pressed(KeyCode::F9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f8_is_reserved_for_mode_switch_not_showcase_warp() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::F8);

        assert!(!wants_neon_showcase_warp(&keys));
    }

    #[test]
    fn shift_f9_is_the_explicit_showcase_warp_shortcut() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ShiftLeft);
        keys.press(KeyCode::F9);

        assert!(wants_neon_showcase_warp(&keys));
    }

    #[test]
    fn night_grade_opens_midtones_without_lifting_the_sky_or_fast() {
        let night = look_pass_grade(0.92, 0.05, true, false);
        let noon = look_pass_grade(0.0, 0.0, true, false);
        let dusk = look_pass_grade(0.18, 0.70, true, false);
        let fast = look_pass_grade(0.92, 0.0, true, true);
        assert!(world_pass_splits_from_sky(true, false));
        assert!(!world_pass_splits_from_sky(true, true));
        assert!(!world_pass_splits_from_sky(false, false));
        assert!(night.ev100 < Exposure::EV100_BLENDER - 0.8);
        assert_eq!(noon.ev100, Exposure::EV100_BLENDER);
        assert!(night.shadow_lift > 0.08);
        assert_eq!(noon.shadow_lift, 0.0);
        assert!(night.shadow_gamma < 0.85);
        assert_eq!(noon.shadow_gamma, 1.0);
        assert!(night.mid_gain > noon.mid_gain);
        assert!(dusk.mid_gain < 1.20);
        assert_eq!(fast.ev100, Exposure::EV100_BLENDER);
        assert_eq!(fast.shadow_lift, 0.0);
        assert_eq!(fast.mid_gain, 1.0);
    }
}
