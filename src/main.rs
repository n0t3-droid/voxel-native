//! Voxel-Native - native voxel engine, Rust + Bevy + wgpu.
//! Successor to R93G (https://github.com/n0t3-droid/N5).

pub mod agent_control;
mod ambient;
mod animation;
mod blocks;
pub mod bots;
mod builder;
mod chunk;
mod city;
mod commands;
mod daynight;
mod director;
mod editor;
mod film;
mod frontier;
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
        // MSAA off: on integrated GPUs (e.g. Vega 8 in Ryzen 5700G)
        // 4x MSAA on HDR (Rgba16Float) buffers quadruples bandwidth and
        // cuts FPS by 30–50%. With the greedy-mesh block-aligned UVs and
        // aggressive fog, visible aliasing is minimal. Users who want it
        // back can set Msaa::Sample2 in High graphics mode.
        .insert_resource(Msaa::Off)
        .add_plugins(agent_control::AgentControlPlugin)
        .add_plugins(ambient::AmbientPlugin)
        .add_plugins((
            settings::SettingsPlugin,
            world::WorldPlugin,
            player::PlayerPlugin,
            daynight::DayNightPlugin,
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
            city::CityPlugin,
        ))
        .add_plugins(bots::BotsPlugin)
        .add_plugins(ships::ShipPlugin)
        .add_plugins(mode::ModePlugin)
        .add_plugins(neurocore::NeuroCorePlugin)
        .add_plugins(qa::QaPlugin)
        .add_plugins(film::FilmPlugin)
        .add_plugins(toolbelt::ToolbeltPlugin)
        .add_plugins(commands::CommandDeckPlugin)
        .add_plugins(sculpt::SculptPlugin)
        .add_systems(Startup, print_controls)
        .run();
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
    info!("-------- Voxel-Native Controls (Minecraft-style) --------");
    info!("  WASD         : move");
    info!("  Space        : jump / fly up  (double-tap = toggle fly)");
    info!("  F            : toggle fly");
    info!("  Ctrl         : sprint");
    info!("  W W (double) : sprint (empfohlen, da Windows Ctrl abfangen kann)");
    info!("  Shift        : fly down / sneak");
    info!("  1-0          : Creative Build tools (default mode)");
    info!("  E            : open inventory");
    info!("  F3           : Creative Build / terrain editing (weapons holstered)");
    info!("  F7           : enter Build Live instantly");
    info!("  F8           : arm or holster weapons explicitly");
    info!("  Tab          : show/hide Build Studio picker");
    info!("  Q / E        : cycle Build Studio tools while building");
    info!("  F1 / Ctrl+P  : command deck / searchable controls");
    info!("  H            : enter nearby shuttle cockpit (or LMB)");
    info!("  Inventar     : Shuttle KI-Gefecht optional (Drohnen aus bis du es einschaltest)");
    info!("  ESC          : pause menu / close overlay");
    info!("  Shift+F3     : toggle debug overlay");
    info!("  Shift+F10    : warp to nearest Aether sky island");
    info!("  Shift+F11    : stamp an orbital landing pad at the player");
    info!("  F2           : screenshot");
    info!("  F5           : save world + settings");
    info!("  LMB          : capture mouse");
    info!("---------------------------------------------------------");
}
