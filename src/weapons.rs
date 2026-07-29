//! Futuristic weapons system — 9 selectable guns with infinite ammo.
//!
//! Hotkeys 1–9 switch between Pistol, Rifle, Sniper, Shotgun, Minigun,
//! Plasma, Blaster, Rocket Launcher and Grenade Launcher. Every shot
//! hit-scans via DDA voxel raycast, then:
//!   * plays a muzzle flash + point-light,
//!   * draws a tracer beam from muzzle to impact,
//!   * spawns voxel debris chunks with gravity + fade-out,
//!   * applies a camera FOV kick for recoil,
//!   * for explosives, breaks a sphere of voxels and spawns an
//!     expanding fireball + strong light flash.
//!
//! All transient entities self-despawn once their life timer hits zero,
//! so full-auto spray cannot accumulate unbounded state.

use bevy::ecs::system::SystemParam;
use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::blocks::{voxel_color, voxel_is_solid, voxel_is_weapon_target, AIR};
use crate::director::UnifiedTelemetry;
use crate::menu::GameState;
use crate::neurocore::{RuntimeBudget, RuntimeProfile};
use crate::player::{Player, PlayerMotionSet};
use crate::settings::WorldSettings;
use crate::world::{VoxelWorld, WorldEditBatch};

// ---------------------------------------------------------------------
// Shared FX asset cache
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ViewmodelMaterialTone {
    DarkBody,
    Gunmetal,
    Chrome,
    Grip,
    Accent,
    Core,
    OpticGlass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WeaponVisualDetail {
    Core,
    Full,
}

impl WeaponVisualDetail {
    fn for_profile(profile: RuntimeProfile) -> Self {
        if profile == RuntimeProfile::LowSpec {
            Self::Core
        } else {
            Self::Full
        }
    }

    fn includes_decorative_parts(self) -> bool {
        self == Self::Full
    }
}
//
// Before this existed, EVERY debris shard, falling block, projectile,
// tracer, impact-puff and explosion allocated a fresh
// `StandardMaterial` (and often a fresh `Mesh`). A single RPG blast
// radius-6 could easily generate 90+ material allocations in one
// frame, each one taking a descriptor-set slot, thrashing GPU
// bind-group caches, and growing `Assets<StandardMaterial>` until the
// FX despawned. Firing sustained Minigun + Shotgun + RPG combat made
// the allocator visible in frame-time traces.
//
// The cache is a per-key lazy store: we hash by `Voxel` for debris /
// falling-block (colour follows block type) and by `WeaponKind` for
// weapon-specific FX (halo, core, tracer, impact puff, warhead).
// Shared meshes (cube sizes, impact-puff sphere, explosion sphere)
// are also cached so every shard after the first is a single hash
// lookup. The functional behaviour is identical — visually the game
// looks the same, the only difference is the allocator graph.
#[derive(Resource, Default)]
pub struct WeaponFxCache {
    debris_mat: std::collections::HashMap<crate::blocks::Voxel, Handle<StandardMaterial>>,
    falling_mat: std::collections::HashMap<crate::blocks::Voxel, Handle<StandardMaterial>>,
    halo_mat: std::collections::HashMap<WeaponKind, Handle<StandardMaterial>>,
    core_mat: std::collections::HashMap<WeaponKind, Handle<StandardMaterial>>,
    tracer_mat: std::collections::HashMap<WeaponKind, Handle<StandardMaterial>>,
    puff_mat: std::collections::HashMap<WeaponKind, Handle<StandardMaterial>>,
    warhead_mat: Option<Handle<StandardMaterial>>,
    explosion_mat: Option<Handle<StandardMaterial>>,
    /// Per-weapon explosion sphere material so Plasma/Grenade blasts
    /// stop allocating a fresh `StandardMaterial` on every detonation.
    /// `update_explosions` only mutates transform/scale (not the
    /// material itself), so sharing one handle per kind is safe.
    explosion_mat_kind: std::collections::HashMap<WeaponKind, Handle<StandardMaterial>>,
    debris_chunk_mesh: Option<Handle<Mesh>>,
    debris_shard_mesh: Option<Handle<Mesh>>,
    debris_dust_mesh: Option<Handle<Mesh>>,
    falling_mesh: Option<Handle<Mesh>>,
    impact_puff_mesh: Option<Handle<Mesh>>,
    explosion_mesh: Option<Handle<Mesh>>,
    /// Shared shockwave-ring mesh. Material can NOT be shared because
    /// `update_shockwaves` mutates emissive/alpha per-instance — but
    /// the geometry never changes, so one handle for every blast.
    shockwave_mesh: Option<Handle<Mesh>>,
    tracer_mesh: std::collections::HashMap<WeaponKind, Handle<Mesh>>,
    halo_mesh: std::collections::HashMap<WeaponKind, Handle<Mesh>>,
    core_mesh: std::collections::HashMap<WeaponKind, Handle<Mesh>>,
    warhead_mesh: std::collections::HashMap<WeaponKind, Handle<Mesh>>,
    viewmodel_cube_mesh: Option<Handle<Mesh>>,
    viewmodel_mats: std::collections::HashMap<
        (ViewmodelMaterialTone, Option<WeaponKind>),
        Handle<StandardMaterial>,
    >,
    /// Pending full-screen flash requests, drained by
    /// `update_screen_flash`. Using a queue (rather than spawning a
    /// fresh `NodeBundle` on the spot) lets us collapse multiple
    /// near-simultaneous explosions onto ONE persistent overlay so a
    /// chain reaction does not stack 3+ alpha layers and blind the
    /// player.
    pending_flashes: Vec<(Vec3, f32, f32)>,
}

impl WeaponFxCache {
    fn viewmodel_cube_shared(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.viewmodel_cube_mesh
            .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 1.0, 1.0)))
            .clone()
    }

    fn viewmodel_mat_for(
        &mut self,
        tone: ViewmodelMaterialTone,
        kind: WeaponKind,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        let keyed_kind = matches!(
            tone,
            ViewmodelMaterialTone::Accent | ViewmodelMaterialTone::Core
        )
        .then_some(kind);
        let key = (tone, keyed_kind);
        if let Some(handle) = self.viewmodel_mats.get(&key) {
            return handle.clone();
        }

        let accent_color = kind.color().to_linear();
        let material = match tone {
            ViewmodelMaterialTone::DarkBody => StandardMaterial {
                base_color: Color::srgb(0.07, 0.08, 0.10),
                perceptual_roughness: 0.42,
                metallic: 0.68,
                ..default()
            },
            ViewmodelMaterialTone::Gunmetal => StandardMaterial {
                base_color: Color::srgb(0.24, 0.27, 0.31),
                perceptual_roughness: 0.26,
                metallic: 0.92,
                ..default()
            },
            ViewmodelMaterialTone::Chrome => StandardMaterial {
                base_color: Color::srgb(0.82, 0.85, 0.92),
                perceptual_roughness: 0.12,
                metallic: 1.0,
                ..default()
            },
            ViewmodelMaterialTone::Grip => StandardMaterial {
                base_color: Color::srgb(0.05, 0.06, 0.08),
                perceptual_roughness: 0.85,
                metallic: 0.1,
                ..default()
            },
            ViewmodelMaterialTone::Accent => StandardMaterial {
                base_color: Color::srgb(0.035, 0.045, 0.06),
                emissive: LinearRgba::rgb(
                    accent_color.red * 3.0,
                    accent_color.green * 3.0,
                    accent_color.blue * 3.0,
                ),
                perceptual_roughness: 0.34,
                metallic: 0.55,
                ..default()
            },
            ViewmodelMaterialTone::Core => StandardMaterial {
                base_color: Color::srgb(
                    (accent_color.red * 0.6).min(1.0),
                    (accent_color.green * 0.6).min(1.0),
                    (accent_color.blue * 0.6).min(1.0),
                ),
                emissive: LinearRgba::rgb(
                    accent_color.red * 14.0 + 1.0,
                    accent_color.green * 14.0 + 1.0,
                    accent_color.blue * 14.0 + 1.0,
                ),
                unlit: true,
                ..default()
            },
            ViewmodelMaterialTone::OpticGlass => StandardMaterial {
                base_color: Color::srgb(0.025, 0.09, 0.11),
                emissive: LinearRgba::rgb(0.12, 0.65, 0.85),
                perceptual_roughness: 0.18,
                metallic: 0.35,
                ..default()
            },
        };
        let handle = materials.add(material);
        self.viewmodel_mats.insert(key, handle.clone());
        handle
    }

    fn debris_mat_for(
        &mut self,
        voxel: crate::blocks::Voxel,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        if let Some(h) = self.debris_mat.get(&voxel) {
            return h.clone();
        }
        let col = voxel_color(voxel);
        let base = Color::srgb(col[0].min(1.0), col[1].min(1.0), col[2].min(1.0));
        let h = materials.add(StandardMaterial {
            base_color: base,
            emissive: LinearRgba::rgb(
                (col[0] * 0.4 + 0.3).min(3.0),
                (col[1] * 0.4 + 0.2).min(3.0),
                (col[2] * 0.4 + 0.15).min(3.0),
            ),
            perceptual_roughness: 0.6,
            metallic: 0.1,
            ..default()
        });
        self.debris_mat.insert(voxel, h.clone());
        h
    }

    fn falling_mat_for(
        &mut self,
        voxel: crate::blocks::Voxel,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        if let Some(h) = self.falling_mat.get(&voxel) {
            return h.clone();
        }
        let col = voxel_color(voxel);
        let h = materials.add(StandardMaterial {
            base_color: Color::srgb(col[0].min(1.0), col[1].min(1.0), col[2].min(1.0)),
            emissive: LinearRgba::rgb(
                (col[0] - 1.0).max(0.0),
                (col[1] - 1.0).max(0.0),
                (col[2] - 1.0).max(0.0),
            ),
            perceptual_roughness: 0.8,
            metallic: 0.05,
            ..default()
        });
        self.falling_mat.insert(voxel, h.clone());
        h
    }

    fn halo_mat_for(
        &mut self,
        kind: WeaponKind,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        if let Some(h) = self.halo_mat.get(&kind) {
            return h.clone();
        }
        let lin = kind.color().to_linear();
        let h = materials.add(StandardMaterial {
            base_color: Color::srgba(lin.red, lin.green, lin.blue, 0.0),
            emissive: LinearRgba::rgb(
                lin.red * 60.0 + 1.5,
                lin.green * 60.0 + 1.5,
                lin.blue * 60.0 + 1.5,
            ),
            unlit: true,
            alpha_mode: AlphaMode::Add,
            ..default()
        });
        self.halo_mat.insert(kind, h.clone());
        h
    }

    fn core_mat_for(
        &mut self,
        kind: WeaponKind,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        if let Some(h) = self.core_mat.get(&kind) {
            return h.clone();
        }
        let lin = kind.color().to_linear();
        let h = materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 1.0, 1.0),
            emissive: LinearRgba::rgb(
                80.0 + lin.red * 14.0,
                80.0 + lin.green * 14.0,
                80.0 + lin.blue * 14.0,
            ),
            unlit: true,
            ..default()
        });
        self.core_mat.insert(kind, h.clone());
        h
    }

    fn tracer_mat_for(
        &mut self,
        kind: WeaponKind,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        if let Some(h) = self.tracer_mat.get(&kind) {
            return h.clone();
        }
        let lin = kind.color().to_linear();
        let h = materials.add(StandardMaterial {
            base_color: Color::srgb(lin.red, lin.green, lin.blue),
            emissive: LinearRgba::rgb(
                lin.red * 14.0 + 2.0,
                lin.green * 14.0 + 2.0,
                lin.blue * 14.0 + 2.0,
            ),
            unlit: true,
            alpha_mode: AlphaMode::Add,
            ..default()
        });
        self.tracer_mat.insert(kind, h.clone());
        h
    }

    fn tracer_mesh_for(&mut self, kind: WeaponKind, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        if let Some(h) = self.tracer_mesh.get(&kind) {
            return h.clone();
        }
        let profile = kind.tracer_fx();
        let h = meshes.add(Cylinder::new(profile.radius, profile.length));
        self.tracer_mesh.insert(kind, h.clone());
        h
    }

    fn puff_mat_for(
        &mut self,
        kind: WeaponKind,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        if let Some(h) = self.puff_mat.get(&kind) {
            return h.clone();
        }
        let lin = kind.color().to_linear();
        let h = materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.85, 0.5),
            emissive: LinearRgba::rgb(
                lin.red * 10.0 + 6.0,
                lin.green * 6.0 + 3.0,
                lin.blue * 8.0 + 1.0,
            ),
            unlit: true,
            ..default()
        });
        self.puff_mat.insert(kind, h.clone());
        h
    }

    fn warhead_mat_shared(
        &mut self,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.warhead_mat
            .get_or_insert_with(|| {
                materials.add(StandardMaterial {
                    base_color: Color::srgb(1.0, 0.8, 0.3),
                    emissive: LinearRgba::rgb(36.0, 21.0, 11.0),
                    unlit: true,
                    ..default()
                })
            })
            .clone()
    }

    fn explosion_mat_shared(
        &mut self,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.explosion_mat
            .get_or_insert_with(|| {
                materials.add(StandardMaterial {
                    base_color: Color::srgb(1.0, 0.6, 0.15),
                    emissive: LinearRgba::rgb(22.0, 9.0, 2.5),
                    unlit: true,
                    alpha_mode: AlphaMode::Add,
                    ..default()
                })
            })
            .clone()
    }

    fn explosion_mat_for(
        &mut self,
        kind: WeaponKind,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        if let Some(h) = self.explosion_mat_kind.get(&kind) {
            return h.clone();
        }
        let profile = kind.explosion_fx();
        let h = materials.add(StandardMaterial {
            base_color: Color::srgb(
                profile.sphere_base_rgb.x,
                profile.sphere_base_rgb.y,
                profile.sphere_base_rgb.z,
            ),
            emissive: LinearRgba::rgb(
                profile.sphere_emissive_rgb.x,
                profile.sphere_emissive_rgb.y,
                profile.sphere_emissive_rgb.z,
            ),
            unlit: true,
            alpha_mode: AlphaMode::Add,
            ..default()
        });
        self.explosion_mat_kind.insert(kind, h.clone());
        h
    }

    fn shockwave_mesh_shared(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.shockwave_mesh
            .get_or_insert_with(|| {
                meshes.add(Cylinder {
                    radius: 1.0,
                    half_height: 0.05,
                })
            })
            .clone()
    }

    fn debris_chunk_mesh_shared(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.debris_chunk_mesh
            .get_or_insert_with(|| meshes.add(Cuboid::new(0.28, 0.28, 0.28)))
            .clone()
    }

    fn debris_shard_mesh_shared(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.debris_shard_mesh
            .get_or_insert_with(|| meshes.add(Cuboid::new(0.16, 0.16, 0.16)))
            .clone()
    }

    fn debris_dust_mesh_shared(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.debris_dust_mesh
            .get_or_insert_with(|| meshes.add(Cuboid::new(0.08, 0.08, 0.08)))
            .clone()
    }

    fn falling_mesh_shared(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.falling_mesh
            .get_or_insert_with(|| meshes.add(Cuboid::new(0.98, 0.98, 0.98)))
            .clone()
    }

    fn impact_puff_mesh_shared(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.impact_puff_mesh
            .get_or_insert_with(|| {
                meshes.add(
                    Sphere::new(0.18)
                        .mesh()
                        .ico(2)
                        .expect("ico subdivision 2 is always valid"),
                )
            })
            .clone()
    }

    fn explosion_mesh_shared(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.explosion_mesh
            .get_or_insert_with(|| {
                meshes.add(
                    Sphere::new(0.35)
                        .mesh()
                        .ico(3)
                        .expect("ico subdivision 3 is always valid"),
                )
            })
            .clone()
    }

    fn halo_mesh_for(&mut self, kind: WeaponKind, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        if let Some(h) = self.halo_mesh.get(&kind) {
            return h.clone();
        }
        let (bolt_len, bolt_radius) = kind.bolt_dims();
        let w = bolt_radius * 3.2;
        let h = meshes.add(Cuboid::new(w, w, bolt_len * 1.15));
        self.halo_mesh.insert(kind, h.clone());
        h
    }

    fn core_mesh_for(&mut self, kind: WeaponKind, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        if let Some(h) = self.core_mesh.get(&kind) {
            return h.clone();
        }
        let (bolt_len, bolt_radius) = kind.bolt_dims();
        let w = bolt_radius * 1.1;
        let h = meshes.add(Cuboid::new(w, w, bolt_len * 0.85));
        self.core_mesh.insert(kind, h.clone());
        h
    }

    fn warhead_mesh_for(&mut self, kind: WeaponKind, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        if let Some(h) = self.warhead_mesh.get(&kind) {
            return h.clone();
        }
        let w = match kind {
            WeaponKind::RocketLauncher => 0.26,
            _ => 0.22,
        };
        let h = meshes.add(Cuboid::new(w, w, w));
        self.warhead_mesh.insert(kind, h.clone());
        h
    }
}

pub struct WeaponsPlugin;

#[derive(SystemParam)]
struct FireControlParams<'w> {
    time: Res<'w, Time>,
    mouse: Res<'w, ButtonInput<MouseButton>>,
    agent: Option<Res<'w, crate::agent_control::AgentControlState>>,
    budget: Res<'w, RuntimeBudget>,
}

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ActiveWeapon::default())
            .insert_resource(ScopeState::default())
            .init_resource::<WeaponHolster>()
            .init_resource::<WeaponFxCache>()
            .init_resource::<CameraShake>()
            .init_resource::<DestructionStats>()
            .init_resource::<HitFeedback>()
            .add_systems(PostStartup, setup_weapon)
            .add_systems(
                Update,
                (
                    switch_weapon.run_if(in_state(GameState::InGame)),
                    scope_input.run_if(in_state(GameState::InGame)),
                    reload_input,
                    (fire_weapon, flush_drill_heat_to_suit)
                        .chain()
                        .after(PlayerMotionSet)
                        .run_if(in_state(GameState::InGame)),
                    animate_viewmodel,
                    update_muzzle_flash,
                    update_tracers,
                    update_projectiles.run_if(in_state(GameState::InGame)),
                    update_debris,
                    update_falling_blocks,
                    update_explosions,
                    update_shockwaves,
                    apply_camera_shake,
                    update_screen_flash,
                    check_bounce_pad.run_if(in_state(GameState::InGame)),
                    decay_hit_feedback,
                    sync_unified_telemetry,
                    cheat_keybinds.run_if(in_state(GameState::InGame)),
                ),
            );
    }
}

fn sync_unified_telemetry(stats: Res<DestructionStats>, mut telemetry: ResMut<UnifiedTelemetry>) {
    telemetry.ground_blocks_broken = stats.blocks_broken;
    telemetry.ground_shots = stats.shots_fired;
    telemetry.luminite_units = stats.luminite_units;
    telemetry.magnetite_units = stats.magnetite_units;
    telemetry.iridium_units = stats.iridium_units;
}

// ---------------------------------------------------------------------
// Fun-boost resources
// ---------------------------------------------------------------------

/// Trauma-based camera shake (Squirrel Eiserloh's shake model). Every
/// hit / explosion stacks trauma onto `trauma`, which decays smoothly
/// and drives pseudo-random yaw / pitch / roll perturbations.
#[derive(Resource, Default)]
pub struct CameraShake {
    pub trauma: f32,
    t: f32,
}

impl CameraShake {
    pub fn add(&mut self, amount: f32) {
        self.trauma = (self.trauma + amount).min(1.0);
    }
}

/// Global destruction / kill counter for the HUD "combo meter".
#[derive(Resource, Default)]
pub struct DestructionStats {
    pub blocks_broken: u64,
    pub shots_fired: u64,
    pub explosions: u64,
    pub combo: u32,
    pub combo_timer: f32,
    /// Mined units toward concept HUD (Luminite / Magnetite / Iridium).
    pub luminite_units: u64,
    pub magnetite_units: u64,
    pub iridium_units: u64,
    /// Accumulated per-frame, applied to `SuitVitals` in `flush_drill_heat_to_suit`.
    pub drill_heat_pending: f32,
}

/// Hitmarker feedback — flashes the crosshair / shows floating "+N".
#[derive(Resource, Default)]
pub struct HitFeedback {
    pub flash_t: f32,
    pub last_hit_blocks: u32,
}

/// Tags the expanding shockwave ring spawned by every explosion.
#[derive(Component)]
pub struct Shockwave {
    pub life: f32,
    pub max_life: f32,
    pub max_scale: f32,
    pub base_rgb: Vec3,
    pub emissive_rgb: Vec3,
}

/// Full-screen flash entity (UI node alpha faded over time).
#[derive(Component)]
pub struct ScreenFlash {
    pub life: f32,
    pub max_life: f32,
    pub rgb: Vec3,
    pub max_alpha: f32,
}

#[derive(Clone, Copy)]
struct ImpactFxProfile {
    puff_life: f32,
    puff_start_scale: f32,
    puff_end_scale: f32,
    halo_life: f32,
    halo_start_scale: f32,
    halo_end_scale: f32,
    light_intensity: f32,
    light_range: f32,
    shake: f32,
}

#[derive(Clone, Copy)]
struct ExplosionFxProfile {
    sphere_base_rgb: Vec3,
    sphere_emissive_rgb: Vec3,
    ring_base_rgb: Vec3,
    ring_emissive_rgb: Vec3,
    light_rgb: Vec3,
    light_intensity: f32,
    flash_rgb: Vec3,
    flash_alpha: f32,
    shake: f32,
    /// Lifetime of the expanding sphere + ground-light in seconds.
    /// Plasma puffs vanish quickly, RPG lingers for the kaboom.
    sphere_life: f32,
    /// Maximum scale of the expanding sphere as a multiple of the
    /// blast radius. > 2.0 means the visible plume reads as larger
    /// than the actual destruction sphere.
    sphere_scale_mul: f32,
}

#[derive(Clone, Copy)]
struct TracerFxProfile {
    length: f32,
    radius: f32,
    life: f32,
}

#[derive(Debug, Clone, Copy)]
struct ViewmodelTuning {
    rest_translation: Vec3,
    muzzle_offset: Vec3,
    recoil_offset: Vec3,
    recoil_pitch: f32,
    muzzle_light_life: f32,
    muzzle_light_range: f32,
}

#[derive(Debug, Clone, Copy)]
struct RifleSilhouette {
    barrel_len: f32,
    optic_len: f32,
    optic_radius: f32,
}

fn viewmodel_recoil_amount(remaining: f32, duration: f32) -> f32 {
    if !remaining.is_finite() || !duration.is_finite() || duration <= 0.0 {
        return 0.0;
    }
    let normalized = (remaining / duration).clamp(0.0, 1.0);
    normalized * normalized * (3.0 - 2.0 * normalized)
}

// ---------------------------------------------------------------------
// Weapon kinds + stats
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WeaponKind {
    Pistol,
    AssaultRifle,
    Sniper,
    Shotgun,
    Minigun,
    PlasmaRifle,
    Blaster,
    RocketLauncher,
    GrenadeLauncher,
}

impl WeaponKind {
    pub const ALL: [WeaponKind; 9] = [
        WeaponKind::Pistol,
        WeaponKind::AssaultRifle,
        WeaponKind::Sniper,
        WeaponKind::Shotgun,
        WeaponKind::Minigun,
        WeaponKind::PlasmaRifle,
        WeaponKind::Blaster,
        WeaponKind::RocketLauncher,
        WeaponKind::GrenadeLauncher,
    ];

    pub fn name(self) -> &'static str {
        match self {
            WeaponKind::Pistol => "Pistol",
            WeaponKind::AssaultRifle => "Rifle",
            WeaponKind::Sniper => "Sniper",
            WeaponKind::Shotgun => "Shotgun",
            WeaponKind::Minigun => "Minigun",
            WeaponKind::PlasmaRifle => "Plasma",
            WeaponKind::Blaster => "Blaster",
            WeaponKind::RocketLauncher => "RPG",
            WeaponKind::GrenadeLauncher => "GLncr",
        }
    }

    pub fn color(self) -> Color {
        match self {
            WeaponKind::Pistol => Color::srgb(1.0, 0.9, 0.4),
            WeaponKind::AssaultRifle => Color::srgb(0.4, 0.9, 1.0),
            WeaponKind::Sniper => Color::srgb(0.2, 1.0, 0.7),
            WeaponKind::Shotgun => Color::srgb(1.0, 0.5, 0.2),
            WeaponKind::Minigun => Color::srgb(1.0, 0.3, 0.3),
            WeaponKind::PlasmaRifle => Color::srgb(0.8, 0.4, 1.0),
            WeaponKind::Blaster => Color::srgb(0.2, 0.8, 1.0),
            WeaponKind::RocketLauncher => Color::srgb(1.0, 0.6, 0.1),
            WeaponKind::GrenadeLauncher => Color::srgb(0.8, 1.0, 0.3),
        }
    }

    fn viewmodel_tuning(self) -> ViewmodelTuning {
        match self {
            WeaponKind::Pistol => ViewmodelTuning {
                rest_translation: Vec3::new(0.17, -0.14, -0.35),
                muzzle_offset: Vec3::new(0.0, 0.008, -0.27),
                recoil_offset: Vec3::new(0.0, 0.012, 0.075),
                recoil_pitch: 0.10,
                muzzle_light_life: 0.065,
                muzzle_light_range: 10.0,
            },
            WeaponKind::AssaultRifle => ViewmodelTuning {
                rest_translation: Vec3::new(0.22, -0.18, -0.52),
                muzzle_offset: Vec3::new(0.0, 0.006, -0.55),
                recoil_offset: Vec3::new(0.0, 0.014, 0.085),
                recoil_pitch: 0.12,
                muzzle_light_life: 0.055,
                muzzle_light_range: 12.0,
            },
            WeaponKind::Sniper => ViewmodelTuning {
                rest_translation: Vec3::new(0.22, -0.16, -0.55),
                muzzle_offset: Vec3::new(0.0, 0.006, -0.71),
                recoil_offset: Vec3::new(0.0, 0.030, 0.18),
                recoil_pitch: 0.22,
                muzzle_light_life: 0.085,
                muzzle_light_range: 18.0,
            },
            WeaponKind::Shotgun => ViewmodelTuning {
                rest_translation: Vec3::new(0.22, -0.18, -0.48),
                muzzle_offset: Vec3::new(0.0, 0.014, -0.58),
                recoil_offset: Vec3::new(0.0, 0.022, 0.17),
                recoil_pitch: 0.18,
                muzzle_light_life: 0.080,
                muzzle_light_range: 16.0,
            },
            WeaponKind::Minigun => ViewmodelTuning {
                rest_translation: Vec3::new(0.26, -0.22, -0.50),
                muzzle_offset: Vec3::new(0.0, 0.005, -0.54),
                recoil_offset: Vec3::new(0.0, 0.008, 0.040),
                recoil_pitch: 0.07,
                muzzle_light_life: 0.045,
                muzzle_light_range: 9.0,
            },
            WeaponKind::PlasmaRifle => ViewmodelTuning {
                rest_translation: Vec3::new(0.22, -0.18, -0.52),
                muzzle_offset: Vec3::new(0.0, 0.0, -0.55),
                recoil_offset: Vec3::new(0.0, 0.016, 0.10),
                recoil_pitch: 0.14,
                muzzle_light_life: 0.070,
                muzzle_light_range: 14.0,
            },
            WeaponKind::Blaster => ViewmodelTuning {
                rest_translation: Vec3::new(0.22, -0.18, -0.52),
                muzzle_offset: Vec3::new(0.0, 0.006, -0.47),
                recoil_offset: Vec3::new(0.0, 0.014, 0.09),
                recoil_pitch: 0.12,
                muzzle_light_life: 0.060,
                muzzle_light_range: 13.0,
            },
            WeaponKind::RocketLauncher => ViewmodelTuning {
                rest_translation: Vec3::new(0.24, -0.20, -0.56),
                muzzle_offset: Vec3::new(0.0, 0.0, -0.65),
                recoil_offset: Vec3::new(0.0, 0.030, 0.18),
                recoil_pitch: 0.22,
                muzzle_light_life: 0.090,
                muzzle_light_range: 18.0,
            },
            WeaponKind::GrenadeLauncher => ViewmodelTuning {
                rest_translation: Vec3::new(0.24, -0.20, -0.56),
                muzzle_offset: Vec3::new(0.0, 0.012, -0.53),
                recoil_offset: Vec3::new(0.0, 0.022, 0.16),
                recoil_pitch: 0.18,
                muzzle_light_life: 0.085,
                muzzle_light_range: 17.0,
            },
        }
    }

    fn rifle_silhouette(self) -> RifleSilhouette {
        match self {
            WeaponKind::Sniper => RifleSilhouette {
                barrel_len: 0.50,
                optic_len: 0.26,
                optic_radius: 0.034,
            },
            WeaponKind::Blaster => RifleSilhouette {
                barrel_len: 0.26,
                optic_len: 0.12,
                optic_radius: 0.024,
            },
            _ => RifleSilhouette {
                barrel_len: 0.34,
                optic_len: 0.17,
                optic_radius: 0.028,
            },
        }
    }

    pub fn cooldown(self) -> f32 {
        match self {
            WeaponKind::Pistol => 0.18,
            WeaponKind::AssaultRifle => 0.09,
            WeaponKind::Sniper => 0.80,
            WeaponKind::Shotgun => 0.60,
            WeaponKind::Minigun => 0.05,
            WeaponKind::PlasmaRifle => 0.22,
            WeaponKind::Blaster => 0.14,
            WeaponKind::RocketLauncher => 1.10,
            WeaponKind::GrenadeLauncher => 0.75,
        }
    }

    /// True = full-auto (hold to fire), false = semi-auto.
    pub fn auto(self) -> bool {
        matches!(
            self,
            WeaponKind::AssaultRifle
                | WeaponKind::Minigun
                | WeaponKind::Blaster
                | WeaponKind::PlasmaRifle
        )
    }

    /// Voxel-sphere break radius at the hit point. 0 = single block.
    pub fn blast_radius(self) -> i32 {
        match self {
            WeaponKind::Pistol
            | WeaponKind::AssaultRifle
            | WeaponKind::Minigun
            | WeaponKind::Blaster => 0,
            WeaponKind::Sniper => 1,
            WeaponKind::Shotgun => 1,
            WeaponKind::PlasmaRifle => 2,
            WeaponKind::RocketLauncher => 5,
            WeaponKind::GrenadeLauncher => 4,
        }
    }

    /// Heat applied to `SuitVitals::laser_drill_charge` once per trigger pull.
    pub fn drill_heat_per_shot(self) -> f32 {
        match self {
            WeaponKind::Pistol => 0.38,
            WeaponKind::AssaultRifle => 0.44,
            WeaponKind::Sniper => 0.58,
            WeaponKind::Shotgun => 0.52,
            WeaponKind::Minigun => 0.24,
            WeaponKind::PlasmaRifle => 0.50,
            WeaponKind::Blaster => 0.42,
            WeaponKind::RocketLauncher => 0.95,
            WeaponKind::GrenadeLauncher => 0.88,
        }
    }

    pub fn pellets(self) -> u32 {
        match self {
            WeaponKind::Shotgun => 7,
            _ => 1,
        }
    }

    pub fn spread(self) -> f32 {
        match self {
            WeaponKind::Sniper => 0.0,
            WeaponKind::Pistol => 0.012,
            WeaponKind::AssaultRifle => 0.018,
            WeaponKind::Minigun => 0.040,
            WeaponKind::PlasmaRifle => 0.010,
            WeaponKind::Blaster => 0.008,
            WeaponKind::Shotgun => 0.12,
            WeaponKind::RocketLauncher => 0.006,
            WeaponKind::GrenadeLauncher => 0.02,
        }
    }

    /// Camera FOV kick (degrees) per shot.
    pub fn fov_kick(self) -> f32 {
        match self {
            WeaponKind::Sniper => 3.5,
            WeaponKind::RocketLauncher => 4.5,
            WeaponKind::Shotgun => 3.0,
            WeaponKind::GrenadeLauncher => 2.8,
            WeaponKind::Minigun => 0.6,
            WeaponKind::PlasmaRifle => 1.4,
            WeaponKind::AssaultRifle => 1.0,
            WeaponKind::Blaster => 1.2,
            WeaponKind::Pistol => 1.5,
        }
    }

    /// Vertical recoil kick in radians applied directly to the player's
    /// pitch. Positive values nudge the view upward and reward recoil
    /// control, especially on sustained automatic fire.
    pub fn pitch_kick(self) -> f32 {
        match self {
            WeaponKind::Pistol => 0.010,
            WeaponKind::AssaultRifle => 0.006,
            WeaponKind::Sniper => 0.022,
            WeaponKind::Shotgun => 0.028,
            WeaponKind::Minigun => 0.0035,
            WeaponKind::PlasmaRifle => 0.012,
            WeaponKind::Blaster => 0.010,
            WeaponKind::RocketLauncher => 0.032,
            WeaponKind::GrenadeLauncher => 0.024,
        }
    }

    /// Trauma added to the camera-shake model for each shot fired. Sits
    /// alongside the other per-weapon feel knobs so combat-feel tuning
    /// stays in one block instead of being hidden inside `fire_weapon`.
    pub fn fire_shake(self) -> f32 {
        match self {
            WeaponKind::Pistol => 0.08,
            WeaponKind::AssaultRifle => 0.10,
            WeaponKind::Blaster => 0.10,
            WeaponKind::PlasmaRifle => 0.14,
            WeaponKind::Sniper => 0.28,
            WeaponKind::Shotgun => 0.32,
            WeaponKind::Minigun => 0.06,
            WeaponKind::RocketLauncher => 0.35,
            WeaponKind::GrenadeLauncher => 0.26,
        }
    }

    /// Per-shot muzzle point-light intensity. Heavy launchers and the
    /// sniper paint the geometry around the player; the minigun keeps
    /// it dim so sustained fire does not blow out the scene.
    pub fn muzzle_light_intensity(self) -> f32 {
        match self {
            WeaponKind::RocketLauncher | WeaponKind::GrenadeLauncher => 1_800_000.0,
            WeaponKind::Sniper | WeaponKind::Shotgun => 1_200_000.0,
            WeaponKind::Minigun => 400_000.0,
            _ => 700_000.0,
        }
    }

    pub fn recoil_time(self) -> f32 {
        match self {
            WeaponKind::Minigun => 0.06,
            WeaponKind::AssaultRifle => 0.10,
            WeaponKind::Pistol => 0.14,
            WeaponKind::Sniper => 0.35,
            WeaponKind::Shotgun => 0.28,
            WeaponKind::RocketLauncher => 0.45,
            WeaponKind::GrenadeLauncher => 0.30,
            WeaponKind::PlasmaRifle => 0.18,
            WeaponKind::Blaster => 0.14,
        }
    }

    /// Projectile travel speed in blocks per second. Tuned so a shot
    /// across a typical 60–80 block sight line takes ~0.3–1.2 s —
    /// slow enough for the player's eye to follow, fast enough that
    /// rifle combat still feels snappy. Sniper and blaster energy
    /// bolts fly noticeably faster than physical rounds.
    pub fn projectile_speed(self) -> f32 {
        match self {
            WeaponKind::Pistol => 90.0,
            WeaponKind::AssaultRifle => 140.0,
            WeaponKind::Minigun => 160.0,
            WeaponKind::Shotgun => 110.0,
            WeaponKind::Sniper => 260.0,
            WeaponKind::PlasmaRifle => 120.0,
            WeaponKind::Blaster => 180.0,
            WeaponKind::RocketLauncher => 55.0,
            WeaponKind::GrenadeLauncher => 45.0,
        }
    }

    /// Visual radius of the glowing projectile core.
    ///
    /// Reserved: the current renderer drives the bolt silhouette via
    /// [`Self::bolt_dims`] and the cached halo/core meshes, but a
    /// follow-up pass may swap to a sphere-based core for plasma/
    /// blaster classes. Kept here so per-weapon tuning lives in one
    /// place even though no caller wires it yet.
    #[allow(dead_code)]
    pub fn projectile_radius(self) -> f32 {
        match self {
            WeaponKind::Pistol => 0.07,
            WeaponKind::AssaultRifle => 0.08,
            WeaponKind::Minigun => 0.07,
            WeaponKind::Shotgun => 0.06,
            WeaponKind::Sniper => 0.10,
            WeaponKind::PlasmaRifle => 0.18,
            WeaponKind::Blaster => 0.14,
            WeaponKind::RocketLauncher => 0.28,
            WeaponKind::GrenadeLauncher => 0.22,
        }
    }

    /// Per-kind (length, radius) of the laser-bolt cuboid. Used by
    /// [`WeaponFxCache`] to build a single shared mesh per weapon
    /// instead of one per shot.
    pub fn bolt_dims(self) -> (f32, f32) {
        let len = match self {
            WeaponKind::Sniper => 2.8,
            WeaponKind::RocketLauncher => 2.4,
            WeaponKind::GrenadeLauncher => 2.0,
            WeaponKind::AssaultRifle | WeaponKind::Blaster => 1.4,
            WeaponKind::PlasmaRifle => 1.2,
            WeaponKind::Pistol => 1.0,
            WeaponKind::Shotgun => 0.8,
            WeaponKind::Minigun => 1.1,
        };
        let r = match self {
            WeaponKind::Sniper => 0.055,
            WeaponKind::RocketLauncher => 0.11,
            WeaponKind::GrenadeLauncher => 0.095,
            WeaponKind::PlasmaRifle | WeaponKind::Blaster => 0.055,
            WeaponKind::AssaultRifle => 0.045,
            WeaponKind::Pistol => 0.040,
            WeaponKind::Shotgun => 0.038,
            WeaponKind::Minigun => 0.035,
        };
        (len, r)
    }

    /// Aim-down-sight zoom multiplier. FOV is divided by this factor
    /// when right-mouse-button is held. Sniper gets an extra mouse-wheel
    /// multiplier on top of this base value.
    pub fn ads_zoom(self) -> f32 {
        match self {
            WeaponKind::Pistol => 1.35,
            WeaponKind::AssaultRifle => 1.8,
            WeaponKind::Sniper => 3.0,
            WeaponKind::Shotgun => 1.4,
            WeaponKind::Minigun => 1.25,
            WeaponKind::PlasmaRifle => 1.7,
            WeaponKind::Blaster => 1.6,
            WeaponKind::RocketLauncher => 2.0,
            WeaponKind::GrenadeLauncher => 1.6,
        }
    }

    /// Magazine size before requiring a reload. Reserve ammo is infinite.
    pub fn mag_size(self) -> u32 {
        match self {
            WeaponKind::Pistol => 12,
            WeaponKind::AssaultRifle => 30,
            WeaponKind::Sniper => 5,
            WeaponKind::Shotgun => 6,
            WeaponKind::Minigun => 200,
            WeaponKind::PlasmaRifle => 20,
            WeaponKind::Blaster => 25,
            WeaponKind::RocketLauncher => 1,
            WeaponKind::GrenadeLauncher => 6,
        }
    }

    /// Time in seconds for a reload animation to finish. Consumed by
    /// `reload_input` when admin cheats disable infinite ammo, and by
    /// `animate_viewmodel` to drive the down-and-back reload pose.
    pub fn reload_time(self) -> f32 {
        match self {
            WeaponKind::Pistol => 1.2,
            WeaponKind::AssaultRifle => 1.9,
            WeaponKind::Sniper => 2.6,
            WeaponKind::Shotgun => 2.4,
            WeaponKind::Minigun => 4.0,
            WeaponKind::PlasmaRifle => 2.0,
            WeaponKind::Blaster => 1.6,
            WeaponKind::RocketLauncher => 2.8,
            WeaponKind::GrenadeLauncher => 2.6,
        }
    }

    fn tracer_fx(self) -> TracerFxProfile {
        match self {
            WeaponKind::Pistol => TracerFxProfile {
                length: 1.15,
                radius: 0.028,
                life: 0.04,
            },
            WeaponKind::AssaultRifle => TracerFxProfile {
                length: 1.55,
                radius: 0.034,
                life: 0.05,
            },
            WeaponKind::Sniper => TracerFxProfile {
                length: 2.25,
                radius: 0.050,
                life: 0.08,
            },
            WeaponKind::Shotgun => TracerFxProfile {
                length: 0.85,
                radius: 0.032,
                life: 0.035,
            },
            WeaponKind::Minigun => TracerFxProfile {
                length: 0.95,
                radius: 0.025,
                life: 0.03,
            },
            WeaponKind::PlasmaRifle => TracerFxProfile {
                length: 1.45,
                radius: 0.046,
                life: 0.06,
            },
            WeaponKind::Blaster => TracerFxProfile {
                length: 1.70,
                radius: 0.042,
                life: 0.06,
            },
            WeaponKind::RocketLauncher => TracerFxProfile {
                length: 2.05,
                radius: 0.085,
                life: 0.09,
            },
            WeaponKind::GrenadeLauncher => TracerFxProfile {
                length: 1.80,
                radius: 0.072,
                life: 0.08,
            },
        }
    }

    fn impact_fx(self) -> ImpactFxProfile {
        match self {
            WeaponKind::Pistol => ImpactFxProfile {
                puff_life: 0.08,
                puff_start_scale: 0.18,
                puff_end_scale: 0.95,
                halo_life: 0.0,
                halo_start_scale: 0.0,
                halo_end_scale: 0.0,
                light_intensity: 90_000.0,
                light_range: 5.5,
                shake: 0.03,
            },
            WeaponKind::AssaultRifle => ImpactFxProfile {
                puff_life: 0.09,
                puff_start_scale: 0.22,
                puff_end_scale: 1.05,
                halo_life: 0.05,
                halo_start_scale: 0.35,
                halo_end_scale: 1.35,
                light_intensity: 120_000.0,
                light_range: 6.5,
                shake: 0.035,
            },
            WeaponKind::Sniper => ImpactFxProfile {
                puff_life: 0.16,
                puff_start_scale: 0.30,
                puff_end_scale: 1.90,
                halo_life: 0.12,
                halo_start_scale: 0.45,
                halo_end_scale: 2.90,
                light_intensity: 350_000.0,
                light_range: 10.5,
                shake: 0.10,
            },
            WeaponKind::Shotgun => ImpactFxProfile {
                puff_life: 0.14,
                puff_start_scale: 0.28,
                puff_end_scale: 1.65,
                halo_life: 0.10,
                halo_start_scale: 0.42,
                halo_end_scale: 2.40,
                light_intensity: 260_000.0,
                light_range: 8.8,
                shake: 0.09,
            },
            WeaponKind::Minigun => ImpactFxProfile {
                puff_life: 0.06,
                puff_start_scale: 0.16,
                puff_end_scale: 0.72,
                halo_life: 0.0,
                halo_start_scale: 0.0,
                halo_end_scale: 0.0,
                light_intensity: 60_000.0,
                light_range: 4.6,
                shake: 0.025,
            },
            WeaponKind::PlasmaRifle => ImpactFxProfile {
                puff_life: 0.12,
                puff_start_scale: 0.26,
                puff_end_scale: 1.25,
                halo_life: 0.08,
                halo_start_scale: 0.45,
                halo_end_scale: 1.95,
                light_intensity: 180_000.0,
                light_range: 7.8,
                shake: 0.05,
            },
            WeaponKind::Blaster => ImpactFxProfile {
                puff_life: 0.12,
                puff_start_scale: 0.24,
                puff_end_scale: 1.35,
                halo_life: 0.09,
                halo_start_scale: 0.40,
                halo_end_scale: 2.05,
                light_intensity: 180_000.0,
                light_range: 7.4,
                shake: 0.05,
            },
            WeaponKind::RocketLauncher => ImpactFxProfile {
                puff_life: 0.16,
                puff_start_scale: 0.34,
                puff_end_scale: 1.70,
                halo_life: 0.10,
                halo_start_scale: 0.55,
                halo_end_scale: 2.60,
                light_intensity: 240_000.0,
                light_range: 9.0,
                shake: 0.10,
            },
            WeaponKind::GrenadeLauncher => ImpactFxProfile {
                puff_life: 0.14,
                puff_start_scale: 0.30,
                puff_end_scale: 1.55,
                halo_life: 0.10,
                halo_start_scale: 0.50,
                halo_end_scale: 2.30,
                light_intensity: 210_000.0,
                light_range: 8.4,
                shake: 0.08,
            },
        }
    }

    fn explosion_fx(self) -> ExplosionFxProfile {
        match self {
            WeaponKind::PlasmaRifle => ExplosionFxProfile {
                sphere_base_rgb: Vec3::new(0.82, 0.45, 1.0),
                sphere_emissive_rgb: Vec3::new(14.0, 7.0, 22.0),
                ring_base_rgb: Vec3::new(0.72, 0.55, 1.0),
                ring_emissive_rgb: Vec3::new(12.0, 8.0, 20.0),
                light_rgb: Vec3::new(0.72, 0.5, 1.0),
                light_intensity: 1_800_000.0,
                flash_rgb: Vec3::new(0.86, 0.70, 1.0),
                flash_alpha: 0.36,
                shake: 0.30,
                sphere_life: 0.26,
                sphere_scale_mul: 1.7,
            },
            WeaponKind::GrenadeLauncher => ExplosionFxProfile {
                sphere_base_rgb: Vec3::new(0.86, 1.0, 0.28),
                sphere_emissive_rgb: Vec3::new(14.0, 18.0, 3.0),
                ring_base_rgb: Vec3::new(0.92, 1.0, 0.46),
                ring_emissive_rgb: Vec3::new(10.0, 14.0, 4.0),
                light_rgb: Vec3::new(0.88, 1.0, 0.35),
                light_intensity: 2_400_000.0,
                flash_rgb: Vec3::new(0.95, 1.0, 0.65),
                flash_alpha: 0.48,
                shake: 0.45,
                sphere_life: 0.42,
                sphere_scale_mul: 2.3,
            },
            _ => ExplosionFxProfile {
                sphere_base_rgb: Vec3::new(1.0, 0.6, 0.15),
                sphere_emissive_rgb: Vec3::new(22.0, 9.0, 2.5),
                ring_base_rgb: Vec3::new(1.0, 0.7, 0.3),
                ring_emissive_rgb: Vec3::new(18.0, 10.0, 3.0),
                light_rgb: Vec3::new(1.0, 0.6, 0.2),
                light_intensity: 3_000_000.0,
                flash_rgb: Vec3::new(1.0, 0.85, 0.55),
                flash_alpha: 0.55,
                shake: 0.55,
                sphere_life: 0.45,
                sphere_scale_mul: 2.1,
            },
        }
    }
}

// ---------------------------------------------------------------------
// Resources + components
// ---------------------------------------------------------------------

#[derive(Resource)]
pub struct ActiveWeapon {
    pub kind: WeaponKind,
    pub needs_rebuild: bool,
}

impl Default for ActiveWeapon {
    fn default() -> Self {
        Self {
            kind: WeaponKind::PlasmaRifle,
            needs_rebuild: false,
        }
    }
}

#[derive(Component)]
pub struct Weapon {
    pub recoil_t: f32,
    pub cooldown: f32,
    pub kind: WeaponKind,
    /// Rounds remaining in magazine. Hits 0 → auto-reload.
    pub mag: u32,
    /// Remaining reload time in seconds. > 0 blocks firing and drives
    /// the reload-swing viewmodel animation.
    pub reload_t: f32,
    /// Total reload time — used to normalise the animation progress.
    pub reload_total: f32,
}

/// Right-mouse-button ADS (aim-down-sight) state. Shared across the
/// weapon, player camera FOV, and mouse-look sensitivity so that
/// everything scales consistently when scoped.
#[derive(Resource)]
pub struct ScopeState {
    /// True while RMB is held and scope animation is engaging.
    pub active: bool,
    /// Smoothed 0..1 blend value used by the viewmodel pose.
    pub progress: f32,
    /// Sniper-only extra zoom multiplier from the scroll wheel (1..=10).
    pub sniper_zoom: f32,
    /// Smoothed effective zoom factor applied to camera FOV and mouse
    /// sensitivity. 1.0 = hip-fire, higher = more zoomed in.
    pub current_zoom: f32,
}

impl Default for ScopeState {
    fn default() -> Self {
        Self {
            active: false,
            progress: 0.0,
            sniper_zoom: 1.0,
            current_zoom: 1.0,
        }
    }
}

#[derive(Resource, Default)]
struct WeaponHolster {
    progress: f32,
}

#[derive(Component)]
struct WeaponRestPose(Transform);

#[derive(Component)]
struct MuzzleFlash {
    life: f32,
    max_life: f32,
    start_scale: f32,
    end_scale: f32,
}

#[derive(Component)]
struct MuzzleFlashLight {
    life: f32,
    max_life: f32,
    base_intensity: f32,
}

#[derive(Component)]
struct Tracer {
    life: f32,
    max_life: f32,
}

#[derive(Component)]
struct Debris {
    velocity: Vec3,
    life: f32,
    max_life: f32,
    spin: Vec3,
}

/// A full voxel that has been disconnected from the world by a blast
/// and is now falling as a physics entity. It is NOT a voxel in the
/// world anymore (so it never blocks shots or the player), and on
/// impact it shatters into tiny debris pieces.
#[derive(Component)]
struct FallingBlock {
    velocity: Vec3,
    spin: Vec3,
    voxel: crate::blocks::Voxel,
    max_fall_time: f32,
}

#[derive(Component)]
struct Explosion {
    life: f32,
    max_life: f32,
    max_scale: f32,
}

const MAX_PROJECTILE_LIFETIME_SECS: f32 = 6.0;
const PROJECTILE_ARRIVAL_GRACE_SECS: f32 = 0.25;
const WEAPON_MAX_RANGE: f32 = 10_000.0;
const MUZZLE_RAY_EPSILON: f32 = 0.05;

/// A travelling projectile carrying its delayed impact data. On
/// arrival the stored payload is applied: break a sphere of blocks,
/// spawn debris, and (for explosives) spawn a fireball.
#[derive(Component)]
struct Projectile {
    dir: Vec3,
    speed: f32,
    /// Safety horizon for shots into unloaded/empty space. Distant hits
    /// still resolve when this expires, while misses simply disappear.
    life: f32,
    /// Remaining distance along `dir` until it reaches the pre-computed
    /// impact point (or its max range, if the ray missed everything).
    remaining: f32,
    kind: WeaponKind,
    /// Voxel hit. `None` means the projectile flew past max range and
    /// should simply vanish on arrival without damage.
    hit_block: Option<(i32, i32, i32)>,
    /// World-space impact position (block centre or end-of-range).
    impact_pos: Vec3,
}

fn projectile_lifetime_secs(remaining: f32, speed: f32) -> f32 {
    if !remaining.is_finite() || !speed.is_finite() || speed <= 0.0 {
        return MAX_PROJECTILE_LIFETIME_SECS;
    }
    (remaining.max(0.0) / speed + PROJECTILE_ARRIVAL_GRACE_SECS)
        .clamp(PROJECTILE_ARRIVAL_GRACE_SECS, MAX_PROJECTILE_LIFETIME_SECS)
}

fn should_spawn_projectile_light(kind: WeaponKind, fx_scale: f32, shot_number: u64) -> bool {
    let fx_scale = if fx_scale.is_finite() {
        fx_scale.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let stride = match kind {
        WeaponKind::RocketLauncher | WeaponKind::GrenadeLauncher if fx_scale >= 0.20 => 1,
        WeaponKind::PlasmaRifle if fx_scale >= 0.75 => 1,
        WeaponKind::PlasmaRifle if fx_scale >= 0.35 => 2,
        WeaponKind::Sniper if fx_scale >= 0.55 => 1,
        WeaponKind::Blaster if fx_scale >= 0.80 => 2,
        WeaponKind::Blaster if fx_scale >= 0.55 => 3,
        WeaponKind::Pistol | WeaponKind::Shotgun if fx_scale >= 0.85 => 2,
        WeaponKind::AssaultRifle if fx_scale >= 0.85 => 3,
        WeaponKind::Minigun if fx_scale >= 0.85 => 5,
        _ => return false,
    };
    shot_number % stride == 0
}

fn should_spawn_muzzle_light(kind: WeaponKind, fx_scale: f32, shot_number: u64) -> bool {
    let fx_scale = if fx_scale.is_finite() {
        fx_scale.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if fx_scale <= 0.20 {
        return false;
    }

    // Keep deliberate shots luminous while bounding overlapping dynamic
    // lights from rapid-fire weapons. LowSpec normally supplies 0.65 here.
    let stride = match kind {
        WeaponKind::Minigun if fx_scale < 0.80 => 3,
        WeaponKind::Minigun => 2,
        WeaponKind::AssaultRifle if fx_scale < 0.80 => 2,
        _ => 1,
    };
    shot_number % stride == 0
}

fn projectile_visual_layer_count(fx_scale: f32) -> usize {
    let fx_scale = if fx_scale.is_finite() {
        fx_scale.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if fx_scale >= 0.75 {
        3
    } else if fx_scale >= 0.35 {
        2
    } else {
        1
    }
}

fn debris_spawn_cap(fx_scale: f32) -> u32 {
    let fx_scale = if fx_scale.is_finite() {
        fx_scale.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if fx_scale <= 0.10 {
        0
    } else {
        (40.0 * fx_scale * fx_scale).round().clamp(4.0, 40.0) as u32
    }
}

fn falling_block_spawn_cap(kind: WeaponKind, fx_scale: f32) -> u32 {
    let fx_scale = if fx_scale.is_finite() {
        fx_scale.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if fx_scale <= 0.10 {
        return 0;
    }
    let full_cap = match kind {
        WeaponKind::RocketLauncher | WeaponKind::GrenadeLauncher => 768.0,
        WeaponKind::PlasmaRifle | WeaponKind::Blaster => 512.0,
        _ => 256.0,
    };
    (full_cap * fx_scale * fx_scale)
        .round()
        .clamp(16.0, full_cap) as u32
}

fn despawn_recursive_if_exists(commands: &mut Commands, entity: Entity) {
    if let Some(entity_commands) = commands.get_entity(entity) {
        entity_commands.despawn_recursive();
    }
}

// ---------------------------------------------------------------------
// Viewmodel setup + hot-swap
// ---------------------------------------------------------------------

fn setup_weapon(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fx: ResMut<WeaponFxCache>,
    settings: Res<WorldSettings>,
    camera_q: Query<Entity, (With<Camera3d>, With<Player>)>,
    active: Res<ActiveWeapon>,
) {
    let Ok(cam) = camera_q.get_single() else {
        return;
    };
    build_viewmodel(
        &mut commands,
        &mut meshes,
        &mut materials,
        &mut fx,
        cam,
        active.kind,
        WeaponVisualDetail::for_profile(settings.runtime_profile),
    );
    info!(
        "Weapon viewmodel ({}) attached to player camera.",
        active.kind.name()
    );
}

fn switch_weapon(
    keys: Res<ButtonInput<KeyCode>>,
    toolbelt: Option<ResMut<crate::toolbelt::ToolbeltState>>,
    mut mode: Option<ResMut<crate::mode::ModeContext>>,
    mut active: ResMut<ActiveWeapon>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fx: ResMut<WeaponFxCache>,
    settings: Res<WorldSettings>,
    camera_q: Query<Entity, (With<Camera3d>, With<Player>)>,
    weapons_q: Query<Entity, With<Weapon>>,
) {
    const HOTKEYS: [(KeyCode, WeaponKind); 9] = [
        (KeyCode::Digit1, WeaponKind::Pistol),
        (KeyCode::Digit2, WeaponKind::AssaultRifle),
        (KeyCode::Digit3, WeaponKind::Sniper),
        (KeyCode::Digit4, WeaponKind::Shotgun),
        (KeyCode::Digit5, WeaponKind::Minigun),
        (KeyCode::Digit6, WeaponKind::PlasmaRifle),
        (KeyCode::Digit7, WeaponKind::Blaster),
        (KeyCode::Digit8, WeaponKind::RocketLauncher),
        (KeyCode::Digit9, WeaponKind::GrenadeLauncher),
    ];
    let requested_slot = HOTKEYS.iter().any(|(k, _)| keys.just_pressed(*k));
    if let Some(mut toolbelt) = toolbelt {
        let blocks_weapons = mode
            .as_deref()
            .map(|mode| !mode.allows_weapons())
            .unwrap_or_else(|| toolbelt.blocks_weapons());
        if blocks_weapons {
            if requested_slot {
                let status =
                    "Weapon slots are holstered in Creative Build. Press F8 to arm weapons.";
                toolbelt.status = status.into();
                if let Some(mode) = mode.as_deref_mut() {
                    mode.status = status.into();
                }
            }
            return;
        }
    }
    let mut requested: Option<WeaponKind> = None;
    for (k, w) in &HOTKEYS {
        if keys.just_pressed(*k) {
            requested = Some(*w);
        }
    }
    if let Some(new_kind) = requested {
        if new_kind != active.kind {
            active.kind = new_kind;
            active.needs_rebuild = true;
        }
    }
    if !active.needs_rebuild {
        return;
    }
    active.needs_rebuild = false;
    for e in weapons_q.iter() {
        despawn_recursive_if_exists(&mut commands, e);
    }
    let Ok(cam) = camera_q.get_single() else {
        return;
    };
    build_viewmodel(
        &mut commands,
        &mut meshes,
        &mut materials,
        &mut fx,
        cam,
        active.kind,
        WeaponVisualDetail::for_profile(settings.runtime_profile),
    );
}

fn build_viewmodel(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut WeaponFxCache,
    cam: Entity,
    kind: WeaponKind,
    detail: WeaponVisualDetail,
) {
    let cube = fx.viewmodel_cube_shared(meshes);
    let cube = &cube;
    let dark_body = fx.viewmodel_mat_for(ViewmodelMaterialTone::DarkBody, kind, materials);
    let gunmetal = fx.viewmodel_mat_for(ViewmodelMaterialTone::Gunmetal, kind, materials);
    // Bright polished chrome for barrel tips, slide rails, muzzle
    // compensators — catches the scene light and reads "fusion-era".
    let chrome = fx.viewmodel_mat_for(ViewmodelMaterialTone::Chrome, kind, materials);
    // Rubberised grip with a faint sheen.
    let grip = fx.viewmodel_mat_for(ViewmodelMaterialTone::Grip, kind, materials);
    let accent = fx.viewmodel_mat_for(ViewmodelMaterialTone::Accent, kind, materials);
    // Ultra-bright energy core — for plasma cells, cooling vents, bolt
    // loading points. Pure weapon-tint, high HDR value for bloom.
    let core = fx.viewmodel_mat_for(ViewmodelMaterialTone::Core, kind, materials);
    // Dark voxel optic: a restrained cyan glint separates glass from
    // the metal housing without transparent overlap in first person.
    let optic_glass = fx.viewmodel_mat_for(ViewmodelMaterialTone::OpticGlass, kind, materials);

    let presentation = kind.viewmodel_tuning();
    let rest = Transform::from_translation(presentation.rest_translation)
        .with_rotation(Quat::from_rotation_y(-0.08) * Quat::from_rotation_x(0.02));

    commands.entity(cam).insert(VisibilityBundle::default());

    // Pre-build the weapon root so we can parent parts to it, then
    // parent the root to the camera. Using the root entity as a parent
    // keeps the hierarchy flat + easy to despawn on hot-swap.
    let root = commands
        .spawn((
            SpatialBundle {
                transform: rest,
                ..default()
            },
            Weapon {
                recoil_t: 0.0,
                cooldown: 0.0,
                kind,
                mag: kind.mag_size(),
                reload_t: 0.0,
                reload_total: 0.0,
            },
            WeaponRestPose(rest),
            Name::new(format!("Weapon:{}", kind.name())),
        ))
        .id();
    commands.entity(cam).add_child(root);

    match kind {
        WeaponKind::Pistol => {
            // Slide + receiver
            spawn_cuboid(
                commands,
                cube,
                root,
                0.065,
                0.085,
                0.22,
                Vec3::new(0.0, 0.0, -0.02),
                Quat::IDENTITY,
                &dark_body,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.072,
                0.028,
                0.23,
                Vec3::new(0.0, 0.04, -0.02),
                Quat::IDENTITY,
                &gunmetal,
            );
            // Polished barrel protruding through the slide.
            spawn_cyl(
                commands,
                cube,
                root,
                0.016,
                0.16,
                Vec3::new(0.0, 0.008, -0.17),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &chrome,
            );
            // Grip with checker panel.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.055,
                0.14,
                0.06,
                Vec3::new(0.0, -0.09, 0.035),
                Quat::from_rotation_x(-0.30),
                &grip,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.058,
                0.015,
                0.062,
                Vec3::new(0.0, -0.12, 0.05),
                Quat::from_rotation_x(-0.30),
                &gunmetal,
            );
            // Trigger guard loop (approximated by a thin ring of cuboids).
            spawn_cuboid(
                commands,
                cube,
                root,
                0.018,
                0.018,
                0.05,
                Vec3::new(0.0, -0.04, 0.015),
                Quat::IDENTITY,
                &dark_body,
            );
            // Top rail with glowing sight strip + front dot.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.012,
                0.016,
                0.20,
                Vec3::new(0.0, 0.056, -0.04),
                Quat::IDENTITY,
                &gunmetal,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.008,
                0.003,
                0.18,
                Vec3::new(0.0, 0.068, -0.04),
                Quat::IDENTITY,
                &accent,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.006,
                0.008,
                0.008,
                Vec3::new(0.0, 0.070, -0.14),
                Quat::IDENTITY,
                &core,
            );
            // Side vents / energy cell window glowing.
            if detail.includes_decorative_parts() {
                for x in [-0.034_f32, 0.034] {
                    spawn_cuboid(
                        commands,
                        cube,
                        root,
                        0.004,
                        0.020,
                        0.035,
                        Vec3::new(x, 0.005, 0.04),
                        Quat::IDENTITY,
                        &core,
                    );
                }
            }
        }
        WeaponKind::AssaultRifle | WeaponKind::Blaster => {
            assemble_rifle(
                commands,
                cube,
                root,
                &dark_body,
                &gunmetal,
                &chrome,
                &grip,
                &accent,
                &core,
                &optic_glass,
                kind,
                detail,
            );
        }
        WeaponKind::Sniper => {
            assemble_rifle(
                commands,
                cube,
                root,
                &dark_body,
                &gunmetal,
                &chrome,
                &grip,
                &accent,
                &core,
                &optic_glass,
                kind,
                detail,
            );
        }
        WeaponKind::Shotgun => {
            // Double-barrel receiver with polished twin tubes.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.105,
                0.065,
                0.44,
                Vec3::ZERO,
                Quat::IDENTITY,
                &dark_body,
            );
            spawn_cyl(
                commands,
                cube,
                root,
                0.024,
                0.40,
                Vec3::new(0.026, 0.014, -0.32),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &chrome,
            );
            spawn_cyl(
                commands,
                cube,
                root,
                0.024,
                0.40,
                Vec3::new(-0.026, 0.014, -0.32),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &chrome,
            );
            // Muzzle brake / chokes.
            spawn_cyl(
                commands,
                cube,
                root,
                0.030,
                0.05,
                Vec3::new(0.026, 0.014, -0.54),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &gunmetal,
            );
            spawn_cyl(
                commands,
                cube,
                root,
                0.030,
                0.05,
                Vec3::new(-0.026, 0.014, -0.54),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &gunmetal,
            );
            // Stock with rubberised cheek pad.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.075,
                0.055,
                0.20,
                Vec3::new(0.0, 0.0, 0.23),
                Quat::IDENTITY,
                &dark_body,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.062,
                0.020,
                0.18,
                Vec3::new(0.0, 0.038, 0.23),
                Quat::IDENTITY,
                &grip,
            );
            // Pump-grip handle.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.055,
                0.04,
                0.08,
                Vec3::new(0.0, -0.05, -0.14),
                Quat::IDENTITY,
                &grip,
            );
            // Sight rail strip.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.003,
                0.014,
                0.36,
                Vec3::new(0.0, 0.04, -0.10),
                Quat::IDENTITY,
                &accent,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.006,
                0.010,
                0.010,
                Vec3::new(0.0, 0.052, -0.26),
                Quat::IDENTITY,
                &core,
            );
            // Shell ejection port glowing orange.
            if detail.includes_decorative_parts() {
                spawn_cuboid(
                    commands,
                    cube,
                    root,
                    0.015,
                    0.008,
                    0.04,
                    Vec3::new(0.046, 0.018, 0.02),
                    Quat::IDENTITY,
                    &core,
                );
            }
        }
        WeaponKind::Minigun => {
            // Main housing with side energy cells.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.16,
                0.13,
                0.32,
                Vec3::ZERO,
                Quat::IDENTITY,
                &dark_body,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.006,
                0.06,
                0.20,
                Vec3::new(0.082, 0.0, 0.03),
                Quat::IDENTITY,
                &core,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.006,
                0.06,
                0.20,
                Vec3::new(-0.082, 0.0, 0.03),
                Quat::IDENTITY,
                &core,
            );
            // Rotary barrel cluster (6 barrels + central shaft).
            let barrel_count = if detail.includes_decorative_parts() {
                6
            } else {
                3
            };
            for i in 0..barrel_count {
                let a = i as f32 * std::f32::consts::TAU / barrel_count as f32;
                let r = 0.045;
                spawn_cyl(
                    commands,
                    cube,
                    root,
                    0.013,
                    0.38,
                    Vec3::new(a.cos() * r, a.sin() * r + 0.005, -0.32),
                    Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                    &chrome,
                );
            }
            spawn_cyl(
                commands,
                cube,
                root,
                0.014,
                0.36,
                Vec3::new(0.0, 0.005, -0.31),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &gunmetal,
            );
            // Barrel shroud ring + muzzle comp.
            spawn_cyl(
                commands,
                cube,
                root,
                0.062,
                0.035,
                Vec3::new(0.0, 0.005, -0.14),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &gunmetal,
            );
            spawn_cyl(
                commands,
                cube,
                root,
                0.068,
                0.05,
                Vec3::new(0.0, 0.005, -0.50),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &accent,
            );
            // Ammo drum underneath.
            spawn_cyl(
                commands,
                cube,
                root,
                0.075,
                0.12,
                Vec3::new(0.0, -0.09, 0.04),
                Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                &dark_body,
            );
            spawn_cyl(
                commands,
                cube,
                root,
                0.055,
                0.035,
                Vec3::new(0.075, -0.09, 0.04),
                Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                &core,
            );
            // Handles.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.035,
                0.10,
                0.04,
                Vec3::new(0.0, -0.08, -0.14),
                Quat::IDENTITY,
                &grip,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.035,
                0.10,
                0.04,
                Vec3::new(0.0, -0.08, 0.16),
                Quat::IDENTITY,
                &grip,
            );
        }
        WeaponKind::PlasmaRifle => {
            // Streamlined smooth body with glowing plasma tube running
            // through the centre.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.095,
                0.075,
                0.44,
                Vec3::ZERO,
                Quat::IDENTITY,
                &dark_body,
            );
            spawn_cyl(
                commands,
                cube,
                root,
                0.024,
                0.40,
                Vec3::new(0.0, 0.0, -0.30),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &gunmetal,
            );
            // Glass-windowed plasma chamber (chunky voxel bar of pure energy).
            spawn_cuboid(
                commands,
                cube,
                root,
                0.028,
                0.028,
                0.26,
                Vec3::new(0.0, 0.0, -0.24),
                Quat::IDENTITY,
                &core,
            );
            // Glowing energy cell cube near the receiver.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.042,
                0.042,
                0.042,
                Vec3::new(0.0, 0.042, 0.07),
                Quat::IDENTITY,
                &core,
            );
            // Cooling fins along the top.
            let fin_count = if detail.includes_decorative_parts() {
                5
            } else {
                2
            };
            for i in 0..fin_count {
                let z = -0.05 - i as f32 * 0.06;
                spawn_cuboid(
                    commands,
                    cube,
                    root,
                    0.06,
                    0.010,
                    0.016,
                    Vec3::new(0.0, 0.050, z),
                    Quat::IDENTITY,
                    &gunmetal,
                );
            }
            // Side accent strips.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.005,
                0.022,
                0.34,
                Vec3::new(0.048, 0.0, -0.10),
                Quat::IDENTITY,
                &accent,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.005,
                0.022,
                0.34,
                Vec3::new(-0.048, 0.0, -0.10),
                Quat::IDENTITY,
                &accent,
            );
            // Muzzle emitter.
            spawn_cyl(
                commands,
                cube,
                root,
                0.032,
                0.04,
                Vec3::new(0.0, 0.0, -0.52),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &core,
            );
            // Pistol grip + stock.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.045,
                0.11,
                0.055,
                Vec3::new(0.0, -0.08, 0.05),
                Quat::from_rotation_x(-0.25),
                &grip,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.055,
                0.042,
                0.14,
                Vec3::new(0.0, 0.0, 0.23),
                Quat::IDENTITY,
                &dark_body,
            );
        }
        WeaponKind::RocketLauncher => {
            // Big tube with polished launcher rim.
            spawn_cyl(
                commands,
                cube,
                root,
                0.060,
                0.76,
                Vec3::new(0.0, 0.0, -0.22),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &dark_body,
            );
            spawn_cyl(
                commands,
                cube,
                root,
                0.078,
                0.10,
                Vec3::new(0.0, 0.0, 0.20),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &gunmetal,
            );
            // Warhead peeking from the front of the tube.
            spawn_cyl(
                commands,
                cube,
                root,
                0.045,
                0.15,
                Vec3::new(0.0, 0.0, -0.52),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &accent,
            );
            spawn_cyl(
                commands,
                cube,
                root,
                0.022,
                0.04,
                Vec3::new(0.0, 0.0, -0.62),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &core,
            );
            // Top handle/scope rail.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.024,
                0.03,
                0.34,
                Vec3::new(0.0, 0.080, -0.18),
                Quat::IDENTITY,
                &gunmetal,
            );
            spawn_cyl(
                commands,
                cube,
                root,
                0.018,
                0.09,
                Vec3::new(0.0, 0.110, -0.26),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &dark_body,
            );
            if detail.includes_decorative_parts() {
                spawn_optic_lens(
                    commands,
                    cube,
                    root,
                    0.015,
                    Vec3::new(0.0, 0.110, -0.31),
                    &optic_glass,
                );
            }
            // Shoulder cradle underneath.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.055,
                0.06,
                0.14,
                Vec3::new(0.0, -0.075, 0.10),
                Quat::IDENTITY,
                &grip,
            );
            // Side fins (vents).
            spawn_cuboid(
                commands,
                cube,
                root,
                0.005,
                0.050,
                0.26,
                Vec3::new(0.060, 0.0, -0.12),
                Quat::IDENTITY,
                &accent,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.005,
                0.050,
                0.26,
                Vec3::new(-0.060, 0.0, -0.12),
                Quat::IDENTITY,
                &accent,
            );
        }
        WeaponKind::GrenadeLauncher => {
            spawn_cuboid(
                commands,
                cube,
                root,
                0.105,
                0.085,
                0.36,
                Vec3::ZERO,
                Quat::IDENTITY,
                &dark_body,
            );
            spawn_cyl(
                commands,
                cube,
                root,
                0.047,
                0.34,
                Vec3::new(0.0, 0.012, -0.32),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &gunmetal,
            );
            // Revolver drum with 6 chambers (glowing).
            spawn_cyl(
                commands,
                cube,
                root,
                0.072,
                0.090,
                Vec3::new(0.0, 0.0, -0.05),
                Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                &dark_body,
            );
            let chamber_count = if detail.includes_decorative_parts() {
                6
            } else {
                3
            };
            for i in 0..chamber_count {
                let a = i as f32 * std::f32::consts::TAU / chamber_count as f32;
                let r = 0.045;
                spawn_cyl(
                    commands,
                    cube,
                    root,
                    0.017,
                    0.094,
                    Vec3::new(0.0, a.sin() * r, -0.05 + a.cos() * r),
                    Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                    &core,
                );
            }
            // Muzzle flash-hider.
            spawn_cyl(
                commands,
                cube,
                root,
                0.055,
                0.035,
                Vec3::new(0.0, 0.012, -0.50),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                &chrome,
            );
            // Grip + stock.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.05,
                0.12,
                0.06,
                Vec3::new(0.0, -0.085, 0.06),
                Quat::from_rotation_x(-0.20),
                &grip,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.058,
                0.045,
                0.16,
                Vec3::new(0.0, 0.0, 0.22),
                Quat::IDENTITY,
                &dark_body,
            );
            // Top rail with sight.
            spawn_cuboid(
                commands,
                cube,
                root,
                0.016,
                0.018,
                0.28,
                Vec3::new(0.0, 0.058, -0.12),
                Quat::IDENTITY,
                &gunmetal,
            );
            spawn_cuboid(
                commands,
                cube,
                root,
                0.008,
                0.010,
                0.010,
                Vec3::new(0.0, 0.072, -0.22),
                Quat::IDENTITY,
                &core,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn assemble_rifle(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    root: Entity,
    dark_body: &Handle<StandardMaterial>,
    gunmetal: &Handle<StandardMaterial>,
    chrome: &Handle<StandardMaterial>,
    grip: &Handle<StandardMaterial>,
    accent: &Handle<StandardMaterial>,
    core: &Handle<StandardMaterial>,
    optic_glass: &Handle<StandardMaterial>,
    kind: WeaponKind,
    detail: WeaponVisualDetail,
) {
    let silhouette = kind.rifle_silhouette();
    let barrel_len = silhouette.barrel_len;
    let scope_len = silhouette.optic_len;
    let scope_r = silhouette.optic_radius;
    let is_sniper = kind == WeaponKind::Sniper;
    let barrel_material = if kind == WeaponKind::Blaster {
        accent
    } else {
        chrome
    };

    // Receiver + handguard stack.
    spawn_cuboid(
        commands,
        cube,
        root,
        0.09,
        0.065,
        0.42,
        Vec3::ZERO,
        Quat::IDENTITY,
        dark_body,
    );
    spawn_cuboid(
        commands,
        cube,
        root,
        0.11,
        0.042,
        0.20,
        Vec3::new(0.0, 0.006, -0.14),
        Quat::IDENTITY,
        gunmetal,
    );
    // Polished barrel + muzzle compensator.
    spawn_cyl(
        commands,
        cube,
        root,
        0.019,
        barrel_len,
        Vec3::new(0.0, 0.006, -0.16 - barrel_len / 2.0),
        Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        barrel_material,
    );
    spawn_cyl(
        commands,
        cube,
        root,
        0.028,
        0.045,
        Vec3::new(0.0, 0.006, -0.16 - barrel_len - 0.015),
        Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        gunmetal,
    );
    // Cooling vents on handguard (stacked fins).
    let vent_count = if detail.includes_decorative_parts() {
        4
    } else {
        2
    };
    for i in 0..vent_count {
        let z = -0.08 - i as f32 * 0.05;
        spawn_cuboid(
            commands,
            cube,
            root,
            0.118,
            0.004,
            0.012,
            Vec3::new(0.0, 0.026, z),
            Quat::IDENTITY,
            gunmetal,
        );
    }
    // Pistol grip (angled) + rubberised feel.
    spawn_cuboid(
        commands,
        cube,
        root,
        0.06,
        0.14,
        0.06,
        Vec3::new(0.0, -0.10, 0.03),
        Quat::from_rotation_x(-0.30),
        grip,
    );
    // Stock.
    spawn_cuboid(
        commands,
        cube,
        root,
        0.070,
        0.050,
        0.16,
        Vec3::new(0.0, 0.0, 0.23),
        Quat::IDENTITY,
        dark_body,
    );
    spawn_cuboid(
        commands,
        cube,
        root,
        0.058,
        0.022,
        0.14,
        Vec3::new(0.0, 0.034, 0.23),
        Quat::IDENTITY,
        grip,
    );
    // Magazine — long angular box under the receiver.
    spawn_cuboid(
        commands,
        cube,
        root,
        0.050,
        0.10,
        0.070,
        Vec3::new(0.0, -0.07, -0.02),
        Quat::from_rotation_x(0.08),
        dark_body,
    );
    spawn_cuboid(
        commands,
        cube,
        root,
        0.004,
        0.065,
        0.055,
        Vec3::new(0.028, -0.07, -0.02),
        Quat::from_rotation_x(0.08),
        core,
    );
    // Top rail.
    spawn_cuboid(
        commands,
        cube,
        root,
        0.05,
        0.028,
        0.14,
        Vec3::new(0.0, 0.0, -0.02),
        Quat::IDENTITY,
        gunmetal,
    );
    // Scope body: thick tube with front + rear lens caps.
    spawn_cyl(
        commands,
        cube,
        root,
        scope_r,
        scope_len,
        Vec3::new(0.0, 0.088, -0.02),
        Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        dark_body,
    );
    // Scope collars are decorative; the scope body remains in Core.
    if detail.includes_decorative_parts() {
        for z in [-0.3_f32, 0.3] {
            spawn_cyl(
                commands,
                cube,
                root,
                scope_r + 0.008,
                0.014,
                Vec3::new(0.0, 0.088, -0.02 + scope_len * z),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                gunmetal,
            );
        }
    }
    // Single-cuboid lens caps keep the voxel optic readable and cheap.
    spawn_optic_lens(
        commands,
        cube,
        root,
        scope_r - 0.004,
        Vec3::new(0.0, 0.088, -0.02 - scope_len / 2.0),
        optic_glass,
    );
    spawn_optic_lens(
        commands,
        cube,
        root,
        scope_r - 0.004,
        Vec3::new(0.0, 0.088, -0.02 + scope_len / 2.0),
        optic_glass,
    );
    // Sniper extras: bipod + elevation turret knob.
    if is_sniper && detail.includes_decorative_parts() {
        spawn_cyl(
            commands,
            cube,
            root,
            0.014,
            0.025,
            Vec3::new(0.0, 0.108, -0.02),
            Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            gunmetal,
        );
        spawn_cyl(
            commands,
            cube,
            root,
            0.008,
            0.10,
            Vec3::new(0.028, -0.055, -0.30),
            Quat::from_rotation_z(0.45),
            gunmetal,
        );
        spawn_cyl(
            commands,
            cube,
            root,
            0.008,
            0.10,
            Vec3::new(-0.028, -0.055, -0.30),
            Quat::from_rotation_z(-0.45),
            gunmetal,
        );
    }
    // Accent emissive stripe along the side (the signature gun colour).
    spawn_cuboid(
        commands,
        cube,
        root,
        0.004,
        0.017,
        0.28,
        Vec3::new(0.047, 0.0, -0.06),
        Quat::IDENTITY,
        accent,
    );
    if detail.includes_decorative_parts() {
        spawn_cuboid(
            commands,
            cube,
            root,
            0.004,
            0.017,
            0.28,
            Vec3::new(-0.047, 0.0, -0.06),
            Quat::IDENTITY,
            accent,
        );
    }
    // Forward energy cell indicator (small bright tab on top).
    spawn_cuboid(
        commands,
        cube,
        root,
        0.010,
        0.012,
        0.020,
        Vec3::new(0.0, 0.030, -0.08),
        Quat::IDENTITY,
        core,
    );
}

fn spawn_cuboid(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    parent: Entity,
    sx: f32,
    sy: f32,
    sz: f32,
    pos: Vec3,
    rot: Quat,
    mat: &Handle<StandardMaterial>,
) {
    let tf = Transform {
        translation: pos,
        rotation: rot,
        scale: Vec3::new(sx, sy, sz),
    };
    commands.entity(parent).with_children(|p| {
        p.spawn(PbrBundle {
            mesh: cube.clone(),
            material: mat.clone(),
            transform: tf,
            ..default()
        });
    });
}

/// One opaque square lens, avoiding duplicate transparent surfaces in
/// the first-person view while retaining the voxel optic silhouette.
fn spawn_optic_lens(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    parent: Entity,
    radius: f32,
    pos: Vec3,
    mat: &Handle<StandardMaterial>,
) {
    let size = radius * 1.6;
    spawn_cuboid(
        commands,
        cube,
        parent,
        size,
        size,
        0.006,
        pos,
        Quat::IDENTITY,
        mat,
    );
}

/// A "voxel cylinder" — two interleaved square cuboids rotated 45°
/// around the barrel axis, giving an 8-sided pixel-prism silhouette
/// that matches the rest of the voxel aesthetic much better than a
/// smooth Bevy cylinder mesh. `rot` still rotates the cylinder axis
/// itself (caller controls orientation); we only add an extra spin
/// around the *local* axis for the second cuboid.
fn spawn_cyl(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    parent: Entity,
    radius: f32,
    height: f32,
    pos: Vec3,
    rot: Quat,
    mat: &Handle<StandardMaterial>,
) {
    // Square cross-section sized so the circumscribed circle matches `radius`.
    let s = radius * 2.0 * 0.78;
    let scale = Vec3::new(s, height, s);
    commands.entity(parent).with_children(|p| {
        p.spawn(PbrBundle {
            mesh: cube.clone(),
            material: mat.clone(),
            transform: Transform {
                translation: pos,
                rotation: rot,
                scale,
            },
            ..default()
        });
        p.spawn(PbrBundle {
            mesh: cube.clone(),
            material: mat.clone(),
            transform: Transform {
                translation: pos,
                rotation: rot * Quat::from_rotation_y(std::f32::consts::FRAC_PI_4),
                scale,
            },
            ..default()
        });
    });
}

// ---------------------------------------------------------------------
// Firing
// ---------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn fire_weapon(
    controls: FireControlParams,
    windows: Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
    toolbelt: Option<Res<crate::toolbelt::ToolbeltState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    world: Res<VoxelWorld>,
    scope: Res<ScopeState>,
    settings: Res<WorldSettings>,
    mut player_q: Query<(&Transform, &mut Player)>,
    mut weapon_q: Query<(&mut Weapon, &Transform)>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fx: ResMut<WeaponFxCache>,
    mut shake: ResMut<CameraShake>,
    mut stats: ResMut<DestructionStats>,
) {
    let cursor_locked = windows
        .get_single()
        .map(crate::mode::cursor_is_captured)
        .unwrap_or(false);
    let Ok((mut weapon, weapon_tf)) = weapon_q.get_single_mut() else {
        return;
    };
    let dt = controls.time.delta_seconds();
    weapon.cooldown = (weapon.cooldown - dt).max(0.0);
    weapon.recoil_t = (weapon.recoil_t - dt).max(0.0);
    let agent_fire = controls
        .agent
        .as_deref()
        .map(|agent| agent.active() && agent.fire)
        .unwrap_or(false);
    if !cursor_locked && !agent_fire {
        return;
    }
    if mode
        .as_deref()
        .map(|mode| !mode.allows_weapons())
        .unwrap_or_else(|| {
            toolbelt
                .as_deref()
                .map(|t| t.blocks_weapons())
                .unwrap_or(false)
        })
    {
        return;
    }
    let infinite = settings.cheats.infinite_ammo;
    if infinite {
        // Cheat: instantly refill the magazine and skip every reload.
        // The HUD counter still displays the value so the readout
        // stays meaningful, it just never falls below mag_size().
        if weapon.mag == 0 {
            weapon.mag = weapon.kind.mag_size();
        }
        weapon.reload_t = 0.0;
        weapon.reload_total = 0.0;
    } else {
        // Real reload gating: a reload in progress blocks fire, and an
        // empty mag must be reloaded explicitly via `reload_input`
        // (auto-triggered when the mag hits 0).
        if weapon.reload_t > 0.0 {
            return;
        }
        if weapon.mag == 0 {
            return;
        }
    }
    let wants_fire = if weapon.kind.auto() {
        controls.mouse.pressed(MouseButton::Left) || agent_fire
    } else {
        controls.mouse.just_pressed(MouseButton::Left) || agent_fire
    };
    if !wants_fire || weapon.cooldown > 0.0 {
        return;
    }
    // Spend a round.
    weapon.mag = weapon.mag.saturating_sub(1);
    let Ok((player_tf, mut player)) = player_q.get_single_mut() else {
        return;
    };
    let kind = weapon.kind;
    let presentation = kind.viewmodel_tuning();
    weapon.cooldown = kind.cooldown();
    let fx_scale = controls.budget.weapon_fx_scale.clamp(0.0, 1.0);

    // --- FUN JUICE ------------------------------------------------------
    // Each gun shakes the camera in proportion to its punch. The exact
    // values live on `WeaponKind::fire_shake` so per-weapon feel knobs
    // stay in one place.
    shake.add(kind.fire_shake());
    stats.shots_fired = stats.shots_fired.saturating_add(1);
    let shot_number = stats.shots_fired;
    weapon.recoil_t = kind.recoil_time();
    player.fov_bonus += kind.fov_kick();
    player.pitch = (player.pitch + kind.pitch_kick()).clamp(-1.54, 1.54);

    let muzzle = weapon_muzzle_world(player_tf, weapon_tf, presentation.muzzle_offset);

    // Muzzle point-light only (no sphere mesh — the laser bolt itself
    // carries the visible flash, and a world-space sphere in front of
    // the camera turned into an ugly persistent yellow ball during
    // full-auto fire).
    if should_spawn_muzzle_light(kind, fx_scale, shot_number) {
        let light_intensity = kind.muzzle_light_intensity() * fx_scale;
        commands.spawn((
            PointLightBundle {
                point_light: PointLight {
                    color: kind.color(),
                    intensity: light_intensity,
                    range: presentation.muzzle_light_range * fx_scale.clamp(0.55, 1.0),
                    shadows_enabled: false,
                    ..default()
                },
                transform: Transform::from_translation(muzzle),
                ..default()
            },
            MuzzleFlashLight {
                life: presentation.muzzle_light_life,
                max_life: presentation.muzzle_light_life,
                base_intensity: light_intensity,
            },
            Name::new("MuzzleFlashLight"),
        ));
    }

    let mut rng = ChaCha8Rng::seed_from_u64(
        (controls.time.elapsed_seconds_wrapped() * 100_000.0) as u64 ^ 0xFACE_FEED,
    );

    let forward = player_tf.forward();
    let base_dir = Vec3::new(forward.x, forward.y, forward.z).normalize_or_zero();
    // Aiming down sight tightens the shot pattern dramatically —
    // scoped full-auto still drifts a little so it doesn't feel
    // synthetic, but a scoped sniper/rifle is effectively pin-point.
    let spread_scale = 1.0 - 0.9 * scope.progress.clamp(0.0, 1.0);
    let effective_spread = kind.spread() * spread_scale;

    for pellet_index in 0..kind.pellets() {
        let camera_dir = if effective_spread > 0.0 {
            random_cone_dir(base_dir, effective_spread, &mut rng)
        } else {
            base_dir
        };

        // The camera ray is authoritative because the crosshair is centred on
        // it. The visual projectile then converges from the offset muzzle to
        // that target. A second DDA from the muzzle prevents firing through
        // close cover while preserving pixel-precise aim during Q/E rolls.
        let shot = solve_shot_path(
            &world,
            player_tf.translation,
            camera_dir,
            muzzle,
            WEAPON_MAX_RANGE,
        );

        // One short signature per trigger pull is enough to sell the muzzle.
        // Pellet weapons still launch every projectile without stacked additive
        // geometry at nearly identical positions.
        if pellet_index == 0 && fx_scale > 0.10 {
            let flash_end = muzzle + shot.direction * kind.tracer_fx().length;
            spawn_tracer(
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut fx,
                kind,
                muzzle,
                flash_end,
            );
        }

        // The travelling projectile carries this pellet's direction and hit.
        spawn_projectile(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut fx,
            muzzle,
            shot.direction,
            shot.travel_dist,
            shot.impact_pos,
            shot.hit_block,
            kind,
            fx_scale,
            pellet_index == 0 && should_spawn_projectile_light(kind, fx_scale, shot_number),
        );
    }
    // One thermal tick per trigger pull (not per pellet) — matches "laser drill" HUD.
    stats.drill_heat_pending += kind.drill_heat_per_shot();
}

fn flush_drill_heat_to_suit(
    mut stats: ResMut<DestructionStats>,
    mut suit: ResMut<crate::player::SuitVitals>,
) {
    let h = stats.drill_heat_pending;
    stats.drill_heat_pending = 0.0;
    if h > 0.0 {
        suit.laser_drill_charge = (suit.laser_drill_charge - h).max(0.0);
    }
}

fn random_cone_dir(base: Vec3, half_angle: f32, rng: &mut ChaCha8Rng) -> Vec3 {
    let cos_t = 1.0 - rng.gen::<f32>() * (1.0 - half_angle.cos());
    let sin_t = (1.0 - cos_t * cos_t).sqrt();
    let phi: f32 = rng.gen_range(0.0..std::f32::consts::TAU);
    let up = if base.y.abs() < 0.95 {
        Vec3::Y
    } else {
        Vec3::X
    };
    let tx = base.cross(up).normalize();
    let ty = base.cross(tx).normalize();
    (base * cos_t + (tx * phi.cos() + ty * phi.sin()) * sin_t).normalize()
}

fn break_blocks(
    world: &mut VoxelWorld,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut WeaponFxCache,
    cx: i32,
    cy: i32,
    cz: i32,
    radius: i32,
    kind: WeaponKind,
    rng: &mut ChaCha8Rng,
    fx_scale: f32,
    stats: &mut DestructionStats,
) -> u32 {
    let mut edit_batch = WorldEditBatch::default();
    let broken = break_blocks_inner(
        world,
        &mut edit_batch,
        commands,
        meshes,
        materials,
        fx,
        cx,
        cy,
        cz,
        radius,
        kind,
        rng,
        0,
        fx_scale,
        stats,
    );
    world.finish_edit_batch(edit_batch);
    broken
}

/// Internal break_blocks with a chain-depth guard — emissive/crystal
/// blocks caught in the blast radius detonate too (max 3 hops deep).
#[allow(clippy::too_many_arguments)]
fn break_blocks_inner(
    world: &mut VoxelWorld,
    edit_batch: &mut WorldEditBatch,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut WeaponFxCache,
    cx: i32,
    cy: i32,
    cz: i32,
    radius: i32,
    kind: WeaponKind,
    rng: &mut ChaCha8Rng,
    chain_depth: u32,
    fx_scale: f32,
    stats: &mut DestructionStats,
) -> u32 {
    use crate::blocks::{
        ore_units_for_mined_voxel, voxel_is_emissive, VOXEL_IRIDIUM, VOXEL_LUMINITE,
        VOXEL_MAGNETITE,
    };
    let r2 = (radius as f32 + 0.5).powi(2);
    let blast_center = Vec3::new(cx as f32 + 0.5, cy as f32 + 0.5, cz as f32 + 0.5);
    let blast_radius = radius.max(1) as f32 + 0.5;
    let mut broken = 0u32;
    let debris_cap = debris_spawn_cap(fx_scale);
    // Secondary blast locations — emissive ("crystal"/lava) voxels that
    // get caught in this explosion detonate after the main sphere
    // finishes clearing, so their shockwave propagates outward.
    let mut chain_sites: Vec<(i32, i32, i32)> = Vec::new();
    for dy in -radius..=radius {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let d2 = (dx * dx + dy * dy + dz * dz) as f32;
                if d2 > r2 {
                    continue;
                }
                let bx = cx + dx;
                let by = cy + dy;
                let bz = cz + dz;
                let v = world.voxel_at(bx, by, bz);
                if !voxel_is_weapon_target(v) {
                    continue;
                }
                // If this block glows (crystal/lava), memorise it as a
                // chain-explosion site BEFORE we clear it. Fun factor
                // skyrockets when shooting one crystal cascades through
                // a whole ore vein.
                if chain_depth < 3
                    && voxel_is_emissive(v)
                    && (dx * dx + dy * dy + dz * dz) as f32 >= r2 * 0.25
                {
                    chain_sites.push((bx, by, bz));
                }
                if world
                    .edit_set_voxel_batched(bx, by, bz, AIR, edit_batch)
                    .is_none()
                {
                    continue;
                }
                let units = ore_units_for_mined_voxel(v);
                if units > 0 {
                    match v {
                        VOXEL_LUMINITE => {
                            stats.luminite_units =
                                stats.luminite_units.saturating_add(u64::from(units));
                        }
                        VOXEL_MAGNETITE => {
                            stats.magnetite_units =
                                stats.magnetite_units.saturating_add(u64::from(units));
                        }
                        VOXEL_IRIDIUM => {
                            stats.iridium_units =
                                stats.iridium_units.saturating_add(u64::from(units));
                        }
                        _ => {}
                    }
                }
                broken = broken.saturating_add(1);
                if broken <= debris_cap {
                    spawn_debris(
                        commands,
                        meshes,
                        materials,
                        fx,
                        bx,
                        by,
                        bz,
                        v,
                        Some((blast_center, blast_radius)),
                        rng,
                    );
                }
            }
        }
    }
    // ----------------------------------------------------------------
    // Support analysis: within a bounded box around the blast, do a
    // BFS through all solid voxels starting from ones that are
    // "grounded" — i.e., attached to solid terrain OUTSIDE the box or
    // sitting on solid ground at the bottom edge. Every solid voxel
    // in the box NOT reached by the BFS has lost its structural
    // connection and falls as an entity.
    //
    // This correctly handles the user's ask: shoot out the base of a
    // pillar → everything above falls (because the BFS can no longer
    // reach those upper voxels from ground). It also handles
    // horizontal overhangs — the cap of a hill whose pillar was
    // severed detaches as a slab.
    //
    // Bounded so it never touches natural floating Karst formations
    // that weren't affected by the blast.
    let margin_xz = 3_i32;
    let margin_up = (radius + 72).max(32);
    let margin_down = 2_i32;
    let x0 = cx - radius - margin_xz;
    let x1 = cx + radius + margin_xz;
    let y0 = cy - radius - margin_down;
    let y1 = cy + radius + margin_up;
    let z0 = cz - radius - margin_xz;
    let z1 = cz + radius + margin_xz;
    let sx = (x1 - x0 + 1) as usize;
    let sy = (y1 - y0 + 1) as usize;
    let sz = (z1 - z0 + 1) as usize;
    let idx = |bx: i32, by: i32, bz: i32| -> usize {
        let lx = (bx - x0) as usize;
        let ly = (by - y0) as usize;
        let lz = (bz - z0) as usize;
        (ly * sz + lz) * sx + lx
    };
    let cell_count = sx * sy * sz;
    let mut solid = vec![false; cell_count];
    let mut supported = vec![false; cell_count];
    for by in y0..=y1 {
        for bz in z0..=z1 {
            for bx in x0..=x1 {
                if voxel_is_solid(world.voxel_at(bx, by, bz)) {
                    solid[idx(bx, by, bz)] = true;
                }
            }
        }
    }
    // Seed BFS with all solid voxels whose neighbour OUTSIDE the box
    // is solid (anchored to surrounding terrain) or whose neighbour
    // at the very bottom layer of the box is solid below (rooted to
    // the ground below our analysis region).
    let mut queue: std::collections::VecDeque<(i32, i32, i32)> = std::collections::VecDeque::new();
    let is_solid_world = |bx: i32, by: i32, bz: i32| voxel_is_solid(world.voxel_at(bx, by, bz));
    for by in y0..=y1 {
        for bz in z0..=z1 {
            for bx in x0..=x1 {
                if !solid[idx(bx, by, bz)] {
                    continue;
                }
                // On the box border, check the voxel just OUTSIDE.
                let mut anchored = false;
                if bx == x0 && is_solid_world(bx - 1, by, bz) {
                    anchored = true;
                }
                if bx == x1 && is_solid_world(bx + 1, by, bz) {
                    anchored = true;
                }
                if bz == z0 && is_solid_world(bx, by, bz - 1) {
                    anchored = true;
                }
                if bz == z1 && is_solid_world(bx, by, bz + 1) {
                    anchored = true;
                }
                if by == y0 && is_solid_world(bx, by - 1, bz) {
                    anchored = true;
                }
                if anchored {
                    supported[idx(bx, by, bz)] = true;
                    queue.push_back((bx, by, bz));
                }
            }
        }
    }
    // Flood through solid voxels (6-connected).
    while let Some((bx, by, bz)) = queue.pop_front() {
        for (dx, dy, dz) in [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ] {
            let nx = bx + dx;
            let ny = by + dy;
            let nz = bz + dz;
            if nx < x0 || nx > x1 || ny < y0 || ny > y1 || nz < z0 || nz > z1 {
                continue;
            }
            let i = idx(nx, ny, nz);
            if !solid[i] || supported[i] {
                continue;
            }
            supported[i] = true;
            queue.push_back((nx, ny, nz));
        }
    }
    // Every solid voxel NOT marked supported falls.
    let falling_cap = falling_block_spawn_cap(kind, fx_scale);
    let mut spawned_falling: u32 = 0;
    for by in y0..=y1 {
        for bz in z0..=z1 {
            for bx in x0..=x1 {
                if spawned_falling >= falling_cap {
                    break;
                }
                let i = idx(bx, by, bz);
                if !solid[i] || supported[i] {
                    continue;
                }
                let v = world.voxel_at(bx, by, bz);
                if !voxel_is_solid(v) {
                    continue;
                }
                if world
                    .edit_set_voxel_batched(bx, by, bz, AIR, edit_batch)
                    .is_none()
                {
                    continue;
                }
                let units = ore_units_for_mined_voxel(v);
                if units > 0 {
                    match v {
                        VOXEL_LUMINITE => {
                            stats.luminite_units =
                                stats.luminite_units.saturating_add(u64::from(units));
                        }
                        VOXEL_MAGNETITE => {
                            stats.magnetite_units =
                                stats.magnetite_units.saturating_add(u64::from(units));
                        }
                        VOXEL_IRIDIUM => {
                            stats.iridium_units =
                                stats.iridium_units.saturating_add(u64::from(units));
                        }
                        _ => {}
                    }
                }
                spawn_falling_block(
                    commands,
                    meshes,
                    materials,
                    fx,
                    bx,
                    by,
                    bz,
                    v,
                    Some((blast_center, blast_radius)),
                    rng,
                );
                spawned_falling += 1;
            }
        }
    }
    // --- CHAIN REACTION -------------------------------------------------
    // Detonate every emissive voxel caught in this blast with a small
    // secondary explosion. Depth is capped so a crystal-rich cave
    // can't recurse forever.
    if chain_depth < 3 {
        // Limit fan-out per tier to avoid exponential blowup.
        let max_chains = match chain_depth {
            0 => 3,
            1 => 2,
            _ => 1,
        };
        let mut chains_used = 0;
        for (cbx, cby, cbz) in chain_sites.into_iter() {
            if chains_used >= max_chains {
                break;
            }
            chains_used += 1;
            let pos = Vec3::new(cbx as f32 + 0.5, cby as f32 + 0.5, cbz as f32 + 0.5);
            spawn_explosion(
                commands,
                meshes,
                materials,
                fx,
                pos,
                (radius - 1).max(2),
                kind,
                fx_scale,
            );
            broken = broken.saturating_add(break_blocks_inner(
                world,
                edit_batch,
                commands,
                meshes,
                materials,
                fx,
                cbx,
                cby,
                cbz,
                (radius - 1).max(2),
                kind,
                rng,
                chain_depth + 1,
                fx_scale,
                stats,
            ));
        }
    }
    broken
}

fn spawn_debris(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut WeaponFxCache,
    bx: i32,
    by: i32,
    bz: i32,
    voxel: crate::blocks::Voxel,
    blast: Option<(Vec3, f32)>,
    rng: &mut ChaCha8Rng,
) {
    // Material + meshes come from the shared cache — per-voxel-type
    // lookup for the material, single global meshes for each piece
    // size. Previously this function added ~3 new mesh handles plus
    // a new material EVERY call; with the cache a full blast radius
    // now re-uses the SAME handles for every block it touches.
    let hot_mat = fx.debris_mat_for(voxel, materials);
    let chunk = fx.debris_chunk_mesh_shared(meshes);
    let shard = fx.debris_shard_mesh_shared(meshes);
    let dust = fx.debris_dust_mesh_shared(meshes);
    let centre = Vec3::new(bx as f32 + 0.5, by as f32 + 0.5, bz as f32 + 0.5);
    // 14 pieces total per block: 3 big + 5 shard + 6 dust.
    let layout: &[(u32, &Handle<Mesh>, f32, f32, f32)] = &[
        (3, &chunk, 3.0, 6.0, 1.4),
        (5, &shard, 4.0, 9.0, 1.0),
        (6, &dust, 6.0, 13.0, 0.7),
    ];
    for (count, mesh, vmin, vmax, life) in layout {
        for _ in 0..*count {
            let offs = Vec3::new(
                rng.gen_range(-0.30..0.30),
                rng.gen_range(-0.30..0.30),
                rng.gen_range(-0.30..0.30),
            );
            let spawn_pos = centre + offs;
            let dir = (offs + Vec3::new(0.0, 0.45, 0.0)).normalize_or_zero();
            let speed: f32 = rng.gen_range(*vmin..*vmax);
            let mut vel = dir * speed;
            if let Some((blast_center, blast_radius)) = blast {
                vel += blast_impulse(
                    blast_center,
                    spawn_pos,
                    blast_radius,
                    *vmax * 1.35,
                    0.85,
                    rng,
                );
            }
            let spin = Vec3::new(
                rng.gen_range(-14.0..14.0),
                rng.gen_range(-14.0..14.0),
                rng.gen_range(-14.0..14.0),
            );
            commands.spawn((
                PbrBundle {
                    mesh: (*mesh).clone(),
                    material: hot_mat.clone(),
                    transform: Transform::from_translation(spawn_pos),
                    ..default()
                },
                Debris {
                    velocity: vel,
                    life: *life,
                    max_life: *life,
                    spin,
                },
                Name::new("Debris"),
            ));
        }
    }
}

/// Spawn a full 1×1×1 voxel as a physics entity that falls and
/// shatters on impact. Unlike normal debris this preserves the original
/// block color so falling rock really looks like a block tumbling down.
fn spawn_falling_block(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut WeaponFxCache,
    bx: i32,
    by: i32,
    bz: i32,
    voxel: crate::blocks::Voxel,
    blast: Option<(Vec3, f32)>,
    rng: &mut ChaCha8Rng,
) {
    let mat = fx.falling_mat_for(voxel, materials);
    let mesh = fx.falling_mesh_shared(meshes);
    let centre = Vec3::new(bx as f32 + 0.5, by as f32 + 0.5, bz as f32 + 0.5);
    // Small random kick so a stack of freshly-unsupported blocks
    // doesn't descend as a single rigid plate — each tumbles a bit.
    let mut vel = Vec3::new(
        rng.gen_range(-0.6..0.6),
        rng.gen_range(-0.4..0.1),
        rng.gen_range(-0.6..0.6),
    );
    if let Some((blast_center, blast_radius)) = blast {
        vel += blast_impulse(blast_center, centre, blast_radius, 9.5, 0.55, rng);
    }
    let spin = Vec3::new(
        rng.gen_range(-1.2..1.2),
        rng.gen_range(-1.2..1.2),
        rng.gen_range(-1.2..1.2),
    );
    commands.spawn((
        PbrBundle {
            mesh,
            material: mat,
            transform: Transform::from_translation(centre),
            ..default()
        },
        FallingBlock {
            velocity: vel,
            spin,
            voxel,
            max_fall_time: 6.0,
        },
        Name::new("FallingBlock"),
    ));
}

fn blast_impulse(
    blast_center: Vec3,
    pos: Vec3,
    blast_radius: f32,
    strength: f32,
    upward_bias: f32,
    rng: &mut ChaCha8Rng,
) -> Vec3 {
    let mut dir = pos - blast_center;
    if dir.length_squared() < 0.0001 {
        dir = Vec3::new(
            rng.gen_range(-0.35..0.35),
            rng.gen_range(0.15..0.85),
            rng.gen_range(-0.35..0.35),
        );
    }
    let dist = dir.length().max(0.001);
    let falloff = (1.0 - dist / blast_radius.max(0.25)).clamp(0.15, 1.0);
    let radial = dir / dist;
    let impulse = radial * (strength * falloff);
    impulse + Vec3::Y * (strength * upward_bias * (0.35 + falloff * 0.65))
}

fn spawn_tracer(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut WeaponFxCache,
    kind: WeaponKind,
    a: Vec3,
    b: Vec3,
) {
    // Short muzzle-flash tracer only. Each weapon gets its own fixed
    // silhouette so the muzzle signature is readable before the bolt
    // itself crosses the scene.
    let delta = b - a;
    let len = delta.length();
    if len < 0.3 {
        return;
    }
    let profile = kind.tracer_fx();
    let mid = a + delta.normalize_or_zero() * (len * 0.5);
    let rot = Quat::from_rotation_arc(Vec3::Y, delta.normalize_or_zero());
    let mat = fx.tracer_mat_for(kind, materials);
    let mesh = fx.tracer_mesh_for(kind, meshes);
    commands.spawn((
        PbrBundle {
            mesh,
            material: mat,
            transform: Transform {
                translation: mid,
                rotation: rot,
                ..default()
            },
            ..default()
        },
        Tracer {
            life: profile.life,
            max_life: profile.life,
        },
        Name::new("MuzzleTracer"),
    ));
}

#[allow(clippy::too_many_arguments)]
fn spawn_projectile(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut WeaponFxCache,
    origin: Vec3,
    dir: Vec3,
    travel_dist: f32,
    impact_pos: Vec3,
    hit_block: Option<(i32, i32, i32)>,
    kind: WeaponKind,
    fx_scale: f32,
    spawn_light: bool,
) {
    let speed = kind.projectile_speed();
    let is_explosive = matches!(
        kind,
        WeaponKind::RocketLauncher | WeaponKind::GrenadeLauncher
    );
    let ndir = dir.normalize_or_zero();
    let spawn_pos = origin + ndir * 0.6;

    let (bolt_len, _bolt_radius) = kind.bolt_dims();
    let halo_mesh = fx.halo_mesh_for(kind, meshes);
    let halo_mat = fx.halo_mat_for(kind, materials);
    let core_mesh = fx.core_mesh_for(kind, meshes);
    let core_mat = fx.core_mat_for(kind, materials);
    // Orientation: Cuboid's long axis is +Z (we made z = bolt_len).
    // Rotate Z onto ndir (instead of Y onto ndir).
    let rot = Quat::from_rotation_arc(Vec3::Z, ndir);
    let entity_name = if is_explosive {
        "RocketBolt"
    } else {
        "LaserBolt"
    };
    let remaining = (travel_dist - 0.6).max(0.0);
    let visual_layers = projectile_visual_layer_count(fx_scale);
    let root_mat = if visual_layers == 1 {
        core_mat.clone()
    } else {
        halo_mat
    };
    let mut proj = commands.spawn((
        PbrBundle {
            mesh: halo_mesh.clone(),
            material: root_mat,
            transform: Transform {
                translation: spawn_pos,
                rotation: rot,
                ..default()
            },
            ..default()
        },
        Projectile {
            dir: ndir,
            speed,
            life: projectile_lifetime_secs(remaining, speed),
            remaining,
            kind,
            hit_block,
            impact_pos,
        },
        Name::new(entity_name),
    ));
    let warhead_mesh = if is_explosive {
        Some(fx.warhead_mesh_for(kind, meshes))
    } else {
        None
    };
    let warhead_mat = if is_explosive {
        Some(fx.warhead_mat_shared(materials))
    } else {
        None
    };
    proj.with_children(|p| {
        // Second halo rotated 45° around Z for octagonal silhouette.
        if visual_layers >= 3 {
            p.spawn(PbrBundle {
                mesh: halo_mesh,
                material: core_mat.clone(),
                transform: Transform {
                    rotation: Quat::from_rotation_z(std::f32::consts::FRAC_PI_4),
                    ..default()
                },
                ..default()
            });
        }
        // Inner white-hot pill.
        if visual_layers >= 2 {
            p.spawn(PbrBundle {
                mesh: core_mesh,
                material: core_mat,
                transform: Transform::default(),
                ..default()
            });
        }
        // Keep the wall-lighting pass for signature shots, but cadence
        // it by runtime tier so automatic fire does not create one live
        // dynamic light per projectile.
        if spawn_light {
            p.spawn(PointLightBundle {
                point_light: PointLight {
                    color: kind.color(),
                    intensity: if is_explosive { 900_000.0 } else { 400_000.0 },
                    range: if is_explosive { 22.0 } else { 16.0 },
                    shadows_enabled: false,
                    ..default()
                },
                transform: Transform::default(),
                ..default()
            });
        }
        // Explosive warhead: chunky glowing cube at the tip.
        if let (Some(orb_mesh), Some(orb_mat)) = (warhead_mesh, warhead_mat) {
            // Tip of the bolt in local space (+Z half-length).
            p.spawn(PbrBundle {
                mesh: orb_mesh,
                material: orb_mat,
                transform: Transform::from_xyz(0.0, 0.0, bolt_len * 0.5),
                ..default()
            });
        }
    });
}

fn spawn_impact_puff(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut WeaponFxCache,
    kind: WeaponKind,
    pos: Vec3,
    fx_scale: f32,
) {
    if fx_scale <= 0.10 {
        return;
    }
    let profile = kind.impact_fx();
    let mat = fx.puff_mat_for(kind, materials);
    let halo_mat = fx.halo_mat_for(kind, materials);
    let mesh = fx.impact_puff_mesh_shared(meshes);
    commands.spawn((
        PbrBundle {
            mesh: mesh.clone(),
            material: mat,
            transform: Transform {
                translation: pos,
                scale: Vec3::splat(profile.puff_start_scale),
                ..default()
            },
            ..default()
        },
        MuzzleFlash {
            life: profile.puff_life * fx_scale.max(0.35),
            max_life: profile.puff_life * fx_scale.max(0.35),
            start_scale: profile.puff_start_scale,
            end_scale: profile.puff_end_scale * fx_scale.clamp(0.45, 1.0),
        },
        Name::new("ImpactPuff"),
    ));
    if profile.halo_life > 0.0 && fx_scale > 0.35 {
        commands.spawn((
            PbrBundle {
                mesh,
                material: halo_mat,
                transform: Transform {
                    translation: pos,
                    scale: Vec3::splat(profile.halo_start_scale),
                    ..default()
                },
                ..default()
            },
            MuzzleFlash {
                life: profile.halo_life * fx_scale,
                max_life: profile.halo_life * fx_scale,
                start_scale: profile.halo_start_scale,
                end_scale: profile.halo_end_scale * fx_scale,
            },
            Name::new("ImpactHalo"),
        ));
    }
    if profile.light_intensity > 0.0 && fx_scale > 0.45 {
        let light_life = profile.puff_life.max(profile.halo_life).max(0.07);
        commands.spawn((
            PointLightBundle {
                point_light: PointLight {
                    color: kind.color(),
                    intensity: profile.light_intensity * fx_scale,
                    range: profile.light_range * fx_scale.clamp(0.55, 1.0),
                    shadows_enabled: false,
                    ..default()
                },
                transform: Transform::from_translation(pos),
                ..default()
            },
            MuzzleFlashLight {
                life: light_life,
                max_life: light_life,
                base_intensity: profile.light_intensity * fx_scale,
            },
            Name::new("ImpactLight"),
        ));
    }
}

fn spawn_explosion(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut WeaponFxCache,
    pos: Vec3,
    radius: i32,
    kind: WeaponKind,
    fx_scale: f32,
) {
    let r = (radius as f32).max(2.0);
    let profile = kind.explosion_fx();
    // Per-kind sphere material is cached; the RPG's classic warm
    // orange uses the legacy shared handle so the asset graph stays
    // backwards-compatible. Both paths now hit the cache instead of
    // allocating a fresh `StandardMaterial` per blast.
    let mat = if matches!(kind, WeaponKind::RocketLauncher) {
        fx.explosion_mat_shared(materials)
    } else {
        fx.explosion_mat_for(kind, materials)
    };
    let mesh = fx.explosion_mesh_shared(meshes);
    let sphere_life = profile.sphere_life;
    commands.spawn((
        PbrBundle {
            mesh,
            material: mat,
            transform: Transform::from_translation(pos),
            ..default()
        },
        Explosion {
            life: sphere_life * fx_scale.max(0.35),
            max_life: sphere_life * fx_scale.max(0.35),
            max_scale: r * profile.sphere_scale_mul * fx_scale.clamp(0.45, 1.0),
        },
        Name::new("Explosion"),
    ));
    if fx_scale > 0.35 {
        commands.spawn((
            PointLightBundle {
                point_light: PointLight {
                    color: Color::srgb(
                        profile.light_rgb.x,
                        profile.light_rgb.y,
                        profile.light_rgb.z,
                    ),
                    intensity: profile.light_intensity * fx_scale,
                    range: r * 6.0 * fx_scale.clamp(0.55, 1.0),
                    shadows_enabled: false,
                    ..default()
                },
                transform: Transform::from_translation(pos),
                ..default()
            },
            MuzzleFlashLight {
                life: sphere_life * fx_scale,
                max_life: sphere_life * fx_scale,
                base_intensity: profile.light_intensity * fx_scale,
            },
            Name::new("ExplosionLight"),
        ));
    }

    // --- SHOCKWAVE RING -------------------------------------------------
    // Flat disc that scales outward + fades. The mesh is cached
    // globally; the material has to stay per-instance because
    // `update_shockwaves` mutates emissive/alpha as the ring expands.
    let ring_mesh = fx.shockwave_mesh_shared(meshes);
    if fx_scale > 0.25 {
        let ring_mat = materials.add(StandardMaterial {
            base_color: Color::srgba(
                profile.ring_base_rgb.x,
                profile.ring_base_rgb.y,
                profile.ring_base_rgb.z,
                1.0,
            ),
            emissive: LinearRgba::rgb(
                profile.ring_emissive_rgb.x,
                profile.ring_emissive_rgb.y,
                profile.ring_emissive_rgb.z,
            ),
            unlit: true,
            alpha_mode: AlphaMode::Add,
            ..default()
        });
        commands.spawn((
            PbrBundle {
                mesh: ring_mesh,
                material: ring_mat,
                transform: Transform::from_translation(pos + Vec3::Y * 0.1),
                ..default()
            },
            Shockwave {
                life: 0.5 * fx_scale.max(0.45),
                max_life: 0.5 * fx_scale.max(0.45),
                max_scale: r * 5.5 * fx_scale.clamp(0.45, 1.0),
                base_rgb: profile.ring_base_rgb,
                emissive_rgb: profile.ring_emissive_rgb,
            },
            Name::new("Shockwave"),
        ));
    }

    // --- FULL-SCREEN FLASH ---------------------------------------------
    // Push onto the shared queue instead of spawning a new overlay
    // entity per blast. `update_screen_flash` collapses every request
    // for the frame into the single persistent flash node and takes
    // the strongest colour/alpha so chain reactions stay readable.
    if fx_scale > 0.40 {
        fx.pending_flashes.push((
            profile.flash_rgb,
            profile.flash_alpha * fx_scale,
            0.28 * fx_scale,
        ));
    }
}

// ---------------------------------------------------------------------
// Per-frame transient updates
// ---------------------------------------------------------------------

fn animate_viewmodel(
    time: Res<Time>,
    scope: Res<ScopeState>,
    active: Res<ActiveWeapon>,
    toolbelt: Option<Res<crate::toolbelt::ToolbeltState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    mut holster: ResMut<WeaponHolster>,
    mut q: Query<(&mut Transform, &Weapon, &WeaponRestPose, &mut Visibility)>,
) {
    // elapsed_seconds_wrapped wraps at 3600s so sin/cos phases keep
    // sub-millisecond precision even after 10h+ sessions (plain
    // elapsed_seconds at 36000s has only ~4ms resolution, making
    // the viewmodel visibly jitter).
    let t = time.elapsed_seconds_wrapped();
    let hide_for_edit = mode
        .as_deref()
        .map(|mode| !mode.allows_weapons())
        .unwrap_or_else(|| {
            toolbelt
                .as_deref()
                .map(|toolbelt| toolbelt.blocks_weapons())
                .unwrap_or(false)
        });
    let holster_target = if hide_for_edit { 1.0 } else { 0.0 };
    let holster_speed = if hide_for_edit { 7.0 } else { 6.0 };
    let holster_k = (time.delta_seconds() * holster_speed).min(1.0);
    holster.progress += (holster_target - holster.progress) * holster_k;
    let holster_t = holster.progress * holster.progress * (3.0 - 2.0 * holster.progress);
    for (mut tf, weapon, rest, mut vis) in q.iter_mut() {
        let rest_tf = rest.0;
        // ------------------------------------------------------------
        // Recoil kick (sharp forward+upward punch after a shot).
        // ------------------------------------------------------------
        let presentation = weapon.kind.viewmodel_tuning();
        let kick = viewmodel_recoil_amount(weapon.recoil_t, weapon.kind.recoil_time());
        let recoil_offset = presentation.recoil_offset * kick;
        let recoil_rot = Quat::from_rotation_x(-presentation.recoil_pitch * kick);

        // ------------------------------------------------------------
        // ADS pose: centre the gun on the crosshair and pull back so
        // the scope glass (or iron sights) sit right on the eye line.
        // Blended by `scope.progress` (0 hip, 1 scoped).
        // ------------------------------------------------------------
        let ads = scope.progress.clamp(0.0, 1.0);
        // Per-weapon ADS offset: rifles/sniper line up scope directly
        // above centre, launchers stay slightly offset so the huge
        // barrel doesn't eat the whole screen.
        let (ads_x, ads_y) = match weapon.kind {
            WeaponKind::Sniper | WeaponKind::AssaultRifle | WeaponKind::Blaster => (0.0, -0.085),
            WeaponKind::RocketLauncher | WeaponKind::GrenadeLauncher => (0.05, -0.12),
            _ => (0.0, -0.07),
        };
        let ads_trans = Vec3::new(ads_x, ads_y, rest_tf.translation.z * 0.55);
        let ads_rot = Quat::IDENTITY;

        // ------------------------------------------------------------
        // Reload pose: drop the gun down, tilt it outward, then ease
        // back up. Triangular curve (0 → 1 → 0) over the reload time
        // so the motion swings down in the first half and returns in
        // the second half, reading clearly as "eject + slam mag".
        // ------------------------------------------------------------
        let reload_amt = if weapon.reload_total > 0.0 {
            let x = 1.0 - (weapon.reload_t / weapon.reload_total).clamp(0.0, 1.0);
            // Smooth triangle: peaks at x = 0.5.
            let tri = 1.0 - (2.0 * x - 1.0).abs();
            tri * tri * (3.0 - 2.0 * tri)
        } else {
            0.0
        };
        // Extra high-frequency wobble so the reload reads "mechanical"
        // instead of a single swing — slight shake while the mag swaps.
        let shake = if reload_amt > 0.05 {
            let s = (time.elapsed_seconds_wrapped() * 28.0).sin() * 0.008 * reload_amt;
            Vec3::new(s, -s * 0.6, 0.0)
        } else {
            Vec3::ZERO
        };
        let reload_trans = Vec3::new(-0.08 * reload_amt, -0.28 * reload_amt, 0.08 * reload_amt);
        let reload_rot = Quat::from_rotation_x(-0.9 * reload_amt)
            * Quat::from_rotation_z(0.45 * reload_amt)
            * Quat::from_rotation_y(-0.25 * reload_amt);

        // ------------------------------------------------------------
        // Idle sway only matters when hip-firing; it looks wrong when
        // the gun is clamped to the centre for scope aiming.
        // ------------------------------------------------------------
        let sway = Vec3::new((t * 0.7).sin() * 0.003, (t * 0.9).cos() * 0.004, 0.0) * (1.0 - ads);

        let base_translation = rest_tf.translation.lerp(ads_trans, ads);
        let base_rotation = rest_tf.rotation.slerp(ads_rot, ads);

        let holster_trans = Vec3::new(0.34, -0.74, 0.20) * holster_t;
        let holster_rot = Quat::from_rotation_x(-0.85 * holster_t)
            * Quat::from_rotation_y(0.35 * holster_t)
            * Quat::from_rotation_z(-0.22 * holster_t);

        let target_translation =
            base_translation + recoil_offset + sway + reload_trans + shake + holster_trans;
        let target_rotation = holster_rot * reload_rot * recoil_rot * base_rotation;

        let k = (time.delta_seconds() * 15.0).min(1.0);
        tf.translation = tf.translation.lerp(target_translation, k);
        tf.rotation = tf.rotation.slerp(target_rotation, k);

        // Hide the viewmodel once fully scoped for EVERY weapon.
        // Most scopes aren't actually see-through glass (the barrel
        // + body + receiver would block the crosshair), so fading
        // the whole gun away at full ADS gives a clean, precise
        // aimed view. Sniper also hands over to its HUD overlay here.
        let hide_for_scope = ads > 0.9;
        let _ = active.kind;
        *vis = if hide_for_scope || (hide_for_edit && holster_t > 0.96) {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
    }
}

/// Right-click toggles aim-down-sight. While scoped: FOV shrinks by
/// the weapon's `ads_zoom`, mouse sensitivity drops proportionally,
/// shot spread collapses. The sniper additionally honours the scroll
/// wheel as a high-precision zoom control capable of reaching ~60× for
/// picking out single blocks ~15 chunks (240 blocks) away.
fn scope_input(
    mouse: Res<ButtonInput<MouseButton>>,
    agent: Option<Res<crate::agent_control::AgentControlState>>,
    mut wheel: EventReader<MouseWheel>,
    windows: Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
    toolbelt: Option<Res<crate::toolbelt::ToolbeltState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    time: Res<Time>,
    active: Res<ActiveWeapon>,
    mut scope: ResMut<ScopeState>,
    weapon_q: Query<&Weapon>,
) {
    let cursor_locked = windows
        .get_single()
        .map(crate::mode::cursor_is_captured)
        .unwrap_or(false);
    let reloading = weapon_q
        .get_single()
        .map(|w| w.reload_t > 0.0)
        .unwrap_or(false);
    let tool_blocks = mode
        .as_deref()
        .map(|mode| !mode.allows_weapons())
        .unwrap_or_else(|| {
            toolbelt
                .as_deref()
                .map(|t| t.blocks_weapons())
                .unwrap_or(false)
        });
    let agent_scope = agent
        .as_deref()
        .map(|agent| agent.active() && agent.scope)
        .unwrap_or(false);
    scope.active = !tool_blocks
        && !reloading
        && ((cursor_locked && mouse.pressed(MouseButton::Right)) || agent_scope);

    let dt = time.delta_seconds();
    let blend = (dt * 10.0).min(1.0);
    let target_progress = if scope.active { 1.0 } else { 0.0 };
    scope.progress += (target_progress - scope.progress) * blend;

    // Sniper-only: scroll wheel adjusts precision multiplier while scoped.
    let is_sniper = active.kind == WeaponKind::Sniper;
    for ev in wheel.read() {
        if is_sniper && scope.active {
            scope.sniper_zoom = (scope.sniper_zoom + ev.y * 0.3).clamp(1.0, 4.0);
        }
    }
    // Reset sniper zoom when leaving scope so every new ADS starts
    // at base magnification instead of the last extreme value.
    if !scope.active && scope.progress < 0.05 {
        scope.sniper_zoom = 1.0;
    }

    let target_zoom = if scope.progress > 0.001 {
        let base = active.kind.ads_zoom();
        let extra = if is_sniper { scope.sniper_zoom } else { 1.0 };
        1.0 + (base * extra - 1.0) * scope.progress
    } else {
        1.0
    };
    scope.current_zoom += (target_zoom - scope.current_zoom) * blend;
    if scope.current_zoom < 1.0 {
        scope.current_zoom = 1.0;
    }
}

/// Reload control. With `cheats.infinite_ammo == true` (default) the
/// magazine refills instantly and no reload animation runs. With it
/// off, this system implements proper reload gating: the `R` key (or
/// auto-trigger on empty mag) starts a per-weapon reload countdown
/// that blocks fire until it completes.
fn reload_input(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    state: Res<State<GameState>>,
    settings: Res<WorldSettings>,
    mut weapon_q: Query<&mut Weapon>,
) {
    let Ok(mut w) = weapon_q.get_single_mut() else {
        return;
    };
    if settings.cheats.infinite_ammo {
        // Cheat path: cancel any ongoing reload and keep the magazine
        // permanently topped up. Matches the engine's classic
        // "infinite energy cells" behaviour.
        w.reload_t = 0.0;
        w.reload_total = 0.0;
        let full = w.kind.mag_size();
        if w.mag < full {
            w.mag = full;
        }
        return;
    }

    // Real reload path.
    let dt = time.delta_seconds();
    if w.reload_t > 0.0 {
        w.reload_t = (w.reload_t - dt).max(0.0);
        if w.reload_t == 0.0 {
            w.mag = w.kind.mag_size();
            w.reload_total = 0.0;
        }
        return;
    }

    let in_game = matches!(state.get(), GameState::InGame);
    let manual = in_game && keys.just_pressed(KeyCode::KeyR);
    let auto = w.mag == 0;
    if (manual && w.mag < w.kind.mag_size()) || auto {
        w.reload_total = w.kind.reload_time();
        w.reload_t = w.reload_total;
    }
}

/// Admin-gated cheat keybinds. Held modifiers are required so the
/// chord is not triggered by accident:
///   * Ctrl+Shift+A  toggles `admin_mode` (the master gate).
///   * Ctrl+I        toggles `infinite_ammo` (only when admin_mode on).
/// Each press logs the new state so the player has a clear confirmation.
fn cheat_keybinds(keys: Res<ButtonInput<KeyCode>>, mut settings: ResMut<WorldSettings>) {
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);

    if ctrl && shift && keys.just_pressed(KeyCode::KeyA) {
        settings.cheats.admin_mode = !settings.cheats.admin_mode;
        info!(
            "Admin mode {}",
            if settings.cheats.admin_mode {
                "ENABLED"
            } else {
                "disabled"
            }
        );
    }

    if settings.cheats.admin_mode && ctrl && !shift && keys.just_pressed(KeyCode::KeyI) {
        settings.cheats.infinite_ammo = !settings.cheats.infinite_ammo;
        info!(
            "Infinite ammo {}",
            if settings.cheats.infinite_ammo {
                "ON"
            } else {
                "OFF"
            }
        );
    }
}

fn update_muzzle_flash(
    time: Res<Time>,
    mut commands: Commands,
    mut flash_q: Query<(Entity, &mut MuzzleFlash, &mut Transform)>,
    mut light_q: Query<(Entity, &mut MuzzleFlashLight, &mut PointLight)>,
) {
    for (e, mut flash, mut tf) in flash_q.iter_mut() {
        flash.life -= time.delta_seconds();
        let s = (flash.life / flash.max_life).clamp(0.0, 1.0);
        let t = 1.0 - s;
        let scale = flash.start_scale + (flash.end_scale - flash.start_scale) * t;
        tf.scale = Vec3::splat(scale.max(0.001));
        if flash.life <= 0.0 {
            despawn_recursive_if_exists(&mut commands, e);
        }
    }
    for (e, mut light, mut pl) in light_q.iter_mut() {
        light.life -= time.delta_seconds();
        let s = (light.life / light.max_life).clamp(0.0, 1.0);
        pl.intensity = light.base_intensity * s;
        if light.life <= 0.0 {
            despawn_recursive_if_exists(&mut commands, e);
        }
    }
}

fn update_tracers(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Tracer, &mut Transform)>,
) {
    for (e, mut tr, mut tf) in q.iter_mut() {
        tr.life -= time.delta_seconds();
        let s = (tr.life / tr.max_life).clamp(0.0, 1.0);
        tf.scale = Vec3::new(s, 1.0, s);
        if tr.life <= 0.0 {
            despawn_recursive_if_exists(&mut commands, e);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn update_projectiles(
    time: Res<Time>,
    budget: Res<RuntimeBudget>,
    mut commands: Commands,
    mut world: ResMut<VoxelWorld>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fx: ResMut<WeaponFxCache>,
    mut shake: ResMut<CameraShake>,
    mut stats: ResMut<DestructionStats>,
    mut feedback: ResMut<HitFeedback>,
    mut q: Query<(Entity, &mut Projectile, &mut Transform)>,
) {
    let dt = time.delta_seconds();
    let fx_scale = budget.weapon_fx_scale.clamp(0.05, 1.0);
    let mut rng =
        ChaCha8Rng::seed_from_u64((time.elapsed_seconds_wrapped() * 97_531.0) as u64 ^ 0xBABE_B00B);
    for (e, mut p, mut tf) in q.iter_mut() {
        p.life -= dt;
        let step = p.speed * dt;
        if step >= p.remaining || p.life <= 0.0 {
            // Arrived or exceeded the visual safety horizon. A stored hit
            // still resolves so lifetime culling never drops weapon damage.
            tf.translation = p.impact_pos;
            if let Some((bx, by, bz)) = p.hit_block {
                let radius = p.kind.blast_radius();
                let killed = break_blocks(
                    &mut world,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut fx,
                    bx,
                    by,
                    bz,
                    radius,
                    p.kind,
                    &mut rng,
                    fx_scale,
                    &mut stats,
                );
                stats.blocks_broken = stats.blocks_broken.saturating_add(killed as u64);
                if killed > 0 {
                    // Combo! Each hit within 2.5 s of the last one
                    // extends the combo meter — the HUD reads this.
                    stats.combo = stats.combo.saturating_add(killed.min(20));
                    stats.combo_timer = 2.5;
                    feedback.flash_t = 0.25;
                    feedback.last_hit_blocks = killed;
                }
                let explosive = matches!(
                    p.kind,
                    WeaponKind::RocketLauncher
                        | WeaponKind::GrenadeLauncher
                        | WeaponKind::PlasmaRifle
                );
                if explosive {
                    spawn_explosion(
                        &mut commands,
                        &mut meshes,
                        &mut materials,
                        &mut fx,
                        p.impact_pos,
                        radius,
                        p.kind,
                        fx_scale,
                    );
                    stats.explosions = stats.explosions.saturating_add(1);
                    // Big kaboom → heavy camera shake.
                    shake.add(p.kind.explosion_fx().shake);
                } else {
                    spawn_impact_puff(
                        &mut commands,
                        &mut meshes,
                        &mut materials,
                        &mut fx,
                        p.kind,
                        p.impact_pos,
                        fx_scale,
                    );
                    shake.add(p.kind.impact_fx().shake);
                }
            }
            despawn_recursive_if_exists(&mut commands, e);
        } else {
            tf.translation += p.dir * step;
            p.remaining -= step;
        }
    }
}

fn update_debris(
    time: Res<Time>,
    budget: Res<RuntimeBudget>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Debris, &mut Transform)>,
) {
    const GRAVITY: f32 = 18.0;
    // Global FX cap — if a flurry of explosions has more than this many
    // debris entities alive, kill the shortest-lived ones first. Keeps
    // the scene from snowballing into a perf death-spiral.
    const DEBRIS_CAP: usize = 600;
    let dt = time.delta_seconds();
    let live = q.iter().count();
    let mut queued_despawns = std::collections::HashSet::new();
    let cap = ((DEBRIS_CAP as f32) * budget.weapon_fx_scale.clamp(0.25, 1.0)) as usize;
    if live > cap {
        let overflow = live - cap;
        let mut candidates: Vec<(Entity, f32)> = q.iter().map(|(e, d, _)| (e, d.life)).collect();
        // Lowest life first.
        candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        for (e, _) in candidates.into_iter().take(overflow) {
            despawn_recursive_if_exists(&mut commands, e);
            queued_despawns.insert(e);
        }
    }
    for (e, mut d, mut tf) in q.iter_mut() {
        if queued_despawns.contains(&e) {
            continue;
        }
        d.life -= dt;
        d.velocity.y -= GRAVITY * dt;
        tf.translation += d.velocity * dt;
        let spin = d.spin;
        tf.rotate_local_x(spin.x * dt);
        tf.rotate_local_y(spin.y * dt);
        tf.rotate_local_z(spin.z * dt);
        let s_raw = (d.life / d.max_life).clamp(0.0, 1.0);
        let s = if s_raw > 0.3 { 1.0 } else { s_raw / 0.3 };
        tf.scale = Vec3::splat(0.35 + s * 0.35);
        if d.life <= 0.0 {
            despawn_recursive_if_exists(&mut commands, e);
        }
    }
}

/// Physics for detached voxels: they accelerate downward, tumble, and
/// shatter into debris the moment they enter a solid cell (or run out
/// of time). They are NOT voxels in the world, so they never block
/// shots or the player's movement — you can keep firing right through
/// them while they fall.
fn update_falling_blocks(
    time: Res<Time>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fx: ResMut<WeaponFxCache>,
    world: Res<VoxelWorld>,
    mut q: Query<(Entity, &mut FallingBlock, &mut Transform)>,
) {
    const GRAVITY: f32 = 22.0;
    let dt = time.delta_seconds();
    let mut rng =
        ChaCha8Rng::seed_from_u64((time.elapsed_seconds_wrapped() * 53_317.0) as u64 ^ 0xDEAD_CAFE);
    for (e, mut fb, mut tf) in q.iter_mut() {
        fb.max_fall_time -= dt;
        fb.velocity.y -= GRAVITY * dt;
        tf.translation += fb.velocity * dt;
        let spin = fb.spin;
        tf.rotate_local_x(spin.x * dt);
        tf.rotate_local_y(spin.y * dt);
        tf.rotate_local_z(spin.z * dt);
        // Bottom-of-the-cube probe: if the voxel beneath is solid, or
        // we've fallen off the world, shatter.
        let bx = tf.translation.x.floor() as i32;
        let by = (tf.translation.y - 0.5).floor() as i32;
        let bz = tf.translation.z.floor() as i32;
        let hit_ground = voxel_is_solid(world.voxel_at(bx, by, bz));
        if hit_ground || fb.max_fall_time <= 0.0 {
            spawn_debris(
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut fx,
                tf.translation.x.floor() as i32,
                tf.translation.y.floor() as i32,
                tf.translation.z.floor() as i32,
                fb.voxel,
                None,
                &mut rng,
            );
            despawn_recursive_if_exists(&mut commands, e);
        }
    }
}

fn update_explosions(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Explosion, &mut Transform)>,
) {
    for (e, mut ex, mut tf) in q.iter_mut() {
        ex.life -= time.delta_seconds();
        let t = 1.0 - (ex.life / ex.max_life).clamp(0.0, 1.0);
        let scale = ex.max_scale * (0.2 + 0.8 * t.min(1.0));
        tf.scale = Vec3::splat(scale);
        if ex.life <= 0.0 {
            despawn_recursive_if_exists(&mut commands, e);
        }
    }
}

// ---------------------------------------------------------------------
// DDA voxel raycast (Amanatides-Woo)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
struct VoxelRayHit {
    block: (i32, i32, i32),
    distance: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ShotPath {
    direction: Vec3,
    impact_pos: Vec3,
    hit_block: Option<(i32, i32, i32)>,
    travel_dist: f32,
}

fn normalized_direction(direction: Vec3, fallback: Vec3) -> Vec3 {
    let direction = direction.normalize_or_zero();
    if direction.length_squared() > 0.5 {
        return direction;
    }

    let fallback = fallback.normalize_or_zero();
    if fallback.length_squared() > 0.5 {
        fallback
    } else {
        Vec3::NEG_Z
    }
}

fn weapon_muzzle_world(
    player_transform: &Transform,
    weapon_transform: &Transform,
    muzzle_offset: Vec3,
) -> Vec3 {
    (player_transform.compute_matrix() * weapon_transform.compute_matrix())
        .transform_point3(muzzle_offset)
}

fn converged_muzzle_direction(muzzle: Vec3, target: Vec3, camera_direction: Vec3) -> Vec3 {
    normalized_direction(target - muzzle, camera_direction)
}

fn voxel_center(block: (i32, i32, i32)) -> Vec3 {
    Vec3::new(
        block.0 as f32 + 0.5,
        block.1 as f32 + 0.5,
        block.2 as f32 + 0.5,
    )
}

fn solve_shot_path(
    world: &VoxelWorld,
    camera_origin: Vec3,
    camera_direction: Vec3,
    muzzle: Vec3,
    max_range: f32,
) -> ShotPath {
    let range = if max_range.is_finite() {
        max_range.max(0.5)
    } else {
        WEAPON_MAX_RANGE
    };
    let camera_direction = normalized_direction(camera_direction, Vec3::NEG_Z);
    let camera_hit = dda_voxel_hit(world, camera_origin, camera_direction, range);
    let intended_target = camera_hit
        .map(|hit| voxel_center(hit.block))
        .unwrap_or(camera_origin + camera_direction * range);
    let direction = converged_muzzle_direction(muzzle, intended_target, camera_direction);
    let intended_distance = (intended_target - muzzle).length().max(0.05);

    // Recast from the physical muzzle toward the camera-selected target.
    // This catches nearby cover and prevents a viewmodel offset from becoming
    // an accidental wall-penetration exploit.
    if let Some(hit) = dda_voxel_hit(
        world,
        muzzle,
        direction,
        intended_distance + MUZZLE_RAY_EPSILON,
    ) {
        return ShotPath {
            direction,
            impact_pos: muzzle + direction * hit.distance,
            hit_block: Some(hit.block),
            travel_dist: hit.distance.max(0.05),
        };
    }

    ShotPath {
        direction,
        impact_pos: intended_target,
        hit_block: camera_hit.map(|hit| hit.block),
        travel_dist: intended_distance,
    }
}

fn dda_voxel_hit(
    world: &VoxelWorld,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
) -> Option<VoxelRayHit> {
    let dir = dir.normalize_or_zero();
    if dir.length_squared() < 1e-6 || !origin.is_finite() || !max_dist.is_finite() {
        return None;
    }
    let mut x = origin.x.floor() as i32;
    let mut y = origin.y.floor() as i32;
    let mut z = origin.z.floor() as i32;
    let step_x = dir.x.signum() as i32;
    let step_y = dir.y.signum() as i32;
    let step_z = dir.z.signum() as i32;
    let t_delta_x = if dir.x != 0.0 {
        (1.0 / dir.x).abs()
    } else {
        f32::INFINITY
    };
    let t_delta_y = if dir.y != 0.0 {
        (1.0 / dir.y).abs()
    } else {
        f32::INFINITY
    };
    let t_delta_z = if dir.z != 0.0 {
        (1.0 / dir.z).abs()
    } else {
        f32::INFINITY
    };
    let nb = |p: f32, step: i32| -> f32 {
        if step > 0 {
            p.floor() + 1.0 - p
        } else if step < 0 {
            p - p.floor()
        } else {
            f32::INFINITY
        }
    };
    let mut tmx = nb(origin.x, step_x) * t_delta_x;
    let mut tmy = nb(origin.y, step_y) * t_delta_y;
    let mut tmz = nb(origin.z, step_z) * t_delta_z;
    // --- Chunk lookup cache ------------------------------------------
    // The DDA walks up to 20 000 voxels across potentially hundreds of
    // chunks. Without caching, every step does a HashMap lookup. Since
    // consecutive voxels almost always live in the same chunk (16³), we
    // stash the last chunk pointer and only re-probe when we cross a
    // chunk boundary. On a long clear shot this drops 20k hash lookups
    // to <100.
    let cs = crate::chunk::CHUNK_SIZE as i32;
    let mut cached_cp: Option<crate::chunk::ChunkPos> = None;
    let mut cached_chunk: Option<&crate::chunk::Chunk> = None;
    let mut steps = 0;
    while steps < 20_000 {
        let t = tmx.min(tmy).min(tmz);
        if t > max_dist {
            return None;
        }
        if tmx <= tmy && tmx <= tmz {
            x += step_x;
            tmx += t_delta_x;
        } else if tmy <= tmz {
            y += step_y;
            tmy += t_delta_y;
        } else {
            z += step_z;
            tmz += t_delta_z;
        }
        let cx = x.div_euclid(cs);
        let cy = y.div_euclid(cs);
        let cz = z.div_euclid(cs);
        let cp = crate::chunk::ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        };
        if cached_cp != Some(cp) {
            cached_cp = Some(cp);
            cached_chunk = world.chunks.get(&cp);
        }
        let v = match cached_chunk {
            Some(chunk) => {
                let lx = x.rem_euclid(cs) as usize;
                let ly = y.rem_euclid(cs) as usize;
                let lz = z.rem_euclid(cs) as usize;
                chunk.get(lx, ly, lz)
            }
            None => AIR,
        };
        if voxel_is_weapon_target(v) {
            return Some(VoxelRayHit {
                block: (x, y, z),
                distance: t,
            });
        }
        steps += 1;
    }
    None
}

// ---------------------------------------------------------------------
// Fun-boost systems
// ---------------------------------------------------------------------

/// Drives camera trauma → yaw/pitch perturbation. Decays exponentially
/// so huge rocket hits rattle the camera hard for ~0.6 s, tiny pistol
/// pops are barely perceptible but still juicy.
fn apply_camera_shake(
    time: Res<Time>,
    mut shake: ResMut<CameraShake>,
    mut cam_q: Query<&mut Transform, With<Camera3d>>,
) {
    let dt = time.delta_seconds();
    shake.trauma = (shake.trauma - dt * 1.6).max(0.0);
    shake.t += dt;
    if shake.trauma <= 0.0 {
        return;
    }
    let Ok(mut tf) = cam_q.get_single_mut() else {
        return;
    };
    // Trauma² → amplitude (so small trauma is gentle, big trauma rocks).
    let amp = shake.trauma * shake.trauma;
    // Deterministic pseudo-noise via trig.
    let t = shake.t * 40.0;
    let nx = (t * 1.7).sin() * (t * 0.83).cos();
    let ny = (t * 2.3).sin() * (t * 0.61).cos();
    let nz = (t * 1.1).sin() * (t * 1.47).cos();
    let yaw = amp * 0.035 * nx;
    let pitch = amp * 0.028 * ny;
    let roll = amp * 0.020 * nz;
    tf.rotation = tf.rotation * Quat::from_euler(EulerRot::YXZ, yaw, pitch, roll);
}

/// Fades the debris counter's "combo streak" timer out once the player
/// stops shooting for a couple seconds.
fn decay_hit_feedback(
    time: Res<Time>,
    mut stats: ResMut<DestructionStats>,
    mut feedback: ResMut<HitFeedback>,
) {
    let dt = time.delta_seconds();
    feedback.flash_t = (feedback.flash_t - dt).max(0.0);
    stats.combo_timer = (stats.combo_timer - dt).max(0.0);
    if stats.combo_timer <= 0.0 {
        stats.combo = 0;
    }
}

/// Expanding white ring spawned by every explosion. Scales from 0 →
/// max_scale while fading its emissive alpha.
fn update_shockwaves(
    time: Res<Time>,
    mut commands: Commands,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut q: Query<(
        Entity,
        &mut Shockwave,
        &mut Transform,
        &Handle<StandardMaterial>,
    )>,
) {
    let dt = time.delta_seconds();
    for (e, mut sw, mut tf, mat_h) in q.iter_mut() {
        sw.life -= dt;
        if sw.life <= 0.0 {
            mats.remove(mat_h.id());
            despawn_recursive_if_exists(&mut commands, e);
            continue;
        }
        let t = 1.0 - (sw.life / sw.max_life).clamp(0.0, 1.0);
        // Ease-out cubic.
        let e_t = 1.0 - (1.0 - t).powi(3);
        let s = sw.max_scale * e_t;
        tf.scale = Vec3::new(s, s * 0.15, s);
        if let Some(m) = mats.get_mut(mat_h) {
            let fade = 1.0 - t;
            m.emissive = LinearRgba::rgb(
                sw.emissive_rgb.x * fade,
                sw.emissive_rgb.y * fade,
                sw.emissive_rgb.z * fade,
            );
            m.base_color = Color::srgba(sw.base_rgb.x, sw.base_rgb.y, sw.base_rgb.z, fade);
        }
    }
}

/// Drains queued flash requests into a single persistent overlay
/// entity, max-merging colour and alpha so simultaneous explosions do
/// not stack their full-screen flashes (which would double the
/// brightness and blind the player during chain reactions).
fn update_screen_flash(
    time: Res<Time>,
    mut commands: Commands,
    mut fx: ResMut<WeaponFxCache>,
    mut q: Query<(Entity, &mut ScreenFlash, &mut BackgroundColor)>,
) {
    let dt = time.delta_seconds();
    let pending = std::mem::take(&mut fx.pending_flashes);
    if pending.is_empty() && q.is_empty() {
        return;
    }
    // Merge every request this frame into the strongest single flash.
    let merged =
        pending.into_iter().fold(
            None::<(Vec3, f32, f32)>,
            |acc, (rgb, alpha, life)| match acc {
                None => Some((rgb, alpha, life)),
                Some((arr, aa, al)) => Some((
                    Vec3::new(arr.x.max(rgb.x), arr.y.max(rgb.y), arr.z.max(rgb.z)),
                    aa.max(alpha),
                    al.max(life),
                )),
            },
        );

    if let Ok((e, mut sf, mut bg)) = q.get_single_mut() {
        if let Some((rgb, alpha, life)) = merged {
            // Refresh the existing overlay if the new flash is brighter
            // or longer than what is already on screen.
            if alpha >= sf.max_alpha * (sf.life / sf.max_life).max(0.0) {
                sf.rgb = rgb;
                sf.max_alpha = alpha;
                sf.life = life;
                sf.max_life = life;
            } else {
                sf.life = sf.life.max(life * 0.5);
            }
        }
        sf.life -= dt;
        if sf.life <= 0.0 {
            despawn_recursive_if_exists(&mut commands, e);
        } else {
            let a = (sf.life / sf.max_life).clamp(0.0, 1.0);
            bg.0 = Color::srgba(sf.rgb.x, sf.rgb.y, sf.rgb.z, a * sf.max_alpha);
        }
    } else if let Some((rgb, alpha, life)) = merged {
        commands.spawn((
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                background_color: Color::srgba(rgb.x, rgb.y, rgb.z, alpha).into(),
                z_index: ZIndex::Global(40),
                ..default()
            },
            ScreenFlash {
                life,
                max_life: life,
                rgb,
                max_alpha: alpha,
            },
            Name::new("ExplosionFlash"),
        ));
    }
}

/// If the player lands on top of an emissive "bounce" block (we reuse
/// the Crystal voxel id — a.k.a. the glowing neon block — as trampoline)
/// catapult them skyward.
fn check_bounce_pad(world: Res<VoxelWorld>, mut player_q: Query<(&Transform, &mut Player)>) {
    use crate::blocks::{voxel_is_emissive, AIR};
    let Ok((tf, mut player)) = player_q.get_single_mut() else {
        return;
    };
    // Foot probe: 1 cell below the player's feet.
    let foot_y = tf.translation.y - 0.05;
    let bx = crate::chunk::floor_to_i32_safe(tf.translation.x);
    let by = crate::chunk::floor_to_i32_safe(foot_y - 1.0);
    let bz = crate::chunk::floor_to_i32_safe(tf.translation.z);
    let v = world.voxel_at(bx, by, bz);
    if v == AIR || !voxel_is_emissive(v) {
        return;
    }
    // Only trigger when descending and near-contact, so walking across
    // an emissive block at constant height doesn't keep launching you.
    if player.velocity.y > 0.5 || !player.on_ground {
        return;
    }
    // BOING! Scaled launch with a tiny bit of horizontal preserved.
    player.velocity.y = 22.0;
    player.on_ground = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projectile_lifetime_tracks_normal_travel_and_caps_long_shots() {
        let normal = projectile_lifetime_secs(90.0, 90.0);
        assert!((normal - 1.25).abs() < f32::EPSILON);
        assert_eq!(
            projectile_lifetime_secs(10_000.0, 45.0),
            MAX_PROJECTILE_LIFETIME_SECS
        );
        assert_eq!(
            projectile_lifetime_secs(-10.0, 90.0),
            PROJECTILE_ARRIVAL_GRACE_SECS
        );
    }

    #[test]
    fn projectile_lifetime_fails_bounded_for_invalid_inputs() {
        for (remaining, speed) in [
            (f32::NAN, 90.0),
            (90.0, f32::NAN),
            (90.0, 0.0),
            (90.0, -1.0),
        ] {
            assert_eq!(
                projectile_lifetime_secs(remaining, speed),
                MAX_PROJECTILE_LIFETIME_SECS
            );
        }
    }

    #[test]
    fn off_axis_muzzle_converges_on_the_camera_crosshair() {
        let muzzle = Vec3::new(0.42, -0.28, -0.75);
        let target = Vec3::new(0.0, 0.0, -120.0);
        let direction = converged_muzzle_direction(muzzle, target, Vec3::NEG_Z);
        let target_t = (target.z - muzzle.z) / direction.z;
        let crossing = muzzle + direction * target_t;

        assert!((crossing.x - target.x).abs() < 1e-4);
        assert!((crossing.y - target.y).abs() < 1e-4);
        assert!((direction.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn degenerate_muzzle_target_uses_the_camera_direction() {
        let muzzle = Vec3::new(2.0, 3.0, 4.0);
        let camera_direction = Vec3::new(0.2, -0.1, -1.0).normalize();
        let direction = converged_muzzle_direction(muzzle, muzzle, camera_direction);

        assert!((direction - camera_direction).length() < 1e-6);
    }

    #[test]
    fn muzzle_world_transform_inherits_camera_roll_without_lag() {
        let player = Transform::from_xyz(10.0, 20.0, 30.0).with_rotation(
            Quat::from_axis_angle(Vec3::Y, 0.6)
                * Quat::from_axis_angle(Vec3::X, -0.2)
                * Quat::from_axis_angle(Vec3::Z, 0.8),
        );
        let weapon = Transform::from_xyz(0.3, -0.2, -0.5);
        let muzzle_offset = Vec3::new(0.0, 0.1, -0.4);
        let actual = weapon_muzzle_world(&player, &weapon, muzzle_offset);
        let expected =
            (player.compute_matrix() * weapon.compute_matrix()).transform_point3(muzzle_offset);

        assert!((actual - expected).length() < 1e-6);
    }

    #[test]
    fn projectile_lights_preserve_signature_shots_and_thin_automatic_fire() {
        assert!(should_spawn_projectile_light(
            WeaponKind::RocketLauncher,
            0.20,
            1
        ));
        assert!(!should_spawn_projectile_light(
            WeaponKind::RocketLauncher,
            0.19,
            1
        ));
        assert!(should_spawn_projectile_light(
            WeaponKind::PlasmaRifle,
            0.35,
            2
        ));
        assert!(!should_spawn_projectile_light(
            WeaponKind::PlasmaRifle,
            0.35,
            1
        ));
        assert!(should_spawn_projectile_light(WeaponKind::Minigun, 1.0, 5));
        assert!(!should_spawn_projectile_light(WeaponKind::Minigun, 1.0, 4));
        assert!(!should_spawn_projectile_light(WeaponKind::Minigun, 0.5, 5));
        assert!(!should_spawn_projectile_light(
            WeaponKind::PlasmaRifle,
            f32::NAN,
            2
        ));
    }

    #[test]
    fn viewmodel_tuning_stays_finite_and_bounded() {
        for kind in WeaponKind::ALL {
            let tuning = kind.viewmodel_tuning();
            assert!(tuning.rest_translation.is_finite(), "{kind:?}");
            assert!(tuning.muzzle_offset.is_finite(), "{kind:?}");
            assert!(tuning.recoil_offset.is_finite(), "{kind:?}");
            assert!(
                (-0.75..=-0.25).contains(&tuning.muzzle_offset.z),
                "{kind:?} muzzle {:?}",
                tuning.muzzle_offset
            );
            assert!(tuning.recoil_offset.length() <= 0.20, "{kind:?}");
            assert!((0.0..=0.24).contains(&tuning.recoil_pitch), "{kind:?}");
            assert!(
                (0.04..=0.10).contains(&tuning.muzzle_light_life),
                "{kind:?}"
            );
            assert!(
                (8.0..=18.0).contains(&tuning.muzzle_light_range),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn muzzle_offsets_follow_barrel_silhouettes() {
        let muzzle_z = |kind: WeaponKind| kind.viewmodel_tuning().muzzle_offset.z;
        assert!(muzzle_z(WeaponKind::Pistol) > muzzle_z(WeaponKind::Blaster));
        assert!(muzzle_z(WeaponKind::Blaster) > muzzle_z(WeaponKind::AssaultRifle));
        assert!(muzzle_z(WeaponKind::Sniper) < muzzle_z(WeaponKind::AssaultRifle));
        assert!(muzzle_z(WeaponKind::RocketLauncher) < muzzle_z(WeaponKind::GrenadeLauncher));

        let blaster = WeaponKind::Blaster.rifle_silhouette();
        let rifle = WeaponKind::AssaultRifle.rifle_silhouette();
        let sniper = WeaponKind::Sniper.rifle_silhouette();
        assert!(blaster.barrel_len < rifle.barrel_len);
        assert!(rifle.barrel_len < sniper.barrel_len);
        assert!(blaster.optic_len < rifle.optic_len);
        assert!(rifle.optic_len < sniper.optic_len);
    }

    #[test]
    fn viewmodel_recoil_curve_clamps_invalid_and_out_of_range_inputs() {
        assert_eq!(viewmodel_recoil_amount(0.2, 0.2), 1.0);
        assert_eq!(viewmodel_recoil_amount(0.1, 0.2), 0.5);
        assert_eq!(viewmodel_recoil_amount(-1.0, 0.2), 0.0);
        assert_eq!(viewmodel_recoil_amount(1.0, 0.2), 1.0);
        assert_eq!(viewmodel_recoil_amount(f32::NAN, 0.2), 0.0);
        assert_eq!(viewmodel_recoil_amount(0.1, 0.0), 0.0);
    }

    #[test]
    fn muzzle_lights_preserve_heavy_shots_and_thin_low_spec_automatic_fire() {
        assert!(should_spawn_muzzle_light(
            WeaponKind::RocketLauncher,
            0.21,
            1
        ));
        assert!(!should_spawn_muzzle_light(
            WeaponKind::RocketLauncher,
            0.20,
            1
        ));

        assert!(should_spawn_muzzle_light(WeaponKind::Minigun, 1.0, 2));
        assert!(!should_spawn_muzzle_light(WeaponKind::Minigun, 1.0, 1));
        assert!(should_spawn_muzzle_light(WeaponKind::Minigun, 0.65, 3));
        assert!(!should_spawn_muzzle_light(WeaponKind::Minigun, 0.65, 2));
        assert!(should_spawn_muzzle_light(WeaponKind::AssaultRifle, 0.65, 2));
        assert!(!should_spawn_muzzle_light(
            WeaponKind::AssaultRifle,
            0.65,
            1
        ));
        assert!(should_spawn_muzzle_light(WeaponKind::Pistol, 0.65, 1));
        assert!(!should_spawn_muzzle_light(WeaponKind::Pistol, f32::NAN, 1));
    }

    #[test]
    fn low_spec_viewmodels_and_projectiles_use_bounded_detail_tiers() {
        assert_eq!(
            WeaponVisualDetail::for_profile(RuntimeProfile::LowSpec),
            WeaponVisualDetail::Core
        );
        assert_eq!(
            WeaponVisualDetail::for_profile(RuntimeProfile::Balanced),
            WeaponVisualDetail::Full
        );
        assert_eq!(projectile_visual_layer_count(f32::NAN), 1);
        assert_eq!(projectile_visual_layer_count(0.34), 1);
        assert_eq!(projectile_visual_layer_count(0.35), 2);
        assert_eq!(projectile_visual_layer_count(0.74), 2);
        assert_eq!(projectile_visual_layer_count(0.75), 3);
    }

    #[test]
    fn physical_fx_caps_scale_before_entities_are_spawned() {
        assert_eq!(debris_spawn_cap(0.10), 0);
        assert_eq!(debris_spawn_cap(1.0), 40);
        assert!(debris_spawn_cap(0.65) < debris_spawn_cap(1.0));
        assert_eq!(debris_spawn_cap(f32::NAN), 0);

        let low_rocket = falling_block_spawn_cap(WeaponKind::RocketLauncher, 0.65);
        let full_rocket = falling_block_spawn_cap(WeaponKind::RocketLauncher, 1.0);
        let low_rifle = falling_block_spawn_cap(WeaponKind::AssaultRifle, 0.65);
        assert!(low_rocket < full_rocket);
        assert!(low_rocket > low_rifle);
        assert_eq!(falling_block_spawn_cap(WeaponKind::Pistol, 0.10), 0);
        assert_eq!(falling_block_spawn_cap(WeaponKind::Pistol, f32::NAN), 0);
    }
}
