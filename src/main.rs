//! Voxel Native - a Rust, Bevy, and wgpu voxel engine engineered with Codex.

mod agent_capabilities;
pub mod agent_control;
pub mod agent_direct_bridge;
mod ambient;
mod animation;
mod blocks;
mod bot_command;
mod bot_executor;
pub mod bots;
mod builder;
mod celestial;
mod chunk;
mod city;
mod commands;
pub mod continuum_morphogenesis;
mod creator_contract;
mod creator_library;
mod daynight;
mod director;
mod editor;
mod feedback_audio;
mod horizon;
mod hud;
mod icons;
pub mod implicit_voxels;
mod live_link;
mod menu;
mod mesher;
mod mission_control;
mod mode;
mod neurocore;
mod object_lab;
pub mod planetary_streaming;
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
mod vegetation;
mod villagers;
pub mod virtual_voxel_hierarchy;
mod voxel_budget;
mod water;
mod weapons;
mod weather;
mod world;
pub mod world_continuum;

use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use bevy::render::settings::{Backends, InstanceFlags, RenderCreation, WgpuSettings};
#[cfg(not(target_arch = "wasm32"))]
use bevy::render::RenderPlugin;
use bevy::utils::Duration;
use bevy::window::WindowResolution;
use bevy::winit::{UpdateMode, WinitSettings};

const MENU_LOW_POWER_INTERVAL: Duration = Duration::from_millis(250);
const PAUSED_LOW_POWER_INTERVAL: Duration = Duration::from_millis(125);
const UNFOCUSED_LOW_POWER_INTERVAL: Duration = Duration::from_millis(500);
const DEFAULT_WINDOW_TITLE: &str = "Voxel Native // Codex Engineering";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopPolicy {
    Continuous,
    ReactiveLowPower(Duration),
}

fn instance_window_title(label: Option<&str>) -> String {
    let label = label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(|label| {
            label
                .chars()
                .filter(|character| !character.is_control())
                .take(32)
                .collect::<String>()
        })
        .filter(|label| !label.is_empty());
    label.map_or_else(
        || DEFAULT_WINDOW_TITLE.to_owned(),
        |label| format!("Voxel Native [{label}] // Codex Engineering"),
    )
}

fn configured_window_title() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        return instance_window_title(std::env::var("VOXEL_NATIVE_INSTANCE_LABEL").ok().as_deref());
    }
    #[cfg(target_arch = "wasm32")]
    {
        instance_window_title(None)
    }
}

const DEFAULT_WINDOW_WIDTH: f32 = 1280.0;
const DEFAULT_WINDOW_HEIGHT: f32 = 720.0;
const QA_EXACT_VIEWPORT_ENV: &str = "VOXEL_NATIVE_QA_EXACT_VIEWPORT";

fn env_value_is_enabled(raw: Option<&str>) -> bool {
    raw.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn exact_qa_viewport_requested(qa_requested: bool, exact_viewport: Option<&str>) -> bool {
    qa_requested && env_value_is_enabled(exact_viewport)
}

fn configured_exact_qa_viewport() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        return exact_qa_viewport_requested(
            qa::qa_enabled(),
            std::env::var(QA_EXACT_VIEWPORT_ENV).ok().as_deref(),
        );
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

fn bounded_window_extent(raw: Option<&str>, fallback: f32, minimum: f32) -> f32 {
    raw.and_then(|value| value.trim().parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(minimum, 8192.0))
        .unwrap_or(fallback)
}

fn configured_window_resolution() -> (f32, f32) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        return (
            bounded_window_extent(
                std::env::var("VOXEL_NATIVE_WINDOW_WIDTH").ok().as_deref(),
                DEFAULT_WINDOW_WIDTH,
                320.0,
            ),
            bounded_window_extent(
                std::env::var("VOXEL_NATIVE_WINDOW_HEIGHT").ok().as_deref(),
                DEFAULT_WINDOW_HEIGHT,
                240.0,
            ),
        );
    }
    #[cfg(target_arch = "wasm32")]
    {
        (DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT)
    }
}

fn primary_window_for_viewport(
    title: String,
    width: f32,
    height: f32,
    exact_qa_viewport: bool,
) -> Window {
    let resolution = if exact_qa_viewport {
        // Diagnostic image experiments compare exact physical pixels. A
        // borderless, non-resizable QA window avoids Windows maximizing a
        // decorated 1920x1080 client surface down to the work area.
        WindowResolution::new(width, height).with_scale_factor_override(1.0)
    } else {
        (width, height).into()
    };
    Window {
        title,
        resolution,
        resizable: !exact_qa_viewport,
        decorations: !exact_qa_viewport,
        enabled_buttons: if exact_qa_viewport {
            bevy::window::EnabledButtons {
                minimize: false,
                maximize: false,
                close: true,
            }
        } else {
            default()
        },
        // AutoVsync caps the frame rate to the monitor refresh and blocks on
        // the compositor. Uncapped FPS remains a deliberate benchmark-only
        // policy rather than a window-shape side effect.
        present_mode: bevy::window::PresentMode::AutoVsync,
        ..default()
    }
}

fn main() -> AppExit {
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
            let exact_qa_viewport = configured_exact_qa_viewport();
            let (window_width, window_height) = configured_window_resolution();
            let plugins = DefaultPlugins.set(WindowPlugin {
                primary_window: Some(primary_window_for_viewport(
                    configured_window_title(),
                    window_width,
                    window_height,
                    exact_qa_viewport,
                )),
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
        .insert_resource(winit_settings_for_loop_policy(
            loop_policy_for_game_state(&menu::GameState::MainMenu),
            false,
        ))
        // MSAA off: on integrated GPUs (e.g. Vega 8 in Ryzen 5700G)
        // 4x MSAA on HDR (Rgba16Float) buffers quadruples bandwidth and
        // cuts FPS by 30–50%. With the greedy-mesh block-aligned UVs and
        // aggressive fog, visible aliasing is minimal. Users who want it
        // back can set Msaa::Sample2 in High graphics mode.
        .insert_resource(Msaa::Off)
        .init_resource::<bot_command::BotCommandStateMachine>()
        .add_plugins(agent_control::AgentControlPlugin)
        .add_plugins(live_link::LiveLinkPlugin)
        .add_plugins(mission_control::MissionControlPlugin)
        .add_plugins(sketch_model::SketchModelPlugin)
        .add_plugins(ambient::AmbientPlugin)
        // Register the render-only foliage material before WorldPlugin builds
        // its block material library. This wind path never enters gameplay
        // physics, so shuttle/player handling remains deterministic.
        .add_plugins(vegetation::VegetationPlugin)
        // The water spectrum is likewise presentation-only: one shared
        // material, constant-work weather uniforms, no fluid authority.
        .add_plugins(water::WaterOpticsPlugin)
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
        .add_plugins(planetary_streaming::PlanetaryStreamingPlugin)
        .add_plugins(bots::BotsPlugin)
        .add_plugins(villagers::VillagersPlugin)
        .add_plugins(feedback_audio::FeedbackAudioPlugin)
        .add_plugins(ships::ShipPlugin)
        .add_plugins(creator_library::CreatorLibraryPlugin)
        .add_plugins(object_lab::ObjectLabPlugin)
        .add_plugins(mode::ModePlugin)
        .add_plugins(neurocore::NeuroCorePlugin)
        .add_plugins(qa::QaPlugin)
        .add_plugins(toolbelt::ToolbeltPlugin)
        .add_plugins(commands::CommandDeckPlugin)
        .add_plugins(sculpt::SculptPlugin)
        .add_systems(Update, sync_winit_loop_with_game_state)
        .add_systems(Startup, print_controls)
        .run()
}

fn loop_policy_for_game_state(state: &menu::GameState) -> LoopPolicy {
    match state {
        menu::GameState::MainMenu => LoopPolicy::ReactiveLowPower(MENU_LOW_POWER_INTERVAL),
        menu::GameState::Paused => LoopPolicy::ReactiveLowPower(PAUSED_LOW_POWER_INTERVAL),
        menu::GameState::InGame => LoopPolicy::Continuous,
    }
}

fn winit_settings_for_loop_policy(policy: LoopPolicy, live_link_active: bool) -> WinitSettings {
    if live_link_active {
        return WinitSettings {
            focused_mode: UpdateMode::Continuous,
            unfocused_mode: UpdateMode::Continuous,
        };
    }

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
    live_link: Res<live_link::LiveLink>,
    mut settings: ResMut<WinitSettings>,
) {
    let desired = winit_settings_for_loop_policy(
        loop_policy_for_game_state(state.get()),
        live_link.is_active(),
    );
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
    env_value_is_enabled(std::env::var(name).ok().as_deref())
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
        let settings = winit_settings_for_loop_policy(
            LoopPolicy::ReactiveLowPower(MENU_LOW_POWER_INTERVAL),
            false,
        );

        assert_eq!(
            settings.focused_mode,
            UpdateMode::reactive_low_power(MENU_LOW_POWER_INTERVAL)
        );
        assert_eq!(
            settings.unfocused_mode,
            UpdateMode::reactive_low_power(UNFOCUSED_LOW_POWER_INTERVAL)
        );
    }

    #[test]
    fn live_link_keeps_both_instances_continuously_updating_when_unfocused() {
        let settings = winit_settings_for_loop_policy(
            LoopPolicy::ReactiveLowPower(MENU_LOW_POWER_INTERVAL),
            true,
        );

        assert_eq!(settings.focused_mode, UpdateMode::Continuous);
        assert_eq!(settings.unfocused_mode, UpdateMode::Continuous);
    }

    #[test]
    fn instance_window_title_distinguishes_live_link_roles() {
        assert_eq!(instance_window_title(None), DEFAULT_WINDOW_TITLE);
        assert_eq!(
            instance_window_title(Some("CODEX QA")),
            "Voxel Native [CODEX QA] // Codex Engineering"
        );
        assert_eq!(
            instance_window_title(Some("  LIVE SPECTATOR  ")),
            "Voxel Native [LIVE SPECTATOR] // Codex Engineering"
        );
    }

    #[test]
    fn instance_window_title_bounds_and_sanitizes_external_labels() {
        let title = instance_window_title(Some("12345678901234567890123456789012EXTRA\nINVISIBLE"));
        assert_eq!(
            title,
            "Voxel Native [12345678901234567890123456789012] // Codex Engineering"
        );
        assert!(!title.contains('\n'));
    }

    #[test]
    fn qa_window_extent_accepts_the_responsive_matrix_and_rejects_bad_values() {
        for (raw, expected) in [
            ("320", 320.0),
            ("800", 800.0),
            ("1280", 1280.0),
            ("1920", 1920.0),
            ("3440", 3440.0),
        ] {
            assert_eq!(bounded_window_extent(Some(raw), 1280.0, 320.0), expected);
        }
        assert_eq!(bounded_window_extent(Some(" 480 "), 720.0, 240.0), 480.0);
        assert_eq!(bounded_window_extent(Some("100"), 1280.0, 320.0), 320.0);
        assert_eq!(bounded_window_extent(Some("99999"), 1280.0, 320.0), 8192.0);
        for raw in ["", "not-a-number", "NaN", "inf", "-inf"] {
            assert_eq!(bounded_window_extent(Some(raw), 1280.0, 320.0), 1280.0);
        }
        assert_eq!(bounded_window_extent(None, 720.0, 240.0), 720.0);
    }

    #[test]
    fn exact_viewport_is_strictly_qa_scoped() {
        for enabled in ["1", " true ", "YES", "on"] {
            assert!(exact_qa_viewport_requested(true, Some(enabled)));
        }
        for disabled in [None, Some(""), Some("0"), Some("false"), Some("unknown")] {
            assert!(!exact_qa_viewport_requested(false, Some("1")));
            assert!(!exact_qa_viewport_requested(true, disabled));
        }
    }

    #[test]
    fn exact_qa_window_policy_does_not_change_normal_or_observer_windows() {
        let normal = primary_window_for_viewport("observer".into(), 1600.0, 900.0, false);
        assert_eq!(normal.title, "observer");
        assert_eq!(normal.resolution.physical_size(), UVec2::new(1600, 900));
        assert_eq!(normal.resolution.scale_factor_override(), None);
        assert!(normal.resizable);
        assert!(normal.decorations);
        assert_eq!(normal.enabled_buttons, default());

        let exact = primary_window_for_viewport("qa".into(), 1920.0, 1080.0, true);
        assert_eq!(exact.resolution.physical_size(), UVec2::new(1920, 1080));
        assert_eq!(exact.resolution.scale_factor_override(), Some(1.0));
        assert!(!exact.resizable);
        assert!(!exact.decorations);
        assert_eq!(
            exact.enabled_buttons,
            bevy::window::EnabledButtons {
                minimize: false,
                maximize: false,
                close: true,
            }
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
