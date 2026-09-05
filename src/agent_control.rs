//! Live agent-control bridge.
//!
//! Enable with `--agent-control` or `VOXEL_NATIVE_AGENT_CONTROL=1`.
//! The engine keeps a visible window open, polls `agent_control.ron`, and
//! maps those commands into synthetic player look/movement/fire controls.
//! This gives an external coding agent a practical way to play and inspect
//! the native game without OS-level input injection.

use bevy::app::AppExit;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::render::view::screenshot::ScreenshotManager;
use bevy::window::PrimaryWindow;
use bevy_egui::{egui, EguiContexts};
use serde::{Deserialize, Serialize};

use crate::menu::{GameState, PendingWorldLoad};
use crate::mode::{ActiveMode, ModeContext};
use crate::player::Player;
use crate::settings::{ActiveWorld, TimeMode, WorldMeta, WorldSettings};
use crate::toolbelt::{ToolbeltState, ToolbeltTool};
use crate::weapons::ActiveWeapon;
use crate::world::{ChunkStreamer, StreamingGovernor, VoxelWorld};

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

pub struct AgentControlPlugin;

impl Plugin for AgentControlPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(AgentControlState::from_env())
            .insert_resource(AgentControlRuntime::from_env())
            .add_systems(PreStartup, apply_agent_world_card_before_init)
            .add_systems(Startup, agent_control_startup_marker)
            .add_systems(
                PreUpdate,
                (
                    poll_agent_control_file,
                    agent_control_handoff,
                    apply_agent_build_mode,
                    apply_agent_inputs,
                )
                    .chain()
                    .run_if(agent_control_runtime_enabled),
            )
            .add_systems(
                Update,
                (
                    agent_control_enter_game,
                    agent_control_game_state,
                    agent_control_record_frame.run_if(agent_control_runtime_enabled),
                    agent_control_toggle_panel.run_if(agent_control_runtime_enabled),
                    agent_control_heartbeat.run_if(agent_control_runtime_enabled),
                    agent_control_overlay.run_if(agent_control_enabled),
                    agent_control_capture.run_if(agent_control_enabled),
                    agent_control_status.run_if(agent_control_enabled),
                    agent_control_exit.run_if(agent_control_enabled),
                )
                    .chain(),
            );
    }
}

#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct AgentControlState {
    pub runtime_enabled: bool,
    pub enabled: bool,
    pub sequence: u64,
    pub forward: f32,
    pub right: f32,
    pub up: f32,
    pub sprint: bool,
    pub fly: bool,
    pub look_x: f32,
    pub look_y: f32,
    pub yaw: Option<f32>,
    pub pitch: Option<f32>,
    pub fire: bool,
    pub scope: bool,
    pub keys: Vec<String>,
    pub mouse_buttons: Vec<String>,
    pub game_state: String,
    pub build_mode: String,
    pub build_tool: String,
    pub handoff: bool,
    pub screenshot: bool,
    pub exit: bool,
    pub hide_hud: bool,
    pub status: String,
}

impl Default for AgentControlState {
    fn default() -> Self {
        Self {
            runtime_enabled: false,
            enabled: false,
            sequence: 0,
            forward: 0.0,
            right: 0.0,
            up: 0.0,
            sprint: false,
            fly: true,
            look_x: 0.0,
            look_y: 0.0,
            yaw: None,
            pitch: None,
            fire: false,
            scope: false,
            keys: Vec::new(),
            mouse_buttons: Vec::new(),
            game_state: String::new(),
            build_mode: String::new(),
            build_tool: String::new(),
            handoff: false,
            screenshot: false,
            exit: false,
            hide_hud: false,
            status: "agent control off".into(),
        }
    }
}

impl AgentControlState {
    fn from_env() -> Self {
        Self {
            runtime_enabled: agent_runtime_enabled(),
            enabled: agent_runtime_enabled(),
            status: "waiting for agent_control.ron".into(),
            ..default()
        }
    }

    #[inline]
    pub fn active(&self) -> bool {
        self.runtime_enabled && self.enabled
    }
}

#[derive(Resource, Debug)]
struct AgentControlRuntime {
    runtime_enabled: bool,
    auto_enter: bool,
    poll_timer: f32,
    status_timer: f32,
    screenshot_timer: f32,
    screenshot_interval: f32,
    last_sequence_for_screenshot: u64,
    last_sequence_for_exit: u64,
    last_handoff_sequence: u64,
    screenshot_index: usize,
    frames: u64,
    total_dt: f32,
    last_frame_ms: f32,
    max_frame_ms: f32,
    stall_count: u64,
    stall_threshold_ms: f32,
    last_command_seconds: f32,
    last_error: Option<String>,
    in_game_frames: u32,
    synthetic_keys: Vec<KeyCode>,
    synthetic_mouse_buttons: Vec<MouseButton>,
    #[cfg(not(target_arch = "wasm32"))]
    control_path: PathBuf,
    #[cfg(not(target_arch = "wasm32"))]
    session_dir: PathBuf,
}

impl AgentControlRuntime {
    fn from_env() -> Self {
        let runtime_enabled = agent_runtime_enabled();
        #[cfg(not(target_arch = "wasm32"))]
        let control_path = std::env::var("VOXEL_NATIVE_AGENT_CONTROL_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("agent_control.ron"));
        #[cfg(not(target_arch = "wasm32"))]
        let session_dir =
            PathBuf::from("agent_runs").join(format!("live_{}", crate::platform::now_epoch()));

        Self {
            runtime_enabled,
            auto_enter: !env_flag("VOXEL_NATIVE_AGENT_NO_AUTO_ENTER")
                && !std::env::args().any(|arg| arg == "--agent-no-auto-enter"),
            poll_timer: 0.0,
            status_timer: 0.0,
            screenshot_timer: 0.0,
            screenshot_interval: env_f32("VOXEL_NATIVE_AGENT_SCREENSHOT_INTERVAL")
                .map(|seconds| {
                    if seconds <= 0.0 {
                        0.0
                    } else {
                        seconds.clamp(0.25, 120.0)
                    }
                })
                .unwrap_or(0.0),
            last_sequence_for_screenshot: 0,
            last_sequence_for_exit: 0,
            last_handoff_sequence: 0,
            screenshot_index: 0,
            frames: 0,
            total_dt: 0.0,
            last_frame_ms: 0.0,
            max_frame_ms: 0.0,
            stall_count: 0,
            stall_threshold_ms: env_f32("VOXEL_NATIVE_AGENT_STALL_MS").unwrap_or(100.0),
            last_command_seconds: 0.0,
            last_error: None,
            in_game_frames: 0,
            synthetic_keys: Vec::new(),
            synthetic_mouse_buttons: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            control_path,
            #[cfg(not(target_arch = "wasm32"))]
            session_dir,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct AgentControlCommand {
    enabled: bool,
    sequence: u64,
    forward: f32,
    right: f32,
    up: f32,
    sprint: bool,
    fly: bool,
    look_x: f32,
    look_y: f32,
    yaw: Option<f32>,
    pitch: Option<f32>,
    fire: bool,
    scope: bool,
    keys: Vec<String>,
    mouse_buttons: Vec<String>,
    game_state: String,
    build_mode: String,
    build_tool: String,
    handoff: bool,
    screenshot: bool,
    exit: bool,
    hide_hud: bool,
}

impl Default for AgentControlCommand {
    fn default() -> Self {
        Self {
            enabled: true,
            sequence: 0,
            forward: 0.0,
            right: 0.0,
            up: 0.0,
            sprint: false,
            fly: true,
            look_x: 0.0,
            look_y: 0.0,
            yaw: None,
            pitch: None,
            fire: false,
            scope: false,
            keys: Vec::new(),
            mouse_buttons: Vec::new(),
            game_state: String::new(),
            build_mode: String::new(),
            build_tool: String::new(),
            handoff: false,
            screenshot: false,
            exit: false,
            hide_hud: false,
        }
    }
}

#[derive(Debug, Serialize)]
struct AgentLiveStatus {
    seconds: f32,
    game_state: String,
    command_sequence: u64,
    command_status: String,
    command_forward: f32,
    command_right: f32,
    command_up: f32,
    command_sprint: bool,
    command_fire: bool,
    command_scope: bool,
    command_keys: Vec<String>,
    command_mouse_buttons: Vec<String>,
    command_game_state: String,
    command_build_mode: String,
    command_build_tool: String,
    command_handoff: bool,
    command_screenshot: bool,
    command_exit: bool,
    weapon: String,
    toolbelt_live: bool,
    toolbelt_palette_open: bool,
    toolbelt_tool: String,
    toolbelt_status: String,
    position: [f32; 3],
    yaw: f32,
    pitch: f32,
    flying: bool,
    fps: f32,
    average_fps: f32,
    frame_ms: f32,
    last_frame_ms: f32,
    max_frame_ms: f32,
    frames: u64,
    stall_count: u64,
    loaded_chunks: usize,
    mesh_entities: usize,
    pending_terrain: usize,
    pending_meshes: usize,
    dirty_chunks: usize,
    render_distance: i32,
    control_enabled: bool,
    last_command_seconds: f32,
    last_error: Option<String>,
    screenshot_count: usize,
    in_game_frames: u32,
    last_screenshot: Option<String>,
    session_dir: String,
    time_of_day: f32,
    world_name: String,
    visual_preset: String,
}

#[derive(Debug, Serialize)]
struct AgentBridgeStatus<'a> {
    stage: &'a str,
    runtime_enabled: bool,
    enabled: bool,
    sequence: u64,
    status: &'a str,
    last_error: Option<&'a str>,
    control_path: String,
    session_dir: String,
}

fn agent_control_runtime_enabled(state: Res<AgentControlState>) -> bool {
    state.runtime_enabled
}

fn agent_control_enabled(state: Res<AgentControlState>) -> bool {
    state.active()
}

fn runtime_control_path(runtime: &AgentControlRuntime) -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        runtime.control_path.to_string_lossy().to_string()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = runtime;
        "browser".into()
    }
}

fn runtime_session_dir(runtime: &AgentControlRuntime) -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        runtime.session_dir.to_string_lossy().to_string()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = runtime;
        "browser".into()
    }
}

fn write_boot_status(
    runtime: &AgentControlRuntime,
    state: &AgentControlState,
    stage: &'static str,
) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Err(e) = std::fs::create_dir_all(&runtime.session_dir) {
            warn!(
                "agent control: could not create {}: {e}",
                runtime.session_dir.display()
            );
            return;
        }
        let status = AgentBridgeStatus {
            stage,
            runtime_enabled: state.runtime_enabled,
            enabled: state.enabled,
            sequence: state.sequence,
            status: &state.status,
            last_error: runtime.last_error.as_deref(),
            control_path: runtime_control_path(runtime),
            session_dir: runtime_session_dir(runtime),
        };
        if let Ok(text) = ron::ser::to_string_pretty(&status, ron::ser::PrettyConfig::default()) {
            let _ = std::fs::write(runtime.session_dir.join("bridge.ron"), text);
        }
    }
}

fn poll_agent_control_file(
    time: Res<Time>,
    mut state: ResMut<AgentControlState>,
    mut runtime: ResMut<AgentControlRuntime>,
) {
    if !runtime.runtime_enabled {
        return;
    }
    runtime.poll_timer -= time.delta_seconds();
    if runtime.poll_timer > 0.0 {
        return;
    }
    runtime.poll_timer = 0.05;

    #[cfg(not(target_arch = "wasm32"))]
    {
        ensure_control_file(&runtime.control_path);
        let Ok(text) = std::fs::read_to_string(&runtime.control_path) else {
            state.status = format!("cannot read {}", runtime.control_path.display());
            return;
        };
        match ron::from_str::<AgentControlCommand>(&text) {
            Ok(command) => {
                runtime.last_command_seconds = time.elapsed_seconds();
                runtime.last_error = None;
                apply_command(&mut state, command);
            }
            Err(e) => {
                let error = format!("control parse error: {e}");
                runtime.last_error = Some(error.clone());
                state.status = error;
            }
        }
    }
}

fn apply_command(state: &mut AgentControlState, command: AgentControlCommand) {
    state.enabled = command.enabled;
    state.sequence = command.sequence;
    state.forward = command.forward.clamp(-1.0, 1.0);
    state.right = command.right.clamp(-1.0, 1.0);
    state.up = command.up.clamp(-1.0, 1.0);
    state.sprint = command.sprint;
    state.fly = command.fly;
    state.look_x = command.look_x.clamp(-4.0, 4.0);
    state.look_y = command.look_y.clamp(-4.0, 4.0);
    state.yaw = command.yaw;
    state.pitch = command.pitch.map(|p| p.clamp(-1.54, 1.54));
    state.fire = command.fire;
    state.scope = command.scope;
    state.keys = command.keys;
    state.mouse_buttons = command.mouse_buttons;
    state.game_state = command.game_state;
    state.build_mode = command.build_mode;
    state.build_tool = command.build_tool;
    state.handoff = command.handoff;
    if state.handoff {
        state.enabled = false;
    }
    state.screenshot = command.screenshot;
    state.exit = command.exit;
    state.hide_hud = command.hide_hud;
    state.status = if state.enabled {
        "agent live".into()
    } else {
        "agent paused".into()
    };
}

fn agent_control_handoff(
    state: Res<AgentControlState>,
    mut runtime: ResMut<AgentControlRuntime>,
    game: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mode: Option<ResMut<ModeContext>>,
    toolbelt: Option<ResMut<ToolbeltState>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    if !state.runtime_enabled {
        return;
    }
    let requested = normalized_input_name(&state.game_state);
    if !state.enabled {
        release_synthetic_inputs(&mut runtime, &mut keys, &mut mouse);
    }

    let wants_handoff =
        state.handoff || matches!(requested.as_str(), "HANDOFF" | "PLAYER" | "MANUAL");
    if !wants_handoff || state.sequence == runtime.last_handoff_sequence {
        return;
    }
    runtime.last_handoff_sequence = state.sequence;

    release_synthetic_inputs(&mut runtime, &mut keys, &mut mouse);

    if let Some(mut mode) = mode {
        if mode.mode != ActiveMode::Combat {
            mode.set(ActiveMode::Combat, "Agent handed control back to player.");
        }
    }
    if let Some(mut toolbelt) = toolbelt {
        toolbelt.live = false;
        toolbelt.palette_open = false;
        toolbelt.status = "Agent handoff complete. Normal player controls restored.".into();
    }
    if *game.get() == GameState::Paused {
        next.set(GameState::InGame);
    }
}
fn agent_control_game_state(
    state: Res<AgentControlState>,
    runtime: Res<AgentControlRuntime>,
    game: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
) {
    if !state.runtime_enabled || !state.enabled || !runtime.auto_enter {
        return;
    }

    let requested = normalized_input_name(&state.game_state);
    match requested.as_str() {
        "" => {
            let testing_ingame = !state.build_mode.trim().is_empty()
                || !state.build_tool.trim().is_empty()
                || state.screenshot
                || state.fire
                || state.scope
                || !state.mouse_buttons.is_empty();
            if testing_ingame && *game.get() == GameState::Paused {
                next.set(GameState::InGame);
            }
        }
        "INGAME" | "GAME" | "PLAY" | "RESUME" => {
            if *game.get() != GameState::InGame {
                next.set(GameState::InGame);
            }
        }
        "PAUSED" | "PAUSE" => {
            if *game.get() == GameState::InGame {
                next.set(GameState::Paused);
            }
        }
        _ => {}
    }
}

fn agent_control_enter_game(
    state: Res<AgentControlState>,
    runtime: Res<AgentControlRuntime>,
    game: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut pending: ResMut<PendingWorldLoad>,
    mut settings: ResMut<WorldSettings>,
    mut commands: Commands,
    active: Option<Res<ActiveWorld>>,
) {
    if !state.active() || !runtime.auto_enter || *game.get() != GameState::MainMenu {
        return;
    }
    if active.is_none() {
        let seed = env_u32("VOXEL_NATIVE_AGENT_SEED").unwrap_or(settings.seed);
        // Never reuse the leftover `agent_control` world: it has ~1000
        // edited chunks and a NeonShuttle save overlay, which hid the
        // frontier postcard behind floating green islands.
        let world_name = env_string("VOXEL_NATIVE_AGENT_WORLD")
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "cinematic_look_pass".into());
        let mut meta = WorldMeta::new(world_name, seed);
        meta.time_mode = TimeMode::Fixed;
        meta.time_of_day = env_f32("VOXEL_NATIVE_AGENT_HOUR")
            .unwrap_or(10.8)
            .clamp(0.0, 24.0);
        settings.seed = seed;
        settings.time_mode = meta.time_mode;
        settings.time_of_day = meta.time_of_day;
        // Ignore voxel-native-save.ron's NeonShuttle / Fog presets so
        // agent stills actually show generated mesas + Clear weather.
        settings.visual_preset = crate::settings::VisualPreset::NaturalWorld;
        settings
            .weather
            .apply_preset(crate::settings::WeatherPreset::Clear);
        apply_agent_mode_card(&mut settings);
        info!(
            "agent control: entering world '{}' seed={} hour={:.2} preset={:?} graphics={:?} rd={}",
            meta.name,
            seed,
            meta.time_of_day,
            settings.visual_preset,
            settings.graphics,
            settings.render_distance
        );
        commands.insert_resource(ActiveWorld { meta });
        pending.0 = true;
    }
    next.set(GameState::InGame);
}

/// Apply Fast/Cinematic agent cards before `init_world` bakes swatches,
/// so a Fast iGPU session does not wait on Balanced 128² textures.
fn apply_agent_world_card_before_init(settings: Option<ResMut<WorldSettings>>) {
    if !agent_runtime_enabled() {
        return;
    }
    let Some(mut settings) = settings else {
        return;
    };
    apply_agent_mode_card(&mut settings);
}

fn apply_agent_mode_card(settings: &mut WorldSettings) {
    if env_flag("VOXEL_NATIVE_AGENT_CINEMATIC") {
        settings.apply_world_mode_card(crate::settings::WorldModeCard::Cinematic);
        settings.visual_preset = crate::settings::VisualPreset::NaturalWorld;
        settings
            .weather
            .apply_preset(crate::settings::WeatherPreset::Clear);
        // Software Vulkan fills a 56-chunk disc too slowly for a
        // 20s still; 40 chunks still covers the spawn postcard.
        settings.render_distance = env_u32("VOXEL_NATIVE_AGENT_RD")
            .unwrap_or(40)
            .clamp(8, 64);
    } else if env_flag("VOXEL_NATIVE_AGENT_FAST") {
        settings.apply_world_mode_card(crate::settings::WorldModeCard::FastLaptop);
        settings.visual_preset = crate::settings::VisualPreset::NaturalWorld;
        settings
            .weather
            .apply_preset(crate::settings::WeatherPreset::Clear);
        if let Some(rd) = env_u32("VOXEL_NATIVE_AGENT_RD") {
            settings.render_distance = rd.clamp(8, 48);
        }
    }
}

fn agent_control_startup_marker(runtime: Res<AgentControlRuntime>, state: Res<AgentControlState>) {
    if !runtime.runtime_enabled {
        return;
    }
    info!(
        "agent control: enabled, control file {:?}, session {:?}, auto_enter {}",
        runtime_control_path(&runtime),
        runtime_session_dir(&runtime),
        runtime.auto_enter
    );
    write_boot_status(&runtime, &state, "startup");
}

fn agent_control_heartbeat(runtime: Res<AgentControlRuntime>, state: Res<AgentControlState>) {
    if state.is_changed() {
        write_boot_status(&runtime, &state, "heartbeat");
    }
}

fn apply_agent_build_mode(
    game: Res<State<GameState>>,
    state: Res<AgentControlState>,
    mut runtime: ResMut<AgentControlRuntime>,
    mode: Option<ResMut<ModeContext>>,
    toolbelt: Option<ResMut<ToolbeltState>>,
) {
    if !state.active() || *game.get() != GameState::InGame {
        return;
    }
    if state.build_mode.trim().is_empty() && state.build_tool.trim().is_empty() {
        return;
    }

    let Some(mut mode) = mode else {
        runtime.last_error = Some("mode resource unavailable".into());
        return;
    };
    let current_tool = mode.build_tool().unwrap_or_else(|| {
        toolbelt
            .as_deref()
            .map(|toolbelt| toolbelt.tool)
            .unwrap_or(ToolbeltTool::DrawRect)
    });
    let tool = if state.build_tool.trim().is_empty() {
        current_tool
    } else {
        match parse_toolbelt_tool(&state.build_tool) {
            Some(tool) => tool,
            None => {
                runtime.last_error = Some(format!("unknown build tool '{}'", state.build_tool));
                return;
            }
        }
    };

    if let Some(mut toolbelt) = toolbelt {
        toolbelt.tool = tool;
    }

    let normalized_mode = normalized_input_name(&state.build_mode);
    match normalized_mode.as_str() {
        "" => {}
        "COMBAT" | "OFF" | "CLOSED" => {
            mode.set(ActiveMode::Combat, "Agent set Combat controls.");
        }
        "PICKER" | "BUILDPICKER" => {
            mode.set(
                ActiveMode::BuildPicker { tool },
                format!("Agent Build Picker: {}.", tool.label()),
            );
        }
        "LIVE" | "BUILDLIVE" => {
            mode.set(
                ActiveMode::BuildLive { tool },
                format!("Agent Build Live: {}. {}", tool.label(), tool.hint()),
            );
        }
        _ => {
            runtime.last_error = Some(format!("unknown build mode '{}'", state.build_mode));
        }
    }
}

fn apply_agent_inputs(
    game: Res<State<GameState>>,
    state: Res<AgentControlState>,
    mut runtime: ResMut<AgentControlRuntime>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    if !state.active() || *game.get() != GameState::InGame {
        release_synthetic_inputs(&mut runtime, &mut keys, &mut mouse);
        return;
    }

    let mut input_errors = Vec::new();
    let next_keys: Vec<KeyCode> = state
        .keys
        .iter()
        .filter_map(|name| match parse_key_code(name) {
            Some(key) => Some(key),
            None => {
                input_errors.push(format!("unknown key '{name}'"));
                None
            }
        })
        .collect();
    let next_buttons: Vec<MouseButton> = state
        .mouse_buttons
        .iter()
        .filter_map(|name| match parse_mouse_button(name) {
            Some(button) => Some(button),
            None => {
                input_errors.push(format!("unknown mouse button '{name}'"));
                None
            }
        })
        .collect();

    let previous_keys = runtime.synthetic_keys.clone();
    for &key in &previous_keys {
        if !next_keys.contains(&key) {
            keys.release(key);
        }
    }
    for &key in &next_keys {
        if !previous_keys.contains(&key) {
            keys.press(key);
        }
    }
    runtime.synthetic_keys = next_keys;

    let previous_buttons = runtime.synthetic_mouse_buttons.clone();
    for &button in &previous_buttons {
        if !next_buttons.contains(&button) {
            mouse.release(button);
        }
    }
    for &button in &next_buttons {
        if !previous_buttons.contains(&button) {
            mouse.press(button);
        }
    }
    runtime.synthetic_mouse_buttons = next_buttons;

    if input_errors.is_empty()
        && runtime
            .last_error
            .as_deref()
            .unwrap_or("")
            .starts_with("unknown")
    {
        runtime.last_error = None;
    } else if !input_errors.is_empty() {
        runtime.last_error = Some(input_errors.join(", "));
    }
}

fn release_synthetic_inputs(
    runtime: &mut AgentControlRuntime,
    keys: &mut ButtonInput<KeyCode>,
    mouse: &mut ButtonInput<MouseButton>,
) {
    for key in runtime.synthetic_keys.drain(..) {
        keys.release(key);
    }
    for button in runtime.synthetic_mouse_buttons.drain(..) {
        mouse.release(button);
    }
}

fn agent_control_record_frame(
    time: Res<Time>,
    game: Res<State<GameState>>,
    mut runtime: ResMut<AgentControlRuntime>,
) {
    let dt = time.delta_seconds().clamp(0.0, 5.0);
    let frame_ms = dt * 1000.0;
    runtime.frames += 1;
    runtime.total_dt += dt;
    runtime.last_frame_ms = frame_ms;
    runtime.max_frame_ms = runtime.max_frame_ms.max(frame_ms);
    if frame_ms >= runtime.stall_threshold_ms {
        runtime.stall_count += 1;
    }
    if *game.get() == GameState::InGame {
        runtime.in_game_frames = runtime.in_game_frames.saturating_add(1);
    } else {
        runtime.in_game_frames = 0;
    }
}

fn agent_control_toggle_panel(
    mut contexts: EguiContexts,
    mut state: ResMut<AgentControlState>,
    runtime: Res<AgentControlRuntime>,
    photo: Option<Res<crate::hud::PhotoMode>>,
) {
    if photo.map(|p| p.hidden).unwrap_or(false) || state.hide_hud {
        return;
    }
    let anchor = if state.enabled {
        egui::Align2::RIGHT_BOTTOM
    } else {
        egui::Align2::LEFT_BOTTOM
    };
    let offset = if state.enabled {
        egui::vec2(-14.0, -14.0)
    } else {
        egui::vec2(14.0, -14.0)
    };

    egui::Area::new(egui::Id::new("agent_control_toggle_panel"))
        .anchor(anchor, offset)
        .show(contexts.ctx_mut(), |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 190))
                .stroke(egui::Stroke::new(
                    1.0,
                    if state.enabled {
                        egui::Color32::from_rgb(0, 240, 255)
                    } else {
                        egui::Color32::from_rgb(120, 160, 170)
                    },
                ))
                .rounding(egui::Rounding::same(6.0))
                .inner_margin(egui::Margin::same(6.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let label = if state.enabled {
                            "AI LIVE: AN"
                        } else {
                            "AI LIVE: AUS"
                        };
                        ui.label(
                            egui::RichText::new(label)
                                .monospace()
                                .color(egui::Color32::from_rgb(230, 255, 245)),
                        );
                        let button_label = if state.enabled { "AUS" } else { "AN" };
                        if ui.button(button_label).clicked() {
                            let next_enabled = !state.enabled;
                            state.sequence = state.sequence.saturating_add(1);
                            state.enabled = next_enabled;
                            state.handoff = !next_enabled;
                            state.forward = 0.0;
                            state.right = 0.0;
                            state.up = 0.0;
                            state.look_x = 0.0;
                            state.look_y = 0.0;
                            state.fire = false;
                            state.scope = false;
                            state.keys.clear();
                            state.mouse_buttons.clear();
                            state.screenshot = false;
                            state.exit = false;
                            state.build_mode = if next_enabled {
                                String::new()
                            } else {
                                "combat".into()
                            };
                            state.build_tool.clear();
                            state.game_state = if next_enabled {
                                String::new()
                            } else {
                                "ingame".into()
                            };
                            state.status = if next_enabled {
                                "agent live".into()
                            } else {
                                "agent paused".into()
                            };
                            write_agent_control_file(&runtime, &state);
                        }
                    });
                });
        });
}

fn agent_control_overlay(
    mut contexts: EguiContexts,
    state: Res<AgentControlState>,
    runtime: Res<AgentControlRuntime>,
    game: Res<State<GameState>>,
    active_weapon: Option<Res<ActiveWeapon>>,
    toolbelt: Option<Res<ToolbeltState>>,
    player: Query<(&Transform, &Player)>,
    photo: Option<Res<crate::hud::PhotoMode>>,
) {
    if photo.map(|p| p.hidden).unwrap_or(false) || state.hide_hud {
        return;
    }
    let Ok((transform, player)) = player.get_single() else {
        return;
    };
    egui::Area::new(egui::Id::new("agent_control_live_overlay"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
        .show(contexts.ctx_mut(), |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 220))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(0, 240, 255)))
                .rounding(egui::Rounding::same(6.0))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("AGENT CONTROL LIVE")
                            .color(egui::Color32::from_rgb(0, 240, 255))
                            .size(17.0)
                            .strong(),
                    );
                    ui.label(format!(
                        "{}  seq {}  pos {:.0}/{:.0}/{:.0}",
                        state.status,
                        state.sequence,
                        transform.translation.x,
                        transform.translation.y,
                        transform.translation.z
                    ));
                    ui.label(format!(
                        "move f {:.1} r {:.1} u {:.1}  look {:.2}/{:.2}  fly {} fire {} scope {}",
                        state.forward,
                        state.right,
                        state.up,
                        state.look_x,
                        state.look_y,
                        player.flying,
                        state.fire,
                        state.scope
                    ));
                    ui.label(format!(
                        "keys {:?}  mouse {:?}",
                        state.keys, state.mouse_buttons
                    ));
                    ui.separator();
                    let ocr_color = egui::Color32::from_rgb(235, 255, 245);
                    ui.monospace(
                        egui::RichText::new(format!(
                            "OCR_STATE={:?} OCR_SEQ={} OCR_STATUS={}",
                            game.get(),
                            state.sequence,
                            state.status
                        ))
                        .color(ocr_color)
                        .size(14.0),
                    );
                    ui.monospace(
                        egui::RichText::new(format!(
                            "OCR_POS={:.1},{:.1},{:.1} OCR_YAW={:.2} OCR_PITCH={:.2}",
                            transform.translation.x,
                            transform.translation.y,
                            transform.translation.z,
                            player.yaw,
                            player.pitch
                        ))
                        .color(ocr_color)
                        .size(14.0),
                    );
                    ui.monospace(
                        egui::RichText::new(format!(
                            "OCR_FRAME_MS={:.1} OCR_MAX_MS={:.1} OCR_STALLS={} OCR_INGAME_FRAMES={}",
                            runtime.last_frame_ms,
                            runtime.max_frame_ms,
                            runtime.stall_count,
                            runtime.in_game_frames
                        ))
                        .color(ocr_color)
                        .size(14.0),
                    );
                    ui.monospace(
                        egui::RichText::new(format!(
                            "OCR_FIRE={} OCR_SCOPE={} OCR_ERROR={}",
                            state.fire,
                            state.scope,
                            runtime.last_error.as_deref().unwrap_or("none")
                        ))
                        .color(ocr_color)
                        .size(14.0),
                    );
                    ui.monospace(
                        egui::RichText::new(format!(
                            "OCR_WEAPON={} OCR_TOOL={} OCR_KEYS={} OCR_MOUSE={}",
                            active_weapon
                                .as_deref()
                                .map(|weapon| format!("{:?}", weapon.kind))
                                .unwrap_or_else(|| "none".into()),
                            toolbelt
                                .as_deref()
                                .map(|toolbelt| format!("{:?}", toolbelt.tool))
                                .unwrap_or_else(|| "none".into()),
                            state.keys.join("+"),
                            state.mouse_buttons.join("+")
                        ))
                        .color(ocr_color)
                        .size(14.0),
                    );
                });
        });
}

fn agent_control_capture(
    time: Res<Time>,
    mut runtime: ResMut<AgentControlRuntime>,
    state: Res<AgentControlState>,
    mut screenshots: ResMut<ScreenshotManager>,
    windows: Query<Entity, With<PrimaryWindow>>,
) {
    runtime.screenshot_timer -= time.delta_seconds();
    let sequence_capture =
        state.screenshot && state.sequence != runtime.last_sequence_for_screenshot;
    let interval_capture = runtime.screenshot_interval > 0.0 && runtime.screenshot_timer <= 0.0;
    if !sequence_capture && !interval_capture {
        return;
    }
    if runtime.in_game_frames < 3 {
        return;
    }
    runtime.screenshot_timer = runtime.screenshot_interval;
    runtime.last_sequence_for_screenshot = state.sequence;

    let Ok(window) = windows.get_single() else {
        return;
    };

    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Err(e) = std::fs::create_dir_all(&runtime.session_dir) {
            warn!(
                "agent control: could not create {}: {e}",
                runtime.session_dir.display()
            );
            return;
        }
        let path = runtime
            .session_dir
            .join(format!("live_{:04}.png", runtime.screenshot_index));
        runtime.screenshot_index += 1;
        match screenshots.save_screenshot_to_disk(window, &path) {
            Ok(_) => info!("agent control: screenshot saved to {}", path.display()),
            Err(e) => warn!("agent control: screenshot failed: {e}"),
        }
    }
}

fn agent_control_status(
    time: Res<Time>,
    diagnostics: Res<DiagnosticsStore>,
    game: Res<State<GameState>>,
    world: Res<VoxelWorld>,
    streamer: Res<ChunkStreamer>,
    governor: Res<StreamingGovernor>,
    state: Res<AgentControlState>,
    settings: Res<WorldSettings>,
    active_world: Option<Res<ActiveWorld>>,
    mut runtime: ResMut<AgentControlRuntime>,
    active_weapon: Option<Res<ActiveWeapon>>,
    toolbelt: Option<Res<ToolbeltState>>,
    player: Query<(&Transform, &Player)>,
) {
    runtime.status_timer -= time.delta_seconds();
    if runtime.status_timer > 0.0 {
        return;
    }
    runtime.status_timer = 0.25;
    let Ok((transform, player)) = player.get_single() else {
        return;
    };
    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0) as f32;
    let frame_ms = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0) as f32;
    let average_fps = if runtime.total_dt > 0.0 {
        runtime.frames as f32 / runtime.total_dt
    } else {
        0.0
    };

    #[cfg(not(target_arch = "wasm32"))]
    {
        let last_screenshot = runtime.screenshot_index.checked_sub(1).map(|idx| {
            runtime
                .session_dir
                .join(format!("live_{idx:04}.png"))
                .to_string_lossy()
                .to_string()
        });
        let status = AgentLiveStatus {
            seconds: time.elapsed_seconds(),
            game_state: format!("{:?}", game.get()),
            command_sequence: state.sequence,
            command_status: state.status.clone(),
            command_forward: state.forward,
            command_right: state.right,
            command_up: state.up,
            command_sprint: state.sprint,
            command_fire: state.fire,
            command_scope: state.scope,
            command_keys: state.keys.clone(),
            command_mouse_buttons: state.mouse_buttons.clone(),
            command_game_state: state.game_state.clone(),
            command_build_mode: state.build_mode.clone(),
            command_build_tool: state.build_tool.clone(),
            command_handoff: state.handoff,
            command_screenshot: state.screenshot,
            command_exit: state.exit,
            weapon: active_weapon
                .as_deref()
                .map(|weapon| format!("{:?}", weapon.kind))
                .unwrap_or_else(|| "none".into()),
            toolbelt_live: toolbelt
                .as_deref()
                .map(|toolbelt| toolbelt.live)
                .unwrap_or(false),
            toolbelt_palette_open: toolbelt
                .as_deref()
                .map(|toolbelt| toolbelt.palette_open)
                .unwrap_or(false),
            toolbelt_tool: toolbelt
                .as_deref()
                .map(|toolbelt| format!("{:?}", toolbelt.tool))
                .unwrap_or_else(|| "none".into()),
            toolbelt_status: toolbelt
                .as_deref()
                .map(|toolbelt| toolbelt.status.clone())
                .unwrap_or_else(|| "none".into()),
            position: [
                transform.translation.x,
                transform.translation.y,
                transform.translation.z,
            ],
            yaw: player.yaw,
            pitch: player.pitch,
            flying: player.flying,
            fps,
            average_fps,
            frame_ms,
            last_frame_ms: runtime.last_frame_ms,
            max_frame_ms: runtime.max_frame_ms,
            frames: runtime.frames,
            stall_count: runtime.stall_count,
            loaded_chunks: world.chunks.len(),
            mesh_entities: streamer.entities.len(),
            pending_terrain: streamer.pending_terrain.len(),
            pending_meshes: streamer.pending_meshes.len(),
            dirty_chunks: streamer.dirty_queue.len() + world.edit_dirty_chunks.len(),
            render_distance: governor.effective_render_distance,
            control_enabled: state.enabled,
            last_command_seconds: runtime.last_command_seconds,
            last_error: runtime.last_error.clone(),
            screenshot_count: runtime.screenshot_index,
            in_game_frames: runtime.in_game_frames,
            last_screenshot,
            session_dir: runtime_session_dir(&runtime),
            time_of_day: settings.time_of_day,
            world_name: active_world
                .as_deref()
                .map(|world| world.meta.name.clone())
                .unwrap_or_default(),
            visual_preset: format!("{:?}", settings.visual_preset),
        };
        if let Err(e) = std::fs::create_dir_all(&runtime.session_dir) {
            warn!(
                "agent control: could not create {}: {e}",
                runtime.session_dir.display()
            );
            return;
        }
        let path = runtime.session_dir.join("status.ron");
        if let Ok(text) = ron::ser::to_string_pretty(&status, ron::ser::PrettyConfig::default()) {
            let _ = std::fs::write(path, text);
        }
    }
}

fn agent_control_exit(
    state: Res<AgentControlState>,
    mut runtime: ResMut<AgentControlRuntime>,
    mut exit: EventWriter<AppExit>,
) {
    if state.exit && state.sequence != runtime.last_sequence_for_exit {
        runtime.last_sequence_for_exit = state.sequence;
        exit.send(AppExit::Success);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_control_file(path: &std::path::Path) {
    if path.exists() {
        return;
    }
    let template = r#"(
    enabled: true,
    sequence: 0,
    forward: 0.0,
    right: 0.0,
    up: 0.0,
    sprint: false,
    fly: true,
    look_x: 0.0,
    look_y: 0.0,
    fire: false,
    scope: false,
    keys: [],
    mouse_buttons: [],
    game_state: "",
    build_mode: "",
    build_tool: "",
    handoff: false,
    screenshot: false,
    exit: false,
)"#;
    let _ = std::fs::write(path, template);
}

fn write_agent_control_file(runtime: &AgentControlRuntime, state: &AgentControlState) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let command = AgentControlCommand {
            enabled: state.enabled,
            sequence: state.sequence,
            forward: 0.0,
            right: 0.0,
            up: 0.0,
            sprint: false,
            fly: true,
            look_x: 0.0,
            look_y: 0.0,
            yaw: None,
            pitch: None,
            fire: false,
            scope: false,
            keys: Vec::new(),
            mouse_buttons: Vec::new(),
            game_state: state.game_state.clone(),
            build_mode: state.build_mode.clone(),
            build_tool: String::new(),
            handoff: state.handoff,
            screenshot: false,
            exit: false,
            hide_hud: state.hide_hud,
        };
        if let Ok(text) = ron::ser::to_string_pretty(&command, ron::ser::PrettyConfig::default()) {
            let _ = std::fs::write(&runtime.control_path, text);
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (runtime, state);
    }
}

fn normalized_input_name(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .flat_map(|c| c.to_uppercase())
        .collect()
}

fn parse_mouse_button(name: &str) -> Option<MouseButton> {
    match normalized_input_name(name).as_str() {
        "LMB" | "LEFT" | "MOUSELEFT" => Some(MouseButton::Left),
        "RMB" | "RIGHT" | "MOUSERIGHT" => Some(MouseButton::Right),
        "MMB" | "MIDDLE" | "MOUSEMIDDLE" => Some(MouseButton::Middle),
        _ => None,
    }
}

fn parse_toolbelt_tool(name: &str) -> Option<ToolbeltTool> {
    match normalized_input_name(name).as_str() {
        "NAV" | "NAVIGATE" | "INSPECT" | "NAVIGATEINSPECT" => Some(ToolbeltTool::Navigate),
        "FILL" | "DRAWRECT" | "RECT" | "RECTANGLE" | "RECTANGLEFILL" => {
            Some(ToolbeltTool::DrawRect)
        }
        "PUSH" | "SCULPT" | "PUSHPULL" | "PUSHPULLFACE" => Some(ToolbeltTool::Sculpt),
        "TOWER" | "SMARTTOWER" => Some(ToolbeltTool::SmartTower),
        "PLACE" | "BRUSHPLACE" | "PLACEBRUSH" => Some(ToolbeltTool::BrushPlace),
        "CUT" | "BRUSHCUT" | "CUTBRUSH" => Some(ToolbeltTool::BrushCut),
        "ROAD" | "CITYROAD" => Some(ToolbeltTool::CityRoad),
        "ZONE" | "DISTRICT" | "CITYDISTRICT" => Some(ToolbeltTool::CityDistrict),
        "SHELL" | "BUILDING" | "CITYBUILDING" => Some(ToolbeltTool::CityBuilding),
        "STAMP" | "FACADE" | "CITYFACADE" => Some(ToolbeltTool::CityFacade),
        "ANIM" | "ANIMATION" | "ANIMATIONPICK" => Some(ToolbeltTool::AnimationPick),
        _ => None,
    }
}

fn parse_key_code(name: &str) -> Option<KeyCode> {
    match normalized_input_name(name).as_str() {
        "A" | "KEYA" => Some(KeyCode::KeyA),
        "B" | "KEYB" => Some(KeyCode::KeyB),
        "C" | "KEYC" => Some(KeyCode::KeyC),
        "D" | "KEYD" => Some(KeyCode::KeyD),
        "E" | "KEYE" => Some(KeyCode::KeyE),
        "F" | "KEYF" => Some(KeyCode::KeyF),
        "G" | "KEYG" => Some(KeyCode::KeyG),
        "H" | "KEYH" => Some(KeyCode::KeyH),
        "I" | "KEYI" => Some(KeyCode::KeyI),
        "J" | "KEYJ" => Some(KeyCode::KeyJ),
        "K" | "KEYK" => Some(KeyCode::KeyK),
        "L" | "KEYL" => Some(KeyCode::KeyL),
        "M" | "KEYM" => Some(KeyCode::KeyM),
        "N" | "KEYN" => Some(KeyCode::KeyN),
        "O" | "KEYO" => Some(KeyCode::KeyO),
        "P" | "KEYP" => Some(KeyCode::KeyP),
        "Q" | "KEYQ" => Some(KeyCode::KeyQ),
        "R" | "KEYR" => Some(KeyCode::KeyR),
        "S" | "KEYS" => Some(KeyCode::KeyS),
        "T" | "KEYT" => Some(KeyCode::KeyT),
        "U" | "KEYU" => Some(KeyCode::KeyU),
        "V" | "KEYV" => Some(KeyCode::KeyV),
        "W" | "KEYW" => Some(KeyCode::KeyW),
        "X" | "KEYX" => Some(KeyCode::KeyX),
        "Y" | "KEYY" => Some(KeyCode::KeyY),
        "Z" | "KEYZ" => Some(KeyCode::KeyZ),
        "0" | "DIGIT0" => Some(KeyCode::Digit0),
        "1" | "DIGIT1" => Some(KeyCode::Digit1),
        "2" | "DIGIT2" => Some(KeyCode::Digit2),
        "3" | "DIGIT3" => Some(KeyCode::Digit3),
        "4" | "DIGIT4" => Some(KeyCode::Digit4),
        "5" | "DIGIT5" => Some(KeyCode::Digit5),
        "6" | "DIGIT6" => Some(KeyCode::Digit6),
        "7" | "DIGIT7" => Some(KeyCode::Digit7),
        "8" | "DIGIT8" => Some(KeyCode::Digit8),
        "9" | "DIGIT9" => Some(KeyCode::Digit9),
        "F1" => Some(KeyCode::F1),
        "F2" => Some(KeyCode::F2),
        "F3" => Some(KeyCode::F3),
        "F4" => Some(KeyCode::F4),
        "F5" => Some(KeyCode::F5),
        "F6" => Some(KeyCode::F6),
        "F7" => Some(KeyCode::F7),
        "F8" => Some(KeyCode::F8),
        "F9" => Some(KeyCode::F9),
        "F10" => Some(KeyCode::F10),
        "F11" => Some(KeyCode::F11),
        "F12" => Some(KeyCode::F12),
        "ESC" | "ESCAPE" => Some(KeyCode::Escape),
        "SPACE" => Some(KeyCode::Space),
        "TAB" => Some(KeyCode::Tab),
        "ENTER" | "RETURN" => Some(KeyCode::Enter),
        "SHIFT" | "SHIFTLEFT" | "LSHIFT" => Some(KeyCode::ShiftLeft),
        "SHIFTRIGHT" | "RSHIFT" => Some(KeyCode::ShiftRight),
        "CTRL" | "CONTROL" | "CONTROLLEFT" | "LCTRL" => Some(KeyCode::ControlLeft),
        "CONTROLRIGHT" | "RCTRL" => Some(KeyCode::ControlRight),
        "ALT" | "ALTLEFT" | "LALT" => Some(KeyCode::AltLeft),
        "ALTRIGHT" | "RALT" => Some(KeyCode::AltRight),
        "PERIOD" | "DOT" => Some(KeyCode::Period),
        "BRACKETLEFT" | "LEFTBRACKET" => Some(KeyCode::BracketLeft),
        "BRACKETRIGHT" | "RIGHTBRACKET" => Some(KeyCode::BracketRight),
        _ => None,
    }
}

fn agent_runtime_enabled() -> bool {
    env_flag("VOXEL_NATIVE_AGENT_CONTROL")
        || std::env::args().any(|arg| matches!(arg.as_str(), "--agent-control" | "--agent"))
}

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

fn env_f32(name: &str) -> Option<f32> {
    std::env::var(name).ok()?.trim().parse().ok()
}

fn env_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok()?.trim().parse().ok()
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
