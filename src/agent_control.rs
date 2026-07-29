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

use crate::icons::Icon;
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
        let mut meta = WorldMeta::new("agent_control".into(), seed);
        meta.time_mode = TimeMode::Fixed;
        meta.time_of_day = env_f32("VOXEL_NATIVE_AGENT_HOUR")
            .unwrap_or(10.8)
            .clamp(0.0, 24.0);
        settings.seed = seed;
        settings.time_mode = meta.time_mode;
        settings.time_of_day = meta.time_of_day;
        commands.insert_resource(ActiveWorld { meta });
        pending.0 = true;
    }
    next.set(GameState::InGame);
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

const AGENT_PANEL_VIEWPORT_MARGIN: f32 = 24.0;
const AGENT_CONTROL_PANEL_WIDTH: f32 = 390.0;
const AGENT_OVERLAY_PANEL_WIDTH: f32 = 720.0;
const AGENT_CONTROL_ACTION_WIDTH: f32 = 154.0;

fn adaptive_agent_panel_width(screen_width: f32, preferred_width: f32) -> f32 {
    let preferred_width = if preferred_width.is_finite() {
        preferred_width.max(1.0)
    } else {
        1.0
    };
    if !screen_width.is_finite() {
        return preferred_width;
    }

    (screen_width - AGENT_PANEL_VIEWPORT_MARGIN)
        .max(1.0)
        .min(preferred_width)
}

fn agent_control_action_spec(enabled: bool) -> (Icon, &'static str, bool) {
    if enabled {
        (Icon::Pause, "Agent live", true)
    } else {
        (Icon::Play, "Agent paused", false)
    }
}

fn agent_control_action(
    ui: &mut egui::Ui,
    enabled: bool,
    available: bool,
    theme: crate::theme::ThemeSettings,
) -> egui::Response {
    let (icon, label, selected) = agent_control_action_spec(enabled);
    ui.add_enabled_ui(available, |ui| {
        crate::ui_kit::icon_action_sized(
            ui,
            icon,
            label,
            selected,
            AGENT_CONTROL_ACTION_WIDTH,
            theme,
        )
    })
    .inner
}

fn set_agent_control_enabled(state: &mut AgentControlState, next_enabled: bool) -> bool {
    if state.enabled == next_enabled {
        return false;
    }

    state.sequence = state.sequence.saturating_add(1);
    state.enabled = next_enabled;
    state.handoff = !next_enabled;
    state.forward = 0.0;
    state.right = 0.0;
    state.up = 0.0;
    state.sprint = false;
    state.fly = true;
    state.look_x = 0.0;
    state.look_y = 0.0;
    state.yaw = None;
    state.pitch = None;
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
    true
}

fn paint_agent_panel_outline(
    ui: &egui::Ui,
    rect: egui::Rect,
    theme: crate::theme::ThemeSettings,
    animation_id: egui::Id,
    active: bool,
    resting_strength: f32,
) {
    let colors = theme.semantic();
    let active_amount = crate::theme::animate_bool_finite(
        ui.ctx(),
        animation_id,
        active,
        crate::theme::MotionRole::State,
    );
    let resting_strength = if resting_strength.is_finite() {
        resting_strength.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let amount = resting_strength + (1.0 - resting_strength) * active_amount;
    let core = if active {
        colors.outline_active
    } else {
        colors.outline_hover
    };
    crate::theme::paint_neon_outline(
        ui.painter(),
        rect,
        crate::theme::KANSO_VISUALS.corner_radius,
        colors.focus_glow,
        core,
        amount,
    );
}

fn agent_control_toggle_panel(
    mut contexts: EguiContexts,
    mut state: ResMut<AgentControlState>,
    runtime: Res<AgentControlRuntime>,
    settings: Res<WorldSettings>,
) {
    let theme = settings.theme;
    let colors = theme.semantic();
    let control_available = state.runtime_enabled && runtime.runtime_enabled;
    let signal_active = control_available && state.enabled && runtime.last_error.is_none();
    let control_owner = if !control_available {
        "UNAVAILABLE"
    } else if state.enabled {
        "AGENT"
    } else {
        "PLAYER"
    };
    let status_message = runtime
        .last_error
        .clone()
        .unwrap_or_else(|| state.status.clone());
    let status_color = if runtime.last_error.is_some() {
        colors.danger
    } else if state.enabled {
        colors.success
    } else {
        colors.text_muted
    };
    let Some(ctx) = contexts.try_ctx_mut() else {
        return;
    };
    let panel_width =
        adaptive_agent_panel_width(ctx.screen_rect().width(), AGENT_CONTROL_PANEL_WIDTH);

    egui::Area::new(egui::Id::new("agent_control_toggle_panel"))
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-14.0, -14.0))
        .show(ctx, |ui| {
            ui.set_width(panel_width);
            let panel = crate::ui_kit::toolbench_frame(theme)
                .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        crate::ui_kit::orbit_pulse(ui, signal_active, theme);
                        crate::ui_kit::status_chip(ui, Icon::Hud, "CONTROL", control_owner, theme);
                        let response =
                            agent_control_action(ui, state.enabled, control_available, theme);
                        if response.clicked() {
                            let next_enabled = !state.enabled;
                            if set_agent_control_enabled(&mut state, next_enabled) {
                                write_agent_control_file(&runtime, &state);
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(status_message)
                            .monospace()
                            .size(10.5)
                            .color(status_color),
                    );
                });
            paint_agent_panel_outline(
                ui,
                panel.response.rect,
                theme,
                egui::Id::new("agent_control_toggle_outline"),
                state.enabled && control_available,
                0.22,
            );
        });
}

fn agent_control_overlay(
    mut contexts: EguiContexts,
    state: Res<AgentControlState>,
    runtime: Res<AgentControlRuntime>,
    settings: Res<WorldSettings>,
    game: Res<State<GameState>>,
    active_weapon: Option<Res<ActiveWeapon>>,
    toolbelt: Option<Res<ToolbeltState>>,
    player: Query<(&Transform, &Player)>,
) {
    let Ok((transform, player)) = player.get_single() else {
        return;
    };
    let theme = settings.theme;
    let colors = theme.semantic();
    let Some(ctx) = contexts.try_ctx_mut() else {
        return;
    };
    let panel_width =
        adaptive_agent_panel_width(ctx.screen_rect().width(), AGENT_OVERLAY_PANEL_WIDTH);
    let signal_active = runtime.last_error.is_none();
    egui::Area::new(egui::Id::new("agent_control_live_overlay"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
        .show(ctx, |ui| {
            ui.set_width(panel_width);
            let panel = crate::ui_kit::toolbench_frame(theme)
                .inner_margin(egui::Margin::symmetric(14.0, 12.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        crate::ui_kit::orbit_pulse(ui, signal_active, theme);
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new("AGENT CONTROL // LIVE")
                                    .color(colors.accent)
                                    .size(16.0)
                                    .strong()
                                    .monospace(),
                            );
                            ui.label(
                                egui::RichText::new(&state.status)
                                    .color(if signal_active {
                                        colors.success
                                    } else {
                                        colors.danger
                                    })
                                    .size(10.5)
                                    .monospace(),
                            );
                        });
                    });
                    ui.add_space(7.0);
                    ui.horizontal_wrapped(|ui| {
                        crate::ui_kit::status_chip(
                            ui,
                            Icon::Hud,
                            "STATE",
                            &format!("{:?}", game.get()),
                            theme,
                        );
                        crate::ui_kit::status_chip(
                            ui,
                            Icon::Follow,
                            "SEQ",
                            &state.sequence.to_string(),
                            theme,
                        );
                        crate::ui_kit::status_chip(
                            ui,
                            Icon::Player,
                            "POS",
                            &format!(
                                "{:.0} / {:.0} / {:.0}",
                                transform.translation.x,
                                transform.translation.y,
                                transform.translation.z
                            ),
                            theme,
                        );
                    });
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "move f {:.1} r {:.1} u {:.1}  look {:.2}/{:.2}  fly {} fire {} scope {}",
                            state.forward,
                            state.right,
                            state.up,
                            state.look_x,
                            state.look_y,
                            player.flying,
                            state.fire,
                            state.scope
                        ))
                        .monospace()
                        .size(11.0)
                        .color(colors.text_muted),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "keys {:?}  mouse {:?}",
                            state.keys, state.mouse_buttons
                        ))
                        .monospace()
                        .size(11.0)
                        .color(colors.text_muted),
                    );
                    ui.add_space(6.0);
                    crate::ui_kit::compact_separator(ui, theme);
                    ui.add_space(6.0);
                    let ocr_color = colors.text;
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
            paint_agent_panel_outline(
                ui,
                panel.response.rect,
                theme,
                egui::Id::new("agent_control_live_outline"),
                signal_active,
                0.34,
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pausing_agent_control_clears_synthetic_input_and_hands_back_to_player() {
        let mut state = AgentControlState {
            runtime_enabled: true,
            enabled: true,
            sequence: 41,
            forward: 1.0,
            right: -0.75,
            up: 0.5,
            sprint: true,
            fly: false,
            look_x: 0.3,
            look_y: -0.2,
            yaw: Some(1.4),
            pitch: Some(-0.4),
            fire: true,
            scope: true,
            keys: vec!["W".into(), "SHIFT".into()],
            mouse_buttons: vec!["LMB".into()],
            game_state: "mainmenu".into(),
            build_mode: "build".into(),
            build_tool: "tower".into(),
            handoff: false,
            screenshot: true,
            exit: true,
            status: "driving".into(),
        };

        assert!(set_agent_control_enabled(&mut state, false));
        assert!(!state.enabled);
        assert_eq!(state.sequence, 42);
        assert!(state.handoff);
        assert_eq!((state.forward, state.right, state.up), (0.0, 0.0, 0.0));
        assert_eq!((state.look_x, state.look_y), (0.0, 0.0));
        assert!(!state.sprint);
        assert!(state.fly);
        assert_eq!((state.yaw, state.pitch), (None, None));
        assert!(!state.fire);
        assert!(!state.scope);
        assert!(state.keys.is_empty());
        assert!(state.mouse_buttons.is_empty());
        assert!(!state.screenshot);
        assert!(!state.exit);
        assert_eq!(state.build_mode, "combat");
        assert!(state.build_tool.is_empty());
        assert_eq!(state.game_state, "ingame");
        assert_eq!(state.status, "agent paused");
    }

    #[test]
    fn resuming_agent_control_is_idempotent_and_saturates_sequence() {
        let mut state = AgentControlState {
            runtime_enabled: true,
            enabled: false,
            sequence: u64::MAX,
            handoff: true,
            build_mode: "combat".into(),
            build_tool: "road".into(),
            game_state: "ingame".into(),
            status: "agent paused".into(),
            ..default()
        };

        assert!(set_agent_control_enabled(&mut state, true));
        assert!(state.enabled);
        assert_eq!(state.sequence, u64::MAX);
        assert!(!state.handoff);
        assert!(state.build_mode.is_empty());
        assert!(state.build_tool.is_empty());
        assert!(state.game_state.is_empty());
        assert_eq!(state.status, "agent live");

        assert!(!set_agent_control_enabled(&mut state, true));
        assert_eq!(state.sequence, u64::MAX);
    }

    #[test]
    fn control_action_exposes_selected_and_disabled_states_without_layout_shift() {
        assert_eq!(
            agent_control_action_spec(true),
            (Icon::Pause, "Agent live", true)
        );
        assert_eq!(
            agent_control_action_spec(false),
            (Icon::Play, "Agent paused", false)
        );

        egui::__run_test_ui(|ui| {
            let theme = crate::theme::ThemeSettings::default();
            let live = agent_control_action(ui, true, true, theme);
            let paused = agent_control_action(ui, false, true, theme);
            let unavailable = agent_control_action(ui, false, false, theme);

            assert!(live.enabled());
            assert!(paused.enabled());
            assert!(!unavailable.enabled());
            assert_eq!(live.rect.size(), paused.rect.size());
            assert_eq!(paused.rect.size(), unavailable.rect.size());
            assert_eq!(live.rect.width(), AGENT_CONTROL_ACTION_WIDTH);
            assert_eq!(live.rect.height(), theme.density.row_height());
        });
    }

    #[test]
    fn panel_width_is_finite_and_never_exceeds_the_viewport_or_preference() {
        assert_eq!(
            adaptive_agent_panel_width(1_920.0, AGENT_CONTROL_PANEL_WIDTH),
            AGENT_CONTROL_PANEL_WIDTH
        );
        assert_eq!(
            adaptive_agent_panel_width(320.0, AGENT_CONTROL_PANEL_WIDTH),
            296.0
        );
        assert_eq!(
            adaptive_agent_panel_width(12.0, AGENT_CONTROL_PANEL_WIDTH),
            1.0
        );
        assert_eq!(
            adaptive_agent_panel_width(f32::NAN, AGENT_OVERLAY_PANEL_WIDTH),
            AGENT_OVERLAY_PANEL_WIDTH
        );
        assert_eq!(adaptive_agent_panel_width(800.0, f32::NAN), 1.0);
    }
}
