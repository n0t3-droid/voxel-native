//! Voxel-Native - native voxel engine, Rust + Bevy + wgpu.
//! Successor to R93G (https://github.com/n0t3-droid/N5).

pub mod agent_control;
mod ambient;
mod animation;
mod blocks;
pub mod bots;
mod builder;
mod celestial;
mod chunk;
mod city;
mod commands;
mod daynight;
mod director;
mod editor;
mod hud;
mod icons;
mod menu;
mod mesher;
mod mode;
mod neurocore;
mod platform;
mod player;
mod qa;
mod sculpt;
mod selection;
mod settings;
mod ships;
mod sketch_model;
mod sky;
mod terrain;
mod textures;
mod theme;
mod toolbelt;
mod ui_kit;
mod weapons;
mod weather;
mod world;

use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use bevy::render::settings::{Backends, InstanceFlags, RenderCreation, WgpuSettings};
#[cfg(not(target_arch = "wasm32"))]
use bevy::render::RenderPlugin;
use bevy::utils::Duration;
use bevy::winit::{UpdateMode, WinitSettings};

const MENU_LOW_POWER_INTERVAL: Duration = Duration::from_millis(250);
const PAUSED_LOW_POWER_INTERVAL: Duration = Duration::from_millis(125);
const UNFOCUSED_LOW_POWER_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopPolicy {
    Continuous,
    ReactiveLowPower(Duration),
}

fn main() {
    configure_render_environment();

    // Install a panic hook that logs the panic location and message
    // but does not swallow the panic itself — Bevy worker threads
    // (async chunk/mesh tasks) can panic on a bad NaN or a stray
    // `unwrap` and the default hook prints the panic then keeps the
    // main loop running silently, which produces the classic "my
    // world stopped generating new chunks" report. We keep the same
    // behaviour, but emit a structured error line via `eprintln!` so
    // the user sees WHICH thread died and WHERE, and can attach a
    // meaningful log to a bug report. Replace with `std::process::abort`
    // here to make worker-thread panics fatal if you prefer.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        eprintln!(
            "[voxel-native] PANIC in thread '{}': {info}",
            thread.name().unwrap_or("unnamed")
        );
        prev(info);
    }));

    App::new()
        .add_plugins({
            let plugins = DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Voxel-Native (R93G successor)".into(),
                    resolution: (1280.0, 720.0).into(),
                    // AutoVsync caps the frame rate to the monitor
                    // refresh and blocks on the compositor. On
                    // integrated GPUs this is strictly better than
                    // the previous AutoNoVsync: GPU stays ~30°C
                    // cooler, no tearing, and at 60 fps the vertex
                    // bandwidth cost of the greedy mesh is a non-
                    // issue. Uncapped FPS is still available by
                    // flipping this to AutoNoVsync for benchmarks.
                    present_mode: bevy::window::PresentMode::AutoVsync,
                    ..default()
                }),
                ..default()
            });
            #[cfg(not(target_arch = "wasm32"))]
            let plugins = plugins
                // Force Vulkan on Windows. The DX12 backend in wgpu 0.20
                // mis-tracks resource states for our two-camera composite
                // (sky + world both with ClearColorConfig::None) and floods
                // the console with INVALID_SUBRESOURCE_STATE. Vulkan is
                // free of that bug and is generally faster on this engine.
                .set(RenderPlugin {
                    render_creation: RenderCreation::Automatic(WgpuSettings {
                        backends: Some(Backends::VULKAN | Backends::METAL),
                        instance_flags: render_instance_flags(),
                        ..default()
                    }),
                    ..default()
                });
            plugins
        })
        .insert_resource(ClearColor(Color::srgb(0.53, 0.80, 0.98)))
        .insert_resource(winit_settings_for_loop_policy(loop_policy_for_game_state(
            &menu::GameState::MainMenu,
        )))
        // MSAA off: on integrated GPUs (e.g. Vega 8 in Ryzen 5700G)
        // 4x MSAA on HDR (Rgba16Float) buffers quadruples bandwidth and
        // cuts FPS by 30–50%. With the greedy-mesh block-aligned UVs and
        // aggressive fog, visible aliasing is minimal. Users who want it
        // back can set Msaa::Sample2 in High graphics mode.
        .insert_resource(Msaa::Off)
        .add_plugins(agent_control::AgentControlPlugin)
        .add_plugins(sketch_model::SketchModelPlugin)
        .add_plugins(ambient::AmbientPlugin)
        .add_plugins((
            settings::SettingsPlugin,
            world::WorldPlugin,
            player::PlayerPlugin,
            daynight::DayNightPlugin,
            celestial::CelestialPlugin,
            sky::SkyPlugin,
            weather::WeatherPlugin,
            hud::HudPlugin,
            editor::EditorPlugin,
            menu::MenuPlugin,
            weapons::WeaponsPlugin,
            builder::BuilderPlugin,
            director::DirectorPlugin,
            animation::AnimationPlugin,
            selection::SelectionPlugin,
        ))
        .add_plugins(city::CityPlugin)
        .add_plugins(bots::BotsPlugin)
        .add_plugins(ships::ShipPlugin)
        .add_plugins(mode::ModePlugin)
        .add_plugins(neurocore::NeuroCorePlugin)
        .add_plugins(qa::QaPlugin)
        .add_plugins(toolbelt::ToolbeltPlugin)
        .add_plugins(commands::CommandDeckPlugin)
        .add_plugins(sculpt::SculptPlugin)
        .add_systems(Update, sync_winit_loop_with_game_state)
        .add_systems(Startup, print_controls)
        .run();
}

fn loop_policy_for_game_state(state: &menu::GameState) -> LoopPolicy {
    match state {
        menu::GameState::MainMenu => LoopPolicy::ReactiveLowPower(MENU_LOW_POWER_INTERVAL),
        menu::GameState::Paused => LoopPolicy::ReactiveLowPower(PAUSED_LOW_POWER_INTERVAL),
        menu::GameState::InGame => LoopPolicy::Continuous,
    }
}

fn winit_settings_for_loop_policy(policy: LoopPolicy) -> WinitSettings {
    let focused_mode = match policy {
        LoopPolicy::Continuous => UpdateMode::Continuous,
        LoopPolicy::ReactiveLowPower(wait) => UpdateMode::reactive_low_power(wait),
    };

    WinitSettings {
        focused_mode,
        unfocused_mode: UpdateMode::reactive_low_power(UNFOCUSED_LOW_POWER_INTERVAL),
    }
}

fn sync_winit_loop_with_game_state(
    state: Res<State<menu::GameState>>,
    mut settings: ResMut<WinitSettings>,
) {
    let desired = winit_settings_for_loop_policy(loop_policy_for_game_state(state.get()));
    if settings.focused_mode != desired.focused_mode
        || settings.unfocused_mode != desired.unfocused_mode
    {
        *settings = desired;
    }
}

#[cfg(target_arch = "wasm32")]
fn configure_render_environment() {}

#[cfg(not(target_arch = "wasm32"))]
fn configure_render_environment() {
    // Vulkan on Windows auto-loads "implicit layers" from system registry
    // entries. Broken overlay installs commonly leave dead manifests behind
    // (Epic EOSOverlay*.json, ReShade DLLs), which the Vulkan loader reports
    // as scary ERROR lines before wgpu even starts. This is not an engine
    // fault, but we can keep our render bootstrap clean by disabling implicit
    // layers for the process. Users who explicitly want overlays/RenderDoc can
    // opt out without changing code.
    if !env_flag("VOXEL_NATIVE_ALLOW_VULKAN_OVERLAYS") {
        append_env_filter("VK_LOADER_LAYERS_DISABLE", "~implicit~");
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn render_instance_flags() -> InstanceFlags {
    if env_flag("VOXEL_NATIVE_WGPU_VALIDATION") || env_flag("WGPU_VALIDATION") {
        InstanceFlags::debugging()
    } else {
        // Bevy/wgpu debug builds request validation by default. On machines
        // without the Vulkan SDK this logs "VK_LAYER_KHRONOS_validation"
        // warnings every run. Runtime correctness is still handled by wgpu;
        // validation can be re-enabled via VOXEL_NATIVE_WGPU_VALIDATION=1.
        InstanceFlags::empty()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[cfg(not(target_arch = "wasm32"))]
fn append_env_filter(name: &str, filter: &str) {
    match std::env::var(name) {
        Ok(current) if current.split(',').any(|part| part.trim() == filter) => {}
        Ok(current) if !current.trim().is_empty() => {
            std::env::set_var(name, format!("{current},{filter}"));
        }
        _ => std::env::set_var(name, filter),
    }
}

fn print_controls() {
    for line in control_lines() {
        info!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printed_controls_do_not_advertise_old_build_function_keys() {
        let controls = control_lines().join("\n");
        for token in ["F1", "F3", "F7", "F8", "Tab", "1-0", "Q / E"] {
            assert!(
                !controls.contains(token),
                "startup controls still advertise old key workflow: {token}"
            );
        }
        assert!(controls.contains("Toolbox"));
        assert!(controls.contains("Pencil"));
    }

    #[test]
    fn main_menu_uses_reactive_low_power_loop_policy() {
        assert_eq!(
            loop_policy_for_game_state(&menu::GameState::MainMenu),
            LoopPolicy::ReactiveLowPower(MENU_LOW_POWER_INTERVAL)
        );
    }

    #[test]
    fn paused_uses_reactive_low_power_loop_policy() {
        assert_eq!(
            loop_policy_for_game_state(&menu::GameState::Paused),
            LoopPolicy::ReactiveLowPower(PAUSED_LOW_POWER_INTERVAL)
        );
    }

    #[test]
    fn gameplay_uses_continuous_loop_policy() {
        assert_eq!(
            loop_policy_for_game_state(&menu::GameState::InGame),
            LoopPolicy::Continuous
        );
    }

    #[test]
    fn idle_winit_settings_ignore_raw_device_motion() {
        let settings =
            winit_settings_for_loop_policy(LoopPolicy::ReactiveLowPower(MENU_LOW_POWER_INTERVAL));

        assert_eq!(
            settings.focused_mode,
            UpdateMode::reactive_low_power(MENU_LOW_POWER_INTERVAL)
        );
        assert_eq!(
            settings.unfocused_mode,
            UpdateMode::reactive_low_power(UNFOCUSED_LOW_POWER_INTERVAL)
        );
    }
}

fn control_lines() -> Vec<&'static str> {
    vec![
        "-------- Voxel-Native Controls (Sketch Editor) --------",
        "  WASD         : move",
        "  Space        : jump / fly up  (double-tap = toggle fly)",
        "  F            : toggle fly",
        "  Ctrl         : sprint",
        "  W W (double) : sprint (recommended when Windows intercepts Ctrl)",
        "  Shift        : fly down / sneak",
        "  Toolbox      : select Pencil, Rectangle, Push/Pull, Room, Opening, Road, Bot Area",
        "  Pencil       : click endpoint to endpoint; lines chain from the last point",
        "  Rectangle    : click two snapped corners for floors, roofs, walls, windows, doors",
        "  Push/Pull    : click a face, move to depth, click again to commit",
        "  RMB          : orbit while drawing without deleting blocks",
        "  LMB          : draw, select, pull, or commit the active toolbox action",
        "  Save         : use the editor/menu save button",
        "  ESC          : pause menu / close overlay",
        "-------------------------------------------------------",
    ]
}
