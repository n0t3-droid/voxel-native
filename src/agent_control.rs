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

use crate::bot_command::{
    BotCommandStateMachine, CommandId, CommandOperation, CommandRecipients, CommandTarget,
    IssueSeverity,
};
use crate::builder::BuilderState;
use crate::icons::Icon;
use crate::menu::{GameState, PendingWorldLoad};
use crate::mode::{ActiveMode, ModeContext};
use crate::player::Player;
use crate::settings::{
    ActiveWorld, SceneryQuality, TerrainGrammarVersion, TimeMode, WorldGenerationIdentity,
    WorldMeta, WorldProfile, WorldSettings,
};
use crate::sketch_model::{SketchDocument, ToolController};
use crate::toolbelt::{
    apply_toolbox_selection, BuildWorkflowPreset, ToolbeltState, ToolbeltTool, ToolboxSelection,
};
use crate::weapons::ActiveWeapon;
use crate::world::{ChunkStreamer, PendingEditedOverrideStore, StreamingGovernor, VoxelWorld};

#[cfg(not(target_arch = "wasm32"))]
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

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
                    apply_agent_bot_command,
                    agent_control_handoff,
                    apply_agent_build_mode,
                    apply_agent_camera_pose,
                    apply_agent_inputs,
                )
                    .chain()
                    .after(crate::live_link::LiveLinkControlSet)
                    .before(crate::live_link::LiveLinkInputSet)
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
                    agent_control_capture.run_if(agent_control_runtime_enabled),
                    agent_control_status.run_if(agent_control_runtime_enabled),
                    agent_control_exit.run_if(agent_control_runtime_enabled),
                )
                    .chain(),
            );
    }
}

#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentControlState {
    pub runtime_enabled: bool,
    pub enabled: bool,
    #[serde(skip)]
    live_link_suppressed: bool,
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
    pub view_label: String,
    camera_pose: Option<AgentCameraPoseRequest>,
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
    bot_command_request: Option<AgentBotCommandRequest>,
    latest_bot_command_id: Option<u64>,
    bot_status: String,
    pub status: String,
}

impl Default for AgentControlState {
    fn default() -> Self {
        Self {
            runtime_enabled: false,
            enabled: false,
            live_link_suppressed: false,
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
            view_label: String::new(),
            camera_pose: None,
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
            bot_command_request: None,
            latest_bot_command_id: None,
            bot_status: "bot control idle".into(),
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
        self.runtime_enabled && self.enabled && !self.live_link_suppressed
    }

    pub(crate) fn live_link_suppressed(&self) -> bool {
        self.live_link_suppressed
    }

    pub(crate) fn set_live_link_suppressed(&mut self, suppressed: bool) -> bool {
        if self.live_link_suppressed == suppressed {
            return false;
        }
        self.live_link_suppressed = suppressed;
        true
    }
}

#[derive(Resource, Debug)]
struct AgentControlRuntime {
    runtime_enabled: bool,
    auto_enter: bool,
    poll_timer: f32,
    bridge_timer: f32,
    status_timer: f32,
    screenshot_timer: f32,
    screenshot_interval: f32,
    last_sequence_for_screenshot: u64,
    pending_sequence_screenshot_frames: Option<u8>,
    last_sequence_for_exit: u64,
    last_handoff_sequence: u64,
    last_build_sequence: Option<u64>,
    last_camera_command_sequence: Option<u64>,
    last_camera_command_pose: Option<AgentCameraPoseRequest>,
    last_camera_pose_sequence: Option<u64>,
    screenshot_index: usize,
    frames: u64,
    total_dt: f32,
    last_frame_ms: f32,
    max_frame_ms: f32,
    stall_count: u64,
    stall_threshold_ms: f32,
    last_command_seconds: f32,
    last_error: Option<String>,
    last_observed_control_text: Option<String>,
    last_accepted_control_text: Option<String>,
    last_control_sequence: Option<u64>,
    last_applied_bot_request: Option<AgentBotCommandRequest>,
    in_game_frames: u32,
    mission_feed_enabled: bool,
    mission_agent_id: String,
    mission_agent_name: String,
    mission_agent_role: String,
    mission_agent_task: String,
    mission_fleet_id: String,
    mission_live_link_side: Option<String>,
    mission_live_link_bind: Option<String>,
    mission_live_link_peer: Option<String>,
    synthetic_keys: Vec<KeyCode>,
    synthetic_mouse_buttons: Vec<MouseButton>,
    #[cfg(not(target_arch = "wasm32"))]
    control_path: PathBuf,
    #[cfg(not(target_arch = "wasm32"))]
    session_dir: PathBuf,
    #[cfg(not(target_arch = "wasm32"))]
    capture_ledger: Arc<Mutex<AgentCaptureLedger>>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
struct AgentCaptureLedger {
    successful_count: usize,
    highest_success: Option<(usize, PathBuf)>,
    latest_outcome_index: Option<usize>,
    latest_error: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentCaptureSnapshot {
    successful_count: usize,
    last_screenshot: Option<PathBuf>,
    latest_error: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
impl AgentCaptureLedger {
    fn record_success(&mut self, index: usize, path: PathBuf) {
        self.successful_count = self.successful_count.saturating_add(1);
        if self
            .highest_success
            .as_ref()
            .is_none_or(|(highest, _)| index > *highest)
        {
            self.highest_success = Some((index, path));
        }
        if self
            .latest_outcome_index
            .is_none_or(|latest| index >= latest)
        {
            self.latest_outcome_index = Some(index);
            self.latest_error = None;
        }
    }

    fn record_failure(&mut self, index: usize, error: impl std::fmt::Display) {
        if self
            .latest_outcome_index
            .is_none_or(|latest| index >= latest)
        {
            self.latest_outcome_index = Some(index);
            self.latest_error = Some(bounded_agent_screenshot_error(index, error));
        }
    }

    fn snapshot(&self) -> AgentCaptureSnapshot {
        AgentCaptureSnapshot {
            successful_count: self.successful_count,
            last_screenshot: self.highest_success.as_ref().map(|(_, path)| path.clone()),
            latest_error: self.latest_error.clone(),
        }
    }
}

impl AgentControlRuntime {
    fn from_env() -> Self {
        let runtime_enabled = agent_runtime_enabled();
        let mission_feed_enabled = env_flag("VOXEL_NATIVE_MISSION_FEED")
            || std::env::var("VOXEL_NATIVE_LIVE_LINK_SIDE")
                .map(|side| side.trim().eq_ignore_ascii_case("codex"))
                .unwrap_or(false);
        let fallback_agent_id = format!("agent-{}", crate::platform::now_epoch());
        let mission_agent_id =
            bounded_env_text("VOXEL_NATIVE_AGENT_ID", &fallback_agent_id, 64, true);
        let mission_agent_name = bounded_env_text(
            "VOXEL_NATIVE_AGENT_NAME",
            &mission_agent_id.to_ascii_uppercase(),
            48,
            false,
        );
        let mission_agent_role =
            bounded_env_text("VOXEL_NATIVE_AGENT_ROLE", "ENGINE EXPLORER", 36, false);
        let mission_agent_task = bounded_env_text(
            "VOXEL_NATIVE_AGENT_TASK",
            "Autonomous world inspection",
            180,
            false,
        );
        let mission_fleet_id =
            bounded_env_text("VOXEL_NATIVE_AGENT_FLEET_ID", "default-fleet", 64, true);
        let mission_live_link_side = std::env::var("VOXEL_NATIVE_LIVE_LINK_SIDE").ok();
        let mission_live_link_bind = std::env::var("VOXEL_NATIVE_LIVE_LINK_BIND").ok();
        let mission_live_link_peer = std::env::var("VOXEL_NATIVE_LIVE_LINK_PEER").ok();
        #[cfg(not(target_arch = "wasm32"))]
        let control_path = std::env::var("VOXEL_NATIVE_AGENT_CONTROL_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("agent_control.ron"));
        #[cfg(not(target_arch = "wasm32"))]
        let session_dir = std::env::var("VOXEL_NATIVE_AGENT_SESSION_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from("agent_runs").join(format!("live_{}", crate::platform::now_epoch()))
            });

        Self {
            runtime_enabled,
            auto_enter: !env_flag("VOXEL_NATIVE_AGENT_NO_AUTO_ENTER")
                && !std::env::args().any(|arg| arg == "--agent-no-auto-enter"),
            poll_timer: 0.0,
            bridge_timer: AGENT_BRIDGE_HEARTBEAT_SECONDS,
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
                .unwrap_or(if mission_feed_enabled { 1.5 } else { 0.0 }),
            last_sequence_for_screenshot: 0,
            pending_sequence_screenshot_frames: None,
            last_sequence_for_exit: 0,
            last_handoff_sequence: 0,
            last_build_sequence: None,
            last_camera_command_sequence: None,
            last_camera_command_pose: None,
            last_camera_pose_sequence: None,
            screenshot_index: 0,
            frames: 0,
            total_dt: 0.0,
            last_frame_ms: 0.0,
            max_frame_ms: 0.0,
            stall_count: 0,
            stall_threshold_ms: env_f32("VOXEL_NATIVE_AGENT_STALL_MS").unwrap_or(100.0),
            last_command_seconds: 0.0,
            last_error: None,
            last_observed_control_text: None,
            last_accepted_control_text: None,
            last_control_sequence: None,
            last_applied_bot_request: None,
            in_game_frames: 0,
            mission_feed_enabled,
            mission_agent_id,
            mission_agent_name,
            mission_agent_role,
            mission_agent_task,
            mission_fleet_id,
            mission_live_link_side,
            mission_live_link_bind,
            mission_live_link_peer,
            synthetic_keys: Vec::new(),
            synthetic_mouse_buttons: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            control_path,
            #[cfg(not(target_arch = "wasm32"))]
            session_dir,
            #[cfg(not(target_arch = "wasm32"))]
            capture_ledger: Arc::new(Mutex::new(AgentCaptureLedger::default())),
        }
    }
}

const AGENT_CAMERA_SAFE_COORDINATE_LIMIT: f32 = 16_777_216.0;
const AGENT_CAMERA_SAFE_Y_LIMIT: f32 = 1_048_576.0;
const AGENT_CAMERA_PITCH_LIMIT: f32 = 1.54;
const AGENT_VIEW_LABEL_MAX_CHARS: usize = 96;
const AGENT_CAMERA_POSE_ERROR_PREFIX: &str = "camera pose: ";
const AGENT_CONTROL_MAX_BYTES: usize = 64 * 1024;
const AGENT_CONTROL_MAX_INPUT_NAMES: usize = 64;
const AGENT_CONTROL_MAX_INPUT_NAME_CHARS: usize = 32;
const AGENT_CONTROL_MAX_COMMAND_CHARS: usize = 64;
const AGENT_CONTROL_MAX_BOT_POINTS: usize = 4_096;
const AGENT_CONTROL_MAX_BOT_RECIPIENTS: usize = 4_096;
const AGENT_BRIDGE_HEARTBEAT_SECONDS: f32 = 1.0;
pub(crate) const LIVE_OBSERVER_PROTOCOL_VERSION: &str = "live-observer-v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct AgentCameraPoseRequest {
    position: [f32; 3],
    yaw: f32,
    pitch: f32,
}

impl Default for AgentCameraPoseRequest {
    fn default() -> Self {
        Self {
            position: [0.0, 140.0, 0.0],
            yaw: 0.0,
            pitch: -0.2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ValidatedAgentCameraPose {
    position: Vec3,
    yaw: f32,
    pitch: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentCameraCommandObservation {
    Fresh,
    Repeat,
    Stale,
    ReusedWithDifferentPose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentControlPayloadDecision {
    Apply,
    Identical,
    Stale,
    ReusedWithDifferentPayload,
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
    view_label: String,
    camera_pose: Option<AgentCameraPoseRequest>,
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
    bot_command: Option<AgentBotCommandRequest>,
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
            view_label: String::new(),
            camera_pose: None,
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
            bot_command: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct AgentBotCommandRequest {
    request_id: u64,
    action: String,
    command_id: Option<u64>,
    operation: String,
    target: AgentBotTargetRequest,
    recipients: AgentBotRecipientsRequest,
    block_reason: String,
}

impl Default for AgentBotCommandRequest {
    fn default() -> Self {
        Self {
            request_id: 0,
            action: "CREATE_PREVIEW".into(),
            command_id: None,
            operation: "INSPECT".into(),
            target: AgentBotTargetRequest::default(),
            recipients: AgentBotRecipientsRequest::default(),
            block_reason: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum AgentBotTargetRequest {
    Point([i32; 3]),
    Area { min: [i32; 3], max: [i32; 3] },
    Path(Vec<[i32; 3]>),
    Selection(Vec<[i32; 3]>),
}

impl Default for AgentBotTargetRequest {
    fn default() -> Self {
        Self::Point([0; 3])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum AgentBotRecipientsRequest {
    All,
    Selected(Vec<u64>),
    Group(u64),
}

impl Default for AgentBotRecipientsRequest {
    fn default() -> Self {
        Self::All
    }
}

#[derive(Debug, Serialize)]
struct AgentLiveStatus {
    seconds: f32,
    game_state: String,
    command_sequence: u64,
    command_status: String,
    command_view_label: String,
    command_camera_pose_requested: bool,
    camera_command_sequence_cursor: Option<u64>,
    camera_pose_handled_sequence: Option<u64>,
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
    bot_last_request_id: Option<u64>,
    bot_latest_command_id: Option<u64>,
    bot_operation: String,
    bot_state: String,
    bot_revision: Option<u64>,
    bot_estimated_voxel_cost: Option<u64>,
    bot_estimated_chunk_cost: Option<u64>,
    bot_applied_voxel_edits: Option<u64>,
    bot_touched_chunks: Option<u64>,
    bot_spawned_projects: Option<u32>,
    bot_warning_count: usize,
    bot_error_count: usize,
    bot_block_reason: Option<String>,
    bot_status: String,
    weapon: String,
    toolbelt_live: bool,
    toolbelt_palette_open: bool,
    toolbelt_tool: String,
    toolbelt_status: String,
    editor_tool: String,
    editor_phase: String,
    editor_selection_count: usize,
    editor_edit_object: bool,
    position: [f32; 3],
    environment_biome: String,
    environment_surface_y: i32,
    environment_temperature_norm: f32,
    environment_atmospheric_moisture: f32,
    environment_soil_moisture: f32,
    environment_river_strength: f32,
    environment_mineral_resonance: f32,
    environment_flowering_resonance: f32,
    environment_flow_direction: [f32; 2],
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
    last_screenshot_error: Option<String>,
    screenshot_count: usize,
    in_game_frames: u32,
    last_screenshot: Option<String>,
    session_dir: String,
}

#[derive(Debug, Serialize)]
struct AgentBridgeStatus<'a> {
    stage: &'a str,
    observer_protocol_version: &'static str,
    isolated_observer: bool,
    runtime_enabled: bool,
    enabled: bool,
    sequence: u64,
    status: &'a str,
    view_label: &'a str,
    last_error: Option<&'a str>,
    control_path: String,
    session_dir: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObserverBridgeContract {
    protocol_version: &'static str,
    isolated_observer: bool,
}

fn observer_bridge_contract(
    runtime_enabled: bool,
    isolated_requested: bool,
) -> ObserverBridgeContract {
    ObserverBridgeContract {
        protocol_version: LIVE_OBSERVER_PROTOCOL_VERSION,
        isolated_observer: isolated_observer_from_flags(runtime_enabled, isolated_requested),
    }
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
        if let Err(e) = prepare_safe_agent_directory(&runtime.session_dir) {
            warn!(
                "agent control: could not create {}: {e}",
                runtime.session_dir.display()
            );
            return;
        }
        let observer_contract = observer_bridge_contract(
            state.runtime_enabled,
            env_flag("VOXEL_NATIVE_AGENT_ISOLATED"),
        );
        let status = AgentBridgeStatus {
            stage,
            observer_protocol_version: observer_contract.protocol_version,
            isolated_observer: observer_contract.isolated_observer,
            runtime_enabled: state.runtime_enabled,
            enabled: state.enabled,
            sequence: state.sequence,
            status: &state.status,
            view_label: &state.view_label,
            last_error: runtime.last_error.as_deref(),
            control_path: runtime_control_path(runtime),
            session_dir: runtime_session_dir(runtime),
        };
        if let Ok(text) = ron::ser::to_string_pretty(&status, ron::ser::PrettyConfig::default()) {
            let path = runtime.session_dir.join("bridge.ron");
            if let Err(error) = atomic_replace_agent_telemetry_text(&path, &text) {
                warn!(
                    "agent control: could not atomically write {}: {error}",
                    path.display()
                );
            }
        }
        if stage == "startup" {
            if let Err(error) = write_agent_capability_manifest_safely(
                &runtime.session_dir,
                &runtime.mission_agent_id,
                &runtime.mission_fleet_id,
            ) {
                warn!("agent control: capability manifest write failed: {error}");
            }
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
        let text = match read_bounded_control_file(&runtime.control_path) {
            Ok(text) => text,
            Err(error) => {
                runtime.last_error = Some(format!(
                    "control file: cannot read {}: {error}",
                    runtime.control_path.display()
                ));
                return;
            }
        };
        if runtime.last_observed_control_text.as_deref() == Some(text.as_str()) {
            if repeated_payload_matches_accepted_identity(
                &text,
                runtime.last_accepted_control_text.as_deref(),
            ) {
                clear_owned_control_boundary_error(&mut runtime.last_error);
            }
            return;
        }
        runtime.last_observed_control_text = Some(text.clone());

        let mut command = match ron::from_str::<AgentControlCommand>(&text) {
            Ok(command) => command,
            Err(error) => {
                runtime.last_error = Some(format!("control parse error: {error}"));
                return;
            }
        };
        match agent_control_payload_decision(
            command.sequence,
            &text,
            runtime.last_control_sequence,
            runtime.last_accepted_control_text.as_deref(),
        ) {
            AgentControlPayloadDecision::Identical => {
                clear_owned_control_boundary_error(&mut runtime.last_error);
                return;
            }
            AgentControlPayloadDecision::Stale => {
                runtime.last_error = Some(format!(
                    "control command: stale sequence {}; cursor is {}",
                    command.sequence,
                    runtime
                        .last_control_sequence
                        .map(|sequence| sequence.to_string())
                        .unwrap_or_else(|| "none".into())
                ));
                return;
            }
            AgentControlPayloadDecision::ReusedWithDifferentPayload => {
                runtime.last_error = Some(format!(
                    "control command: sequence {} was reused with a different payload",
                    command.sequence
                ));
                return;
            }
            AgentControlPayloadDecision::Apply => {}
        }
        if let Err(error) = sanitize_agent_control_command(&mut command) {
            runtime.last_error = Some(format!("control command: {error}"));
            return;
        }

        runtime.last_control_sequence = Some(command.sequence);
        runtime.last_accepted_control_text = Some(text);
        runtime.last_command_seconds = time.elapsed_seconds();
        clear_owned_control_boundary_error(&mut runtime.last_error);
        apply_command(&mut state, command);
    }
}

fn repeated_payload_matches_accepted_identity(
    payload: &str,
    accepted_payload: Option<&str>,
) -> bool {
    accepted_payload == Some(payload)
}

fn agent_control_payload_decision(
    sequence: u64,
    payload: &str,
    last_sequence: Option<u64>,
    last_payload: Option<&str>,
) -> AgentControlPayloadDecision {
    match last_sequence {
        None => AgentControlPayloadDecision::Apply,
        Some(last) if sequence > last => AgentControlPayloadDecision::Apply,
        Some(last) if sequence < last => AgentControlPayloadDecision::Stale,
        Some(_) if last_payload == Some(payload) => AgentControlPayloadDecision::Identical,
        Some(_) => AgentControlPayloadDecision::ReusedWithDifferentPayload,
    }
}

fn clear_owned_control_boundary_error(last_error: &mut Option<String>) {
    if last_error.as_deref().is_some_and(|error| {
        error.starts_with("control file:")
            || error.starts_with("control parse error:")
            || error.starts_with("control command:")
    }) {
        *last_error = None;
    }
}

fn apply_command(state: &mut AgentControlState, command: AgentControlCommand) {
    state.enabled = command.enabled;
    state.sequence = command.sequence;
    state.forward = finite_clamped_or_zero(command.forward, -1.0, 1.0);
    state.right = finite_clamped_or_zero(command.right, -1.0, 1.0);
    state.up = finite_clamped_or_zero(command.up, -1.0, 1.0);
    state.sprint = command.sprint;
    state.fly = command.fly;
    state.look_x = finite_clamped_or_zero(command.look_x, -4.0, 4.0);
    state.look_y = finite_clamped_or_zero(command.look_y, -4.0, 4.0);
    state.yaw = finite_optional(command.yaw);
    state.pitch = finite_optional(command.pitch).map(|pitch| pitch.clamp(-1.54, 1.54));
    state.view_label = sanitize_view_label(&command.view_label);
    state.camera_pose = command.camera_pose;
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
    state.bot_command_request = command.bot_command;
    state.status = if state.enabled {
        "agent live".into()
    } else {
        "agent paused".into()
    };
}

fn finite_clamped_or_zero(value: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        0.0
    }
}

fn finite_optional(value: Option<f32>) -> Option<f32> {
    value.filter(|value| value.is_finite())
}

fn sanitize_view_label(label: &str) -> String {
    sanitize_bounded_control_text(label, AGENT_VIEW_LABEL_MAX_CHARS)
}

fn sanitize_bounded_control_text(text: &str, max_chars: usize) -> String {
    let mut sanitized = String::with_capacity(text.len().min(max_chars));
    let mut pending_space = false;
    let mut character_count = 0;

    for character in text.trim().chars() {
        if character.is_control() || character.is_whitespace() {
            pending_space = !sanitized.is_empty();
            continue;
        }
        if pending_space {
            if character_count >= max_chars {
                break;
            }
            sanitized.push(' ');
            character_count += 1;
            pending_space = false;
        }
        if character_count >= max_chars {
            break;
        }
        sanitized.push(character);
        character_count += 1;
    }

    sanitized
}

fn sanitize_agent_control_command(command: &mut AgentControlCommand) -> Result<(), String> {
    command.keys =
        bounded_unique_input_names(std::mem::take(&mut command.keys), AgentInputNameKind::Key);
    command.mouse_buttons = bounded_unique_input_names(
        std::mem::take(&mut command.mouse_buttons),
        AgentInputNameKind::MouseButton,
    );
    command.game_state =
        sanitize_bounded_control_text(&command.game_state, AGENT_CONTROL_MAX_COMMAND_CHARS);
    command.build_mode =
        sanitize_bounded_control_text(&command.build_mode, AGENT_CONTROL_MAX_COMMAND_CHARS);
    command.build_tool =
        sanitize_bounded_control_text(&command.build_tool, AGENT_CONTROL_MAX_COMMAND_CHARS);
    command.view_label = sanitize_view_label(&command.view_label);

    let Some(bot) = command.bot_command.as_mut() else {
        return Ok(());
    };
    bot.action = sanitize_bounded_control_text(&bot.action, AGENT_CONTROL_MAX_COMMAND_CHARS);
    bot.operation = sanitize_bounded_control_text(&bot.operation, AGENT_CONTROL_MAX_COMMAND_CHARS);
    bot.block_reason =
        sanitize_bounded_control_text(&bot.block_reason, AGENT_CONTROL_MAX_COMMAND_CHARS);
    match &mut bot.target {
        AgentBotTargetRequest::Path(points) => {
            deduplicate_bounded_values(points, AGENT_CONTROL_MAX_BOT_POINTS, "bot path")?;
        }
        AgentBotTargetRequest::Selection(points) => {
            deduplicate_bounded_values(points, AGENT_CONTROL_MAX_BOT_POINTS, "bot selection")?;
        }
        AgentBotTargetRequest::Point(_) | AgentBotTargetRequest::Area { .. } => {}
    }
    if let AgentBotRecipientsRequest::Selected(recipients) = &mut bot.recipients {
        deduplicate_bounded_values(
            recipients,
            AGENT_CONTROL_MAX_BOT_RECIPIENTS,
            "bot recipients",
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum AgentInputNameKind {
    Key,
    MouseButton,
}

fn bounded_unique_input_names(values: Vec<String>, kind: AgentInputNameKind) -> Vec<String> {
    let mut identities = std::collections::BTreeSet::new();
    let mut bounded = Vec::with_capacity(values.len().min(AGENT_CONTROL_MAX_INPUT_NAMES));
    for value in values {
        if bounded.len() >= AGENT_CONTROL_MAX_INPUT_NAMES {
            break;
        }
        let value = sanitize_bounded_control_text(&value, AGENT_CONTROL_MAX_INPUT_NAME_CHARS);
        let normalized = normalized_input_name(&value);
        let identity = match kind {
            AgentInputNameKind::Key => parse_key_code(&value)
                .map(|key| format!("KEY:{key:?}"))
                .unwrap_or(normalized),
            AgentInputNameKind::MouseButton => parse_mouse_button(&value)
                .map(|button| format!("MOUSE:{button:?}"))
                .unwrap_or(normalized),
        };
        if !identity.is_empty() && identities.insert(identity) {
            bounded.push(value);
        }
    }
    bounded
}

fn deduplicate_bounded_values<T: Copy + Ord>(
    values: &mut Vec<T>,
    max_values: usize,
    label: &str,
) -> Result<(), String> {
    if values.len() > max_values {
        return Err(format!(
            "{label} has {} entries; limit is {max_values}",
            values.len()
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    values.retain(|value| seen.insert(*value));
    Ok(())
}

fn validate_agent_camera_pose(
    request: AgentCameraPoseRequest,
) -> Result<ValidatedAgentCameraPose, &'static str> {
    let position = Vec3::new(
        request.position[0],
        request.position[1],
        request.position[2],
    );
    if !position.is_finite() || !request.yaw.is_finite() || !request.pitch.is_finite() {
        return Err("position, yaw, and pitch must all be finite");
    }
    if position.x.abs() > AGENT_CAMERA_SAFE_COORDINATE_LIMIT
        || position.z.abs() > AGENT_CAMERA_SAFE_COORDINATE_LIMIT
    {
        return Err("horizontal position exceeds the f32 exact-integer range");
    }
    if position.y.abs() > AGENT_CAMERA_SAFE_Y_LIMIT {
        return Err("vertical position exceeds the observer safety bound");
    }
    if request.pitch.abs() > AGENT_CAMERA_PITCH_LIMIT {
        return Err("pitch exceeds the observer look bound");
    }

    let yaw = (request.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
        - std::f32::consts::PI;
    Ok(ValidatedAgentCameraPose {
        position,
        yaw,
        pitch: request.pitch,
    })
}

fn agent_camera_pose_identity_matches(
    left: Option<AgentCameraPoseRequest>,
    right: Option<AgentCameraPoseRequest>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.position
                .into_iter()
                .zip(right.position)
                .all(|(left, right)| left.to_bits() == right.to_bits())
                && left.yaw.to_bits() == right.yaw.to_bits()
                && left.pitch.to_bits() == right.pitch.to_bits()
        }
        _ => false,
    }
}

fn observe_agent_camera_command(
    sequence: u64,
    pose: Option<AgentCameraPoseRequest>,
    last_sequence: &mut Option<u64>,
    last_pose: &mut Option<AgentCameraPoseRequest>,
) -> AgentCameraCommandObservation {
    match *last_sequence {
        None => {
            *last_sequence = Some(sequence);
            *last_pose = pose;
            AgentCameraCommandObservation::Fresh
        }
        Some(last) if sequence > last => {
            *last_sequence = Some(sequence);
            *last_pose = pose;
            AgentCameraCommandObservation::Fresh
        }
        Some(last) if sequence < last => AgentCameraCommandObservation::Stale,
        Some(_) if agent_camera_pose_identity_matches(pose, *last_pose) => {
            AgentCameraCommandObservation::Repeat
        }
        Some(_) => AgentCameraCommandObservation::ReusedWithDifferentPose,
    }
}

fn agent_camera_pose_pending(
    state: &AgentControlState,
    game: &GameState,
    last_handled_sequence: Option<u64>,
) -> bool {
    state.active()
        && *game == GameState::InGame
        && state.camera_pose.is_some()
        && last_handled_sequence.is_none_or(|last| state.sequence > last)
}

fn apply_validated_agent_camera_pose(
    pose: ValidatedAgentCameraPose,
    transform: &mut Transform,
    player: &mut Player,
) {
    transform.translation = pose.position;
    transform.rotation =
        Quat::from_axis_angle(Vec3::Y, pose.yaw) * Quat::from_axis_angle(Vec3::X, pose.pitch);
    player.yaw = pose.yaw;
    player.pitch = pose.pitch;
    player.velocity = Vec3::ZERO;
    player.on_ground = false;
    player.flying = true;
    player.placed_on_surface = true;
}

fn clear_owned_camera_pose_error(last_error: &mut Option<String>) {
    if last_error
        .as_deref()
        .is_some_and(|error| error.starts_with(AGENT_CAMERA_POSE_ERROR_PREFIX))
    {
        *last_error = None;
    }
}

fn apply_agent_camera_pose(
    game: Res<State<GameState>>,
    state: Res<AgentControlState>,
    mut runtime: ResMut<AgentControlRuntime>,
    mut player: Query<(&mut Transform, &mut Player)>,
) {
    let observation = {
        let runtime = &mut *runtime;
        observe_agent_camera_command(
            state.sequence,
            state.camera_pose,
            &mut runtime.last_camera_command_sequence,
            &mut runtime.last_camera_command_pose,
        )
    };
    match observation {
        AgentCameraCommandObservation::Fresh => {
            clear_owned_camera_pose_error(&mut runtime.last_error);
        }
        AgentCameraCommandObservation::Repeat => {}
        AgentCameraCommandObservation::Stale => {
            runtime.last_error = Some(format!(
                "{AGENT_CAMERA_POSE_ERROR_PREFIX}stale command sequence {}; cursor is {}",
                state.sequence,
                runtime
                    .last_camera_command_sequence
                    .map(|sequence| sequence.to_string())
                    .unwrap_or_else(|| "none".into())
            ));
            return;
        }
        AgentCameraCommandObservation::ReusedWithDifferentPose => {
            if runtime
                .last_camera_pose_sequence
                .is_none_or(|last| state.sequence > last)
            {
                runtime.last_camera_pose_sequence = Some(state.sequence);
            }
            runtime.last_error = Some(format!(
                "{AGENT_CAMERA_POSE_ERROR_PREFIX}sequence {} was reused with a different camera_pose",
                state.sequence
            ));
            return;
        }
    }

    let Some(request) = state.camera_pose else {
        clear_owned_camera_pose_error(&mut runtime.last_error);
        return;
    };
    if !agent_camera_pose_pending(&state, game.get(), runtime.last_camera_pose_sequence) {
        return;
    }
    let Ok((mut transform, mut player)) = player.get_single_mut() else {
        return;
    };

    // Whether valid or rejected, this sequence is consumed exactly once. A
    // corrected request must use a strictly newer sequence so observers cannot
    // oscillate between payloads that claim the same command identity or
    // replay a stale teleport after a later view was shown.
    runtime.last_camera_pose_sequence = Some(state.sequence);
    match validate_agent_camera_pose(request) {
        Ok(pose) => {
            apply_validated_agent_camera_pose(pose, &mut transform, &mut player);
            clear_owned_camera_pose_error(&mut runtime.last_error);
        }
        Err(error) => {
            runtime.last_error = Some(format!("{AGENT_CAMERA_POSE_ERROR_PREFIX}{error}"));
        }
    }
}

fn apply_agent_bot_command(
    mut state: ResMut<AgentControlState>,
    mut runtime: ResMut<AgentControlRuntime>,
    mut commands: ResMut<BotCommandStateMachine>,
) {
    let Some(request) = state.bot_command_request.clone() else {
        return;
    };

    match process_agent_bot_request(
        &request,
        &mut commands,
        &mut runtime.last_applied_bot_request,
        state.active(),
    ) {
        Ok(AgentBotRequestOutcome::Applied(id)) => {
            state.latest_bot_command_id = Some(id.get());
            state.bot_status = bot_command_status(&request.action, &commands, id);
            if runtime
                .last_error
                .as_deref()
                .is_some_and(|error| error.starts_with("bot request"))
            {
                runtime.last_error = None;
            }
        }
        Ok(AgentBotRequestOutcome::Duplicate) => {}
        Ok(AgentBotRequestOutcome::DeferredWhilePaused) => {
            state.bot_status = format!(
                "bot request {} deferred while agent control is paused",
                request.request_id
            );
        }
        Err(error) => {
            let error = format!("bot request {}: {error}", request.request_id);
            state.bot_status = error.clone();
            runtime.last_error = Some(error);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentBotRequestOutcome {
    Applied(CommandId),
    Duplicate,
    DeferredWhilePaused,
}

fn process_agent_bot_request(
    request: &AgentBotCommandRequest,
    commands: &mut BotCommandStateMachine,
    last_applied: &mut Option<AgentBotCommandRequest>,
    agent_active: bool,
) -> Result<AgentBotRequestOutcome, String> {
    if request.request_id == 0 {
        return Err("request_id must be greater than zero".into());
    }

    let action = normalize_bot_token(&request.action);
    if let Some(applied) = last_applied.as_ref() {
        use std::cmp::Ordering;

        match request.request_id.cmp(&applied.request_id) {
            Ordering::Less => {
                return Err(format!(
                    "stale request_id {}; last applied request_id is {}",
                    request.request_id, applied.request_id
                ));
            }
            Ordering::Equal if request == applied => {
                return Ok(AgentBotRequestOutcome::Duplicate);
            }
            Ordering::Equal => {
                return Err(format!(
                    "request_id {} was reused with a different payload",
                    request.request_id
                ));
            }
            Ordering::Greater => {}
        }
    }

    if !agent_active && !bot_action_allowed_while_paused(&action) {
        return Ok(AgentBotRequestOutcome::DeferredWhilePaused);
    }

    let id = if matches!(action.as_str(), "CREATE" | "CREATEPREVIEW") {
        let operation = parse_bot_operation(&request.operation)?;
        let target = bot_target(&request.target);
        let recipients = bot_recipients(&request.recipients);
        let id = commands
            .create(operation, target, recipients)
            .map_err(|error| error.to_string())?;
        commands
            .prepare_preview(id)
            .map_err(|error| error.to_string())?;
        id
    } else {
        let id = request
            .command_id
            .and_then(CommandId::from_raw)
            .ok_or_else(|| format!("action {} requires a non-zero command_id", request.action))?;

        match action.as_str() {
            "APPROVE" => {
                commands.approve(id).map_err(|error| error.to_string())?;
            }
            "EXECUTE" => commands
                .request_execution(id)
                .map_err(|error| error.to_string())?,
            "PAUSE" => commands.pause(id).map_err(|error| error.to_string())?,
            "RESUME" => commands.resume(id).map_err(|error| error.to_string())?,
            "COMPLETE" => {
                return Err("COMPLETE is executor-owned and requires execution proof".into());
            }
            "CANCEL" => commands.cancel(id).map_err(|error| error.to_string())?,
            "BLOCK" => commands
                .block(id, request.block_reason.clone())
                .map_err(|error| error.to_string())?,
            "UNBLOCK" => commands.unblock(id).map_err(|error| error.to_string())?,
            _ => return Err(format!("unknown bot action {:?}", request.action)),
        }
        id
    };

    *last_applied = Some(request.clone());
    Ok(AgentBotRequestOutcome::Applied(id))
}

fn bot_action_allowed_while_paused(action: &str) -> bool {
    matches!(action, "PAUSE" | "BLOCK" | "CANCEL")
}

fn normalize_bot_token(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_uppercase())
        .collect()
}

fn parse_bot_operation(value: &str) -> Result<CommandOperation, String> {
    match normalize_bot_token(value).as_str() {
        "INSPECT" => Ok(CommandOperation::Inspect),
        "PENCIL" | "LINE" => Ok(CommandOperation::Pencil),
        "RECTANGLE" | "RECT" => Ok(CommandOperation::Rectangle),
        "PUSHPULL" | "PUSH" => Ok(CommandOperation::PushPull),
        "ROOM" | "HOLLOW" => Ok(CommandOperation::Room),
        "CUTOPENING" | "OPENING" | "WINDOW" => Ok(CommandOperation::CutOpening),
        "MOVE" => Ok(CommandOperation::Move),
        "ROTATE" => Ok(CommandOperation::Rotate),
        "SCALE" => Ok(CommandOperation::Scale),
        "PAINT" | "MATERIAL" => Ok(CommandOperation::Paint),
        "ROAD" => Ok(CommandOperation::Road),
        "CLEARFLATTEN" | "FLATTEN" => Ok(CommandOperation::ClearFlatten),
        "REPAIR" => Ok(CommandOperation::Repair),
        _ => Err(format!("unknown bot operation {value:?}")),
    }
}

fn bot_target(target: &AgentBotTargetRequest) -> CommandTarget {
    match target {
        AgentBotTargetRequest::Point(point) => CommandTarget::Point(IVec3::from_array(*point)),
        AgentBotTargetRequest::Area { min, max } => CommandTarget::Area {
            min: IVec3::from_array(*min),
            max: IVec3::from_array(*max),
        },
        AgentBotTargetRequest::Path(points) => {
            CommandTarget::Path(points.iter().copied().map(IVec3::from_array).collect())
        }
        AgentBotTargetRequest::Selection(points) => {
            CommandTarget::Selection(points.iter().copied().map(IVec3::from_array).collect())
        }
    }
}

fn bot_recipients(recipients: &AgentBotRecipientsRequest) -> CommandRecipients {
    match recipients {
        AgentBotRecipientsRequest::All => CommandRecipients::All,
        AgentBotRecipientsRequest::Selected(bots) => CommandRecipients::Selected(bots.clone()),
        AgentBotRecipientsRequest::Group(group) => CommandRecipients::Group(*group),
    }
}

fn bot_command_status(action: &str, commands: &BotCommandStateMachine, id: CommandId) -> String {
    let Ok(command) = commands.command(id) else {
        return format!("bot command {id} unavailable");
    };
    if normalize_bot_token(action) == "EXECUTE" {
        return format!("bot command {id} authorized for exact world execution");
    }
    format!(
        "bot command {id} {:?} at revision {}",
        command.state(),
        command.revision().get()
    )
}

#[derive(Debug, Default)]
struct BotCommandTelemetry {
    operation: String,
    state: String,
    revision: Option<u64>,
    estimated_voxel_cost: Option<u64>,
    estimated_chunk_cost: Option<u64>,
    applied_voxel_edits: Option<u64>,
    touched_chunks: Option<u64>,
    spawned_projects: Option<u32>,
    warning_count: usize,
    error_count: usize,
    block_reason: Option<String>,
}

fn bot_command_telemetry(
    commands: &BotCommandStateMachine,
    latest_id: Option<u64>,
) -> BotCommandTelemetry {
    let Some(id) = latest_id.and_then(CommandId::from_raw) else {
        return BotCommandTelemetry {
            operation: "NONE".into(),
            state: "IDLE".into(),
            ..default()
        };
    };
    let Ok(command) = commands.command(id) else {
        return BotCommandTelemetry {
            operation: "UNKNOWN".into(),
            state: "MISSING".into(),
            ..default()
        };
    };

    let mut telemetry = BotCommandTelemetry {
        operation: format!("{:?}", command.operation()).to_ascii_uppercase(),
        state: format!("{:?}", command.state()).to_ascii_uppercase(),
        revision: Some(command.revision().get()),
        block_reason: command.block_reason().map(str::to_owned),
        ..default()
    };
    if let Some(preview) = command.preview() {
        telemetry.estimated_voxel_cost = Some(preview.estimated_voxel_cost);
        telemetry.estimated_chunk_cost = Some(preview.estimated_chunk_cost);
        for issue in &preview.issues {
            match issue.severity() {
                IssueSeverity::Warning => telemetry.warning_count += 1,
                IssueSeverity::Error => telemetry.error_count += 1,
            }
        }
    }
    if let Some(completion) = command.completion() {
        telemetry.applied_voxel_edits = Some(completion.applied_voxel_edits);
        telemetry.touched_chunks = Some(completion.touched_chunks);
        telemetry.spawned_projects = Some(completion.spawned_projects);
    }
    telemetry
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
    if !state.active() || !runtime.auto_enter {
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
    mut pending_edits: ResMut<PendingEditedOverrideStore>,
    mut settings: ResMut<WorldSettings>,
    mut commands: Commands,
    active: Option<Res<ActiveWorld>>,
) {
    if !state.active() || !runtime.auto_enter || *game.get() != GameState::MainMenu {
        return;
    }
    if active.is_none() {
        let seed = env_u32("VOXEL_NATIVE_AGENT_SEED").unwrap_or(settings.seed);
        let world_profile = std::env::var("VOXEL_NATIVE_AGENT_PROFILE")
            .ok()
            .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
                "astral" | "astral_frontier" | "frontier" => Some(WorldProfile::AstralFrontier),
                "natural" => Some(WorldProfile::Natural),
                _ => None,
            })
            .unwrap_or(settings.world_profile);
        let scenery_quality = std::env::var("VOXEL_NATIVE_AGENT_SCENERY")
            .ok()
            .as_deref()
            .and_then(parse_agent_scenery_quality)
            .unwrap_or(settings.scenery_quality);
        let world_name = std::env::var("VOXEL_NATIVE_AGENT_WORLD")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "agent_control".into());
        let identity = WorldGenerationIdentity {
            seed,
            world_profile,
            scenery_quality,
            terrain_grammar: TerrainGrammarVersion::CURRENT,
        };
        let mut meta = WorldMeta::new_with_identity(world_name, identity);
        if std::env::var("VOXEL_NATIVE_AGENT_FOCUS")
            .map(|value| value.trim().eq_ignore_ascii_case("river"))
            .unwrap_or(false)
        {
            let generator = crate::terrain::TerrainGenerator::from_identity(identity);
            if let Some(focus) = generator.find_hydrographic_focus(0, 0, 4096) {
                meta.player_pos = [
                    focus.x as f32 + 0.5,
                    crate::terrain::WATER_LEVEL as f32 + 34.0,
                    focus.y as f32 + 0.5,
                ];
                meta.player_yaw = 0.0;
                meta.player_pitch = -0.28;
            }
        }
        meta.time_mode = TimeMode::Fixed;
        meta.time_of_day = env_f32("VOXEL_NATIVE_AGENT_HOUR")
            .unwrap_or(10.8)
            .clamp(0.0, 24.0);
        let allow_existing_exact =
            agent_world_entry_allows_existing_exact(isolated_observer_enabled());
        let meta = match crate::world::prepare_programmatic_world_entry(
            &meta,
            allow_existing_exact,
            &mut pending_edits,
        ) {
            Ok(meta) => meta,
            Err(reason) => {
                warn!("agent control: refused world entry: {reason}");
                return;
            }
        };
        settings.seed = seed;
        settings.world_profile = world_profile;
        settings.scenery_quality = identity.scenery_quality;
        settings.terrain_grammar = identity.terrain_grammar;
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

fn agent_control_heartbeat(
    time: Res<Time>,
    mut runtime: ResMut<AgentControlRuntime>,
    state: Res<AgentControlState>,
) {
    if advance_agent_bridge_heartbeat(&mut runtime.bridge_timer, time.delta_seconds()) {
        write_boot_status(&runtime, &state, "heartbeat");
    }
}

fn advance_agent_bridge_heartbeat(timer: &mut f32, delta_seconds: f32) -> bool {
    let delta_seconds = if delta_seconds.is_finite() {
        delta_seconds.clamp(0.0, AGENT_BRIDGE_HEARTBEAT_SECONDS)
    } else {
        0.0
    };
    *timer -= delta_seconds;
    if *timer > 0.0 {
        return false;
    }
    *timer = AGENT_BRIDGE_HEARTBEAT_SECONDS;
    true
}

fn apply_agent_build_mode(
    game: Res<State<GameState>>,
    state: Res<AgentControlState>,
    mut runtime: ResMut<AgentControlRuntime>,
    mut mode: Option<ResMut<ModeContext>>,
    mut toolbelt: Option<ResMut<ToolbeltState>>,
    mut builder: Option<ResMut<BuilderState>>,
    mut tool_controller: Option<ResMut<ToolController>>,
    sketch_doc: Option<Res<SketchDocument>>,
) {
    if !state.active() || *game.get() != GameState::InGame {
        return;
    }
    if state.build_mode.trim().is_empty() && state.build_tool.trim().is_empty() {
        return;
    }
    // A command is an edge, not a held button. Re-applying a semantic tool
    // every frame would continuously restart previews and make drawing feel
    // mysteriously broken even though the highlighted icon looked correct.
    if runtime.last_build_sequence == Some(state.sequence) {
        return;
    }
    runtime.last_build_sequence = Some(state.sequence);

    let Some(mode) = mode.as_deref_mut() else {
        runtime.last_error = Some("mode resource unavailable".into());
        return;
    };
    let current_tool = mode.build_tool().unwrap_or_else(|| {
        toolbelt
            .as_deref()
            .map(|toolbelt| toolbelt.tool)
            .unwrap_or(ToolbeltTool::DrawRect)
    });
    let selection = if state.build_tool.trim().is_empty() {
        None
    } else {
        match parse_agent_build_selection(&state.build_tool) {
            Some(selection) => Some(selection),
            None => {
                runtime.last_error = Some(format!("unknown build tool '{}'", state.build_tool));
                return;
            }
        }
    };
    let tool = selection
        .map(ToolboxSelection::tool)
        .unwrap_or(current_tool);

    if let Some(selection) = selection {
        let (Some(toolbelt), Some(builder), Some(tool_controller), Some(sketch_doc)) = (
            toolbelt.as_deref_mut(),
            builder.as_deref_mut(),
            tool_controller.as_deref_mut(),
            sketch_doc.as_deref(),
        ) else {
            runtime.last_error = Some("semantic editor resources unavailable".into());
            return;
        };
        // Use exactly the same transaction-safe activation path as a visible
        // toolbox click. This keeps brush/material presets, house guides,
        // ToolController ownership, status copy, and selection lifecycle in
        // one authoritative implementation.
        apply_toolbox_selection(
            selection,
            toolbelt,
            mode,
            builder,
            tool_controller,
            sketch_doc.default_material(),
        );
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
            let status = selection
                .map(ToolboxSelection::live_status)
                .unwrap_or_else(|| format!("Agent Build Live: {}. {}", tool.label(), tool.hint()));
            mode.set(ActiveMode::BuildLive { tool }, status);
        }
        _ => {
            runtime.last_error = Some(format!("unknown build mode '{}'", state.build_mode));
            return;
        }
    }

    if let Some(toolbelt) = toolbelt.as_deref_mut() {
        toolbelt.status = mode.status.clone();
    }
    if runtime.last_error.as_deref().is_some_and(|error| {
        error.starts_with("unknown build")
            || error == "mode resource unavailable"
            || error == "semantic editor resources unavailable"
    }) {
        runtime.last_error = None;
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

    update_agent_input_error(&mut runtime.last_error, input_errors);
}

fn update_agent_input_error(last_error: &mut Option<String>, input_errors: Vec<String>) {
    if input_errors.is_empty() {
        // Input polling runs every frame. It may only clear an error that it
        // owns; otherwise a valid input frame could hide a build, bot, shader,
        // or control-file failure before the overlay/status capture sees it.
        if last_error
            .as_deref()
            .is_some_and(|error| error.starts_with("input: "))
        {
            *last_error = None;
        }
    } else {
        *last_error = Some(format!("input: {}", input_errors.join(", ")));
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
/// Horizontal space reserved for the Sketch Editor's left toolbox plus a
/// readable gap. This is a layout contract, not a one-resolution pixel fix.
const AGENT_OVERLAY_TOOLBOX_CLEARANCE: f32 = 76.0;
const AGENT_CONTROL_ACTION_WIDTH: f32 = 154.0;
const AGENT_BRAND_TITLE_SIZE: f32 = 30.0;
const AGENT_BRAND_STATUS_SIZE: f32 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct AgentOverlayLayout {
    left: f32,
    width: f32,
    compact: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentControlVisualState {
    selected: bool,
    status_active: bool,
}

fn agent_control_visual_state(
    available: bool,
    enabled: bool,
    has_error: bool,
) -> AgentControlVisualState {
    let selected = available && enabled;
    AgentControlVisualState {
        selected,
        status_active: selected && !has_error,
    }
}

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

fn agent_overlay_layout(screen_size: egui::Vec2) -> AgentOverlayLayout {
    let screen_width = if screen_size.x.is_finite() {
        screen_size.x.max(1.0)
    } else {
        AGENT_OVERLAY_TOOLBOX_CLEARANCE + AGENT_OVERLAY_PANEL_WIDTH
    };
    let screen_height = if screen_size.y.is_finite() {
        screen_size.y.max(1.0)
    } else {
        720.0
    };
    let left = AGENT_OVERLAY_TOOLBOX_CLEARANCE.min((screen_width - 1.0).max(0.0));
    let width = adaptive_agent_panel_width(screen_width - left, AGENT_OVERLAY_PANEL_WIDTH);

    AgentOverlayLayout {
        left,
        width,
        compact: width < 460.0 || screen_height < 560.0,
    }
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

fn agent_control_panel<R>(
    ui: &mut egui::Ui,
    width: f32,
    theme: crate::theme::ThemeSettings,
    animation_id: egui::Id,
    selected: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    ui.set_width(width);
    crate::ui_kit::surface_panel_animated(ui, theme, animation_id, selected, |ui| {
        let content_width = ui.available_width().min(width);
        if content_width.is_finite() {
            ui.set_min_width(content_width.max(0.0));
        }
        add_contents(ui)
    })
}

fn agent_control_status_signal(
    ui: &mut egui::Ui,
    active: bool,
    theme: crate::theme::ThemeSettings,
) -> egui::Response {
    crate::ui_kit::signal_reactor(ui, active, theme)
}

fn set_agent_control_enabled(state: &mut AgentControlState, next_enabled: bool) -> bool {
    if state.enabled == next_enabled {
        return false;
    }
    let Some(next_sequence) = state.sequence.checked_add(1) else {
        return false;
    };

    state.sequence = next_sequence;
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
    state.camera_pose = None;
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

fn agent_control_toggle_panel(
    mut contexts: EguiContexts,
    mut state: ResMut<AgentControlState>,
    runtime: Res<AgentControlRuntime>,
    settings: Res<WorldSettings>,
) {
    let theme = settings.theme;
    let colors = theme.semantic();
    let control_available = state.runtime_enabled && runtime.runtime_enabled;
    let isolated_observer = isolated_observer_enabled();
    let control_mutable =
        control_available && agent_control_file_rewrite_allowed(isolated_observer);
    let visual = agent_control_visual_state(
        control_available,
        state.enabled,
        runtime.last_error.is_some(),
    );
    let control_owner = if !control_available {
        "UNAVAILABLE"
    } else if isolated_observer {
        "EXTERNAL"
    } else if state.enabled {
        "AGENT"
    } else {
        "PLAYER"
    };
    let status_message = runtime.last_error.clone().unwrap_or_else(|| {
        if isolated_observer {
            "isolated observer: external control file is authoritative".into()
        } else {
            state.status.clone()
        }
    });
    let status_color = if runtime.last_error.is_some() {
        colors.danger
    } else if visual.status_active {
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
            agent_control_panel(
                ui,
                panel_width,
                theme,
                egui::Id::new("agent_control_toggle_panel_motion"),
                visual.selected,
                |ui| {
                    ui.horizontal_wrapped(|ui| {
                        agent_control_status_signal(ui, visual.status_active, theme);
                        crate::ui_kit::status_chip(ui, Icon::Hud, "CONTROL", control_owner, theme);
                        let response =
                            agent_control_action(ui, state.enabled, control_mutable, theme);
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
                },
            );
        });
}

fn agent_control_overlay(
    mut contexts: EguiContexts,
    state: Res<AgentControlState>,
    runtime: Res<AgentControlRuntime>,
    bot_commands: Res<BotCommandStateMachine>,
    settings: Res<WorldSettings>,
    game: Res<State<GameState>>,
    active_weapon: Option<Res<ActiveWeapon>>,
    toolbelt: Option<Res<ToolbeltState>>,
    tool_controller: Option<Res<ToolController>>,
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
    let overlay_layout = agent_overlay_layout(ctx.screen_rect().size());
    let visual = agent_control_visual_state(
        state.runtime_enabled && runtime.runtime_enabled,
        state.enabled,
        runtime.last_error.is_some(),
    );
    let bot = bot_command_telemetry(&bot_commands, state.latest_bot_command_id);
    egui::Area::new(egui::Id::new("agent_control_live_overlay"))
        .anchor(
            egui::Align2::LEFT_TOP,
            egui::vec2(overlay_layout.left, 12.0),
        )
        .show(ctx, |ui| {
            agent_control_panel(
                ui,
                overlay_layout.width,
                theme,
                egui::Id::new("agent_control_live_panel_motion"),
                visual.selected,
                |ui| {
                    ui.horizontal(|ui| {
                        agent_control_status_signal(ui, visual.status_active, theme);
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.label(
                                    egui::RichText::new("AGENT CONTROL")
                                        .color(colors.accent)
                                        .size(AGENT_BRAND_TITLE_SIZE)
                                        .strong()
                                        .monospace(),
                                );
                                ui.label(
                                    egui::RichText::new("// LIVE")
                                        .color(colors.success)
                                        .size(AGENT_BRAND_TITLE_SIZE)
                                        .strong()
                                        .monospace(),
                                );
                            });
                            ui.label(
                                egui::RichText::new(format!(
                                    "●  {}",
                                    state.status.to_ascii_uppercase()
                                ))
                                    .color(if visual.status_active {
                                        colors.success
                                    } else {
                                        colors.danger
                                    })
                                    .size(AGENT_BRAND_STATUS_SIZE)
                                    .strong()
                                    .monospace(),
                            );
                        });
                    });
                    if !state.view_label.is_empty() {
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(format!("VIEW // {}", state.view_label))
                                .color(colors.accent)
                                .size(15.0)
                                .strong()
                                .monospace(),
                        );
                    }
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
                        crate::ui_kit::status_chip(
                            ui,
                            Icon::Follow,
                            "BOT",
                            &state
                                .latest_bot_command_id
                                .map(|id| id.to_string())
                                .unwrap_or_else(|| "NONE".into()),
                            theme,
                        );
                        crate::ui_kit::status_chip(
                            ui,
                            Icon::Approve,
                            "BOT STATE",
                            &bot.state,
                            theme,
                        );
                        crate::ui_kit::status_chip(
                            ui,
                            Icon::Builder,
                            "TASK",
                            &bot.operation,
                            theme,
                        );
                        crate::ui_kit::status_chip(
                            ui,
                            Icon::Builder,
                            "EDITOR",
                            &tool_controller
                                .as_deref()
                                .map(|controller| format!("{:?}", controller.active_tool()))
                                .unwrap_or_else(|| "NONE".into()),
                            theme,
                        );
                    });
                    if !overlay_layout.compact {
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
                        ui.label(
                            egui::RichText::new(format!(
                                "bot {}  voxels {}  chunks {}  issues {}/{}",
                                state.bot_status,
                                bot.estimated_voxel_cost
                                    .map(|value| value.to_string())
                                    .unwrap_or_else(|| "-".into()),
                                bot.estimated_chunk_cost
                                    .map(|value| value.to_string())
                                    .unwrap_or_else(|| "-".into()),
                                bot.warning_count,
                                bot.error_count
                            ))
                            .monospace()
                            .size(11.0)
                            .color(if bot.error_count > 0 {
                                colors.danger
                            } else {
                                colors.text_muted
                            }),
                        );
                    }
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
                    if !state.view_label.is_empty() {
                        ui.monospace(
                            egui::RichText::new(format!("OCR_VIEW={}", state.view_label))
                                .color(ocr_color)
                                .size(14.0),
                        );
                    }
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
                            "OCR_EDITOR={} OCR_PHASE={} OCR_SELECTION={} OCR_EDIT_OBJECT={}",
                            tool_controller
                                .as_deref()
                                .map(|controller| format!("{:?}", controller.active_tool()))
                                .unwrap_or_else(|| "none".into()),
                            tool_controller
                                .as_deref()
                                .map(|controller| format!("{:?}", controller.tool_phase()))
                                .unwrap_or_else(|| "none".into()),
                            tool_controller
                                .as_deref()
                                .map(|controller| controller.selection().len())
                                .unwrap_or(0),
                            tool_controller
                                .as_deref()
                                .is_some_and(|controller| controller.edit_object_active())
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
                    ui.monospace(
                        egui::RichText::new(format!(
                            "OCR_BOT_ID={} OCR_BOT_STATE={} OCR_BOT_OP={} OCR_BOT_REV={}",
                            state
                                .latest_bot_command_id
                                .map(|id| id.to_string())
                                .unwrap_or_else(|| "none".into()),
                            bot.state,
                            bot.operation,
                            bot.revision
                                .map(|revision| revision.to_string())
                                .unwrap_or_else(|| "none".into())
                        ))
                        .color(ocr_color)
                        .size(14.0),
                    );
                    ui.monospace(
                        egui::RichText::new(format!(
                            "OCR_BOT_VOXELS={} OCR_BOT_CHUNKS={} OCR_BOT_WARN={} OCR_BOT_ERRORS={}",
                            bot.estimated_voxel_cost
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "none".into()),
                            bot.estimated_chunk_cost
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "none".into()),
                            bot.warning_count,
                            bot.error_count
                        ))
                        .color(ocr_color)
                        .size(14.0),
                    );
                    ui.monospace(
                        egui::RichText::new(format!(
                            "OCR_BOT_STATUS={}",
                            state.bot_status
                        ))
                        .color(ocr_color)
                        .size(14.0),
                    );
                },
            );
        });
}

const AGENT_CAPTURE_SETTLE_FRAMES: u8 = 2;
const AGENT_SCREENSHOT_MAX_PIXELS: u64 = 16_777_216;
const AGENT_SCREENSHOT_MAX_ENCODED_BYTES: usize = 64 * 1024 * 1024;
const AGENT_SCREENSHOT_ERROR_MAX_CHARS: usize = 256;

#[cfg(not(target_arch = "wasm32"))]
fn bounded_agent_screenshot_error(index: usize, error: impl std::fmt::Display) -> String {
    let message = format!("screenshot: capture {index:04} failed: {error}");
    message
        .chars()
        .filter(|character| !character.is_control())
        .take(AGENT_SCREENSHOT_ERROR_MAX_CHARS)
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn lock_agent_capture_ledger(
    ledger: &Arc<Mutex<AgentCaptureLedger>>,
) -> std::sync::MutexGuard<'_, AgentCaptureLedger> {
    ledger.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(not(target_arch = "wasm32"))]
fn agent_capture_snapshot(ledger: &Arc<Mutex<AgentCaptureLedger>>) -> AgentCaptureSnapshot {
    lock_agent_capture_ledger(ledger).snapshot()
}

#[cfg(not(target_arch = "wasm32"))]
fn agent_status_error_projection(
    runtime_last_error: &Option<String>,
    capture_snapshot: &AgentCaptureSnapshot,
) -> (Option<String>, Option<String>) {
    (
        runtime_last_error.clone(),
        capture_snapshot.latest_error.clone(),
    )
}

fn mission_feed_error_count(
    bot_error_count: usize,
    runtime_error: &Option<String>,
    screenshot_error: &Option<String>,
) -> usize {
    bot_error_count
        .saturating_add(usize::from(runtime_error.is_some()))
        .saturating_add(usize::from(screenshot_error.is_some()))
}

fn advance_sequence_screenshot(requested: bool, pending_frames: &mut Option<u8>) -> bool {
    if requested {
        *pending_frames = Some(AGENT_CAPTURE_SETTLE_FRAMES);
        return false;
    }
    match *pending_frames {
        Some(frames) if frames > 1 => {
            *pending_frames = Some(frames - 1);
            false
        }
        Some(_) => {
            *pending_frames = None;
            true
        }
        None => false,
    }
}

fn agent_control_capture(
    time: Res<Time>,
    mut runtime: ResMut<AgentControlRuntime>,
    state: Res<AgentControlState>,
    mut screenshots: ResMut<ScreenshotManager>,
    windows: Query<Entity, With<PrimaryWindow>>,
) {
    runtime.screenshot_timer -= time.delta_seconds();
    let sequence_request =
        state.screenshot && state.sequence != runtime.last_sequence_for_screenshot;
    if sequence_request {
        // Mark the edge immediately so a held screenshot command cannot arm a
        // second capture while the UI/render graph gets two settling frames.
        runtime.last_sequence_for_screenshot = state.sequence;
    }
    let sequence_capture = advance_sequence_screenshot(
        sequence_request,
        &mut runtime.pending_sequence_screenshot_frames,
    );
    let interval_capture = runtime.screenshot_interval > 0.0 && runtime.screenshot_timer <= 0.0;
    if !sequence_capture && !interval_capture {
        return;
    }
    if runtime.in_game_frames < 3 {
        #[cfg(not(target_arch = "wasm32"))]
        lock_agent_capture_ledger(&runtime.capture_ledger).record_failure(
            runtime.screenshot_index,
            "capture rejected before the game rendered three complete frames",
        );
        return;
    }
    runtime.screenshot_timer = runtime.screenshot_interval;

    let window = match windows.get_single() {
        Ok(window) => window,
        Err(error) => {
            #[cfg(not(target_arch = "wasm32"))]
            lock_agent_capture_ledger(&runtime.capture_ledger).record_failure(
                runtime.screenshot_index,
                format!("primary window unavailable or ambiguous: {error}"),
            );
            return;
        }
    };

    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Err(e) = prepare_safe_agent_directory(&runtime.session_dir) {
            lock_agent_capture_ledger(&runtime.capture_ledger).record_failure(
                runtime.screenshot_index,
                format!("session directory rejected: {e}"),
            );
            warn!(
                "agent control: could not create {}: {e}",
                runtime.session_dir.display()
            );
            return;
        }
        let capture_index = runtime.screenshot_index;
        let Some(next_capture_index) = capture_index.checked_add(1) else {
            lock_agent_capture_ledger(&runtime.capture_ledger)
                .record_failure(capture_index, "filename cursor overflow");
            warn!("agent control: screenshot filename cursor exhausted");
            return;
        };
        let path = runtime
            .session_dir
            .join(format!("live_{capture_index:04}.png"));
        let callback_path = path.clone();
        let capture_ledger = Arc::clone(&runtime.capture_ledger);
        match screenshots.take_screenshot(
            window,
            move |captured| match encode_agent_screenshot_png(captured)
                .and_then(|bytes| write_agent_screenshot_png(&callback_path, &bytes))
            {
                Ok(()) => {
                    lock_agent_capture_ledger(&capture_ledger)
                        .record_success(capture_index, callback_path.clone());
                    info!(
                        "agent control: screenshot saved safely to {}",
                        callback_path.display()
                    );
                }
                Err(error) => {
                    lock_agent_capture_ledger(&capture_ledger)
                        .record_failure(capture_index, &error);
                    warn!(
                        "agent control: screenshot write rejected for {}: {error}",
                        callback_path.display()
                    );
                }
            },
        ) {
            Ok(_) => {
                runtime.screenshot_index = next_capture_index;
                info!("agent control: screenshot requested for {}", path.display());
            }
            Err(error) => {
                lock_agent_capture_ledger(&runtime.capture_ledger)
                    .record_failure(capture_index, &error);
                warn!("agent control: screenshot request failed: {error}");
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn encode_agent_screenshot_png(captured: Image) -> std::io::Result<Vec<u8>> {
    let extent = captured.texture_descriptor.size;
    let pixels = u64::from(extent.width)
        .checked_mul(u64::from(extent.height))
        .and_then(|count| count.checked_mul(u64::from(extent.depth_or_array_layers)))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "agent screenshot dimensions overflow their fixed work budget",
            )
        })?;
    if pixels == 0 || pixels > AGENT_SCREENSHOT_MAX_PIXELS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "agent screenshot contains {pixels} pixels; limit is {AGENT_SCREENSHOT_MAX_PIXELS}"
            ),
        ));
    }

    let dynamic = captured.try_into_dynamic().map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("agent screenshot texture conversion failed: {error}"),
        )
    })?;
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(dynamic.to_rgb8())
        .write_to(&mut encoded, image::ImageFormat::Png)
        .map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("agent screenshot PNG encoding failed: {error}"),
            )
        })?;
    let encoded = encoded.into_inner();
    if encoded.len() > AGENT_SCREENSHOT_MAX_ENCODED_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "agent screenshot encodes to {} bytes; limit is {AGENT_SCREENSHOT_MAX_ENCODED_BYTES}",
                encoded.len()
            ),
        ));
    }
    Ok(encoded)
}

fn agent_control_status(
    time: Res<Time>,
    diagnostics: Res<DiagnosticsStore>,
    game: Res<State<GameState>>,
    world: Res<VoxelWorld>,
    settings: Res<WorldSettings>,
    active_world: Option<Res<ActiveWorld>>,
    streamer: Res<ChunkStreamer>,
    governor: Res<StreamingGovernor>,
    state: Res<AgentControlState>,
    bot_commands: Res<BotCommandStateMachine>,
    mut runtime: ResMut<AgentControlRuntime>,
    active_weapon: Option<Res<ActiveWeapon>>,
    toolbelt: Option<Res<ToolbeltState>>,
    tool_controller: Option<Res<ToolController>>,
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
        let bot = bot_command_telemetry(&bot_commands, state.latest_bot_command_id);
        let environment_x = transform.translation.x.floor() as i32;
        let environment_z = transform.translation.z.floor() as i32;
        let environment = world
            .generator
            .environment_sample_at(environment_x, environment_z);
        let environment_surface_y = world
            .generator
            .surface_height_at(environment_x, environment_z);
        let environment_biome = format!(
            "{:?}",
            world.generator.biome_at(environment_x, environment_z)
        );
        let capture_snapshot = agent_capture_snapshot(&runtime.capture_ledger);
        let last_screenshot = capture_snapshot
            .last_screenshot
            .as_ref()
            .map(|path| path.to_string_lossy().to_string());
        let (reported_error, last_screenshot_error) =
            agent_status_error_projection(&runtime.last_error, &capture_snapshot);
        let status = AgentLiveStatus {
            seconds: time.elapsed_seconds(),
            game_state: format!("{:?}", game.get()),
            command_sequence: state.sequence,
            command_status: state.status.clone(),
            command_view_label: state.view_label.clone(),
            command_camera_pose_requested: state.camera_pose.is_some(),
            camera_command_sequence_cursor: runtime.last_camera_command_sequence,
            camera_pose_handled_sequence: runtime.last_camera_pose_sequence,
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
            bot_last_request_id: runtime
                .last_applied_bot_request
                .as_ref()
                .map(|request| request.request_id),
            bot_latest_command_id: state.latest_bot_command_id,
            bot_operation: bot.operation,
            bot_state: bot.state,
            bot_revision: bot.revision,
            bot_estimated_voxel_cost: bot.estimated_voxel_cost,
            bot_estimated_chunk_cost: bot.estimated_chunk_cost,
            bot_applied_voxel_edits: bot.applied_voxel_edits,
            bot_touched_chunks: bot.touched_chunks,
            bot_spawned_projects: bot.spawned_projects,
            bot_warning_count: bot.warning_count,
            bot_error_count: bot.error_count,
            bot_block_reason: bot.block_reason,
            bot_status: state.bot_status.clone(),
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
            editor_tool: tool_controller
                .as_deref()
                .map(|controller| format!("{:?}", controller.active_tool()))
                .unwrap_or_else(|| "none".into()),
            editor_phase: tool_controller
                .as_deref()
                .map(|controller| format!("{:?}", controller.tool_phase()))
                .unwrap_or_else(|| "none".into()),
            editor_selection_count: tool_controller
                .as_deref()
                .map(|controller| controller.selection().len())
                .unwrap_or(0),
            editor_edit_object: tool_controller
                .as_deref()
                .is_some_and(|controller| controller.edit_object_active()),
            position: [
                transform.translation.x,
                transform.translation.y,
                transform.translation.z,
            ],
            environment_biome,
            environment_surface_y,
            environment_temperature_norm: environment.temperature_norm,
            environment_atmospheric_moisture: environment.atmospheric_moisture,
            environment_soil_moisture: environment.soil_moisture,
            environment_river_strength: environment.river_strength,
            environment_mineral_resonance: environment.mineral_resonance,
            environment_flowering_resonance: environment.flowering_resonance,
            environment_flow_direction: environment.flow_direction,
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
            last_error: reported_error,
            last_screenshot_error,
            screenshot_count: capture_snapshot.successful_count,
            in_game_frames: runtime.in_game_frames,
            last_screenshot,
            session_dir: runtime_session_dir(&runtime),
        };
        if let Err(e) = prepare_safe_agent_directory(&runtime.session_dir) {
            warn!(
                "agent control: could not create {}: {e}",
                runtime.session_dir.display()
            );
            return;
        }
        let path = runtime.session_dir.join("status.ron");
        if let Ok(text) = ron::ser::to_string_pretty(&status, ron::ser::PrettyConfig::default()) {
            if let Err(error) = atomic_replace_agent_telemetry_text(&path, &text) {
                warn!(
                    "agent control: could not atomically write {}: {error}",
                    path.display()
                );
            }
        }
        if runtime.mission_feed_enabled {
            let (world_name, world_profile, world_seed, time_of_day) = active_world
                .as_deref()
                .map(|active| {
                    (
                        active.meta.name.clone(),
                        match active.meta.world_profile {
                            WorldProfile::Natural => "natural".to_owned(),
                            WorldProfile::AstralFrontier => "astral".to_owned(),
                        },
                        active.meta.seed,
                        active.meta.time_of_day,
                    )
                })
                .unwrap_or_else(|| {
                    (
                        "unloaded".to_owned(),
                        match settings.world_profile {
                            WorldProfile::Natural => "natural".to_owned(),
                            WorldProfile::AstralFrontier => "astral".to_owned(),
                        },
                        settings.seed,
                        settings.time_of_day,
                    )
                });
            let feed = crate::mission_control::MissionFeedSnapshot {
                schema_version: crate::mission_control::MISSION_FEED_SCHEMA_VERSION,
                agent_id: runtime.mission_agent_id.clone(),
                fleet_id: runtime.mission_fleet_id.clone(),
                display_name: runtime.mission_agent_name.clone(),
                role: runtime.mission_agent_role.clone(),
                task: runtime.mission_agent_task.clone(),
                process_id: std::process::id(),
                heartbeat_epoch: crate::platform::now_epoch(),
                status: status.command_status.clone(),
                game_state: status.game_state.clone(),
                world_name,
                world_profile,
                world_seed,
                time_of_day,
                position: status.position,
                fps: status.fps,
                frame_ms: status.frame_ms,
                stall_count: status.stall_count,
                loaded_chunks: status.loaded_chunks,
                pending_work: status.pending_terrain + status.pending_meshes + status.dirty_chunks,
                warning_count: status.bot_warning_count,
                error_count: mission_feed_error_count(
                    status.bot_error_count,
                    &status.last_error,
                    &status.last_screenshot_error,
                ),
                control_enabled: status.control_enabled,
                capability_schema_version:
                    crate::agent_capabilities::AGENT_CAPABILITY_SCHEMA_VERSION,
                power_profile_id: crate::agent_capabilities::SHARED_POWER_PROFILE_ID.into(),
                direct_bridge_ready: crate::agent_capabilities::DIRECT_BRIDGE_READY,
                ron_fallback_ready: crate::agent_capabilities::RON_FALLBACK_READY,
                visual_capture_ready: crate::agent_capabilities::VISUAL_CAPTURE_READY,
                last_screenshot: status.last_screenshot.clone(),
                session_dir: status.session_dir.clone(),
                live_link_side: runtime.mission_live_link_side.clone(),
                live_link_bind: runtime.mission_live_link_bind.clone(),
                live_link_peer: runtime.mission_live_link_peer.clone(),
            };
            let feed_path = runtime.session_dir.join("mission_feed.ron");
            if let Ok(text) = ron::ser::to_string_pretty(&feed, ron::ser::PrettyConfig::default()) {
                if let Err(error) = atomic_replace_agent_telemetry_text(&feed_path, &text) {
                    warn!(
                        "agent control: could not atomically write {}: {error}",
                        feed_path.display()
                    );
                }
            }
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
fn read_bounded_control_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;

    let path = rooted_agent_path(path)?;
    if let Some(parent) = path.parent() {
        ensure_safe_agent_directory_chain(parent)?;
    }
    let path_metadata = std::fs::symlink_metadata(&path)?;
    if !path_metadata.file_type().is_file()
        || path_metadata.file_type().is_symlink()
        || agent_native_metadata_is_reparse_point(&path_metadata)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "control path is not a safe regular file",
        ));
    }
    if path_metadata.len() > AGENT_CONTROL_MAX_BYTES as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("control file exceeds the {AGENT_CONTROL_MAX_BYTES}-byte limit"),
        ));
    }

    let file = std::fs::OpenOptions::new().read(true).open(&path)?;
    if let Some(parent) = path.parent() {
        ensure_safe_agent_directory_chain(parent)?;
    }
    let opened_metadata = file.metadata()?;
    if !opened_metadata.file_type().is_file()
        || agent_native_metadata_is_reparse_point(&opened_metadata)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "opened control handle is not a safe regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(opened_metadata.len())
            .unwrap_or(AGENT_CONTROL_MAX_BYTES)
            .min(AGENT_CONTROL_MAX_BYTES),
    );
    file.take((AGENT_CONTROL_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    decode_bounded_control_bytes(bytes)
}

#[cfg(not(target_arch = "wasm32"))]
fn decode_bounded_control_bytes(bytes: Vec<u8>) -> std::io::Result<String> {
    if bytes.len() > AGENT_CONTROL_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("control file exceeds the {AGENT_CONTROL_MAX_BYTES}-byte limit"),
        ));
    }
    String::from_utf8(bytes).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("control file is not UTF-8: {error}"),
        )
    })
}

#[cfg(all(not(target_arch = "wasm32"), windows))]
fn agent_native_metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(all(not(target_arch = "wasm32"), not(windows)))]
fn agent_native_metadata_is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_control_file(path: &std::path::Path) {
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
    view_label: "",
    // Example: Some((position: (120.0, 90.0, -64.0), yaw: 0.5, pitch: -0.25))
    camera_pose: None,
    fire: false,
    scope: false,
    keys: [],
    mouse_buttons: [],
    game_state: "",
    build_mode: "",
    build_tool: "",
    bot_command: None,
    handoff: false,
    screenshot: false,
    exit: false,
)"#;
    if let Err(error) = create_agent_control_file_if_missing(path, template) {
        warn!(
            "agent control: could not safely create control file {}: {error}",
            path.display()
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn create_agent_control_file_if_missing(
    path: &std::path::Path,
    text: &str,
) -> std::io::Result<bool> {
    use std::io::Write;

    let path = rooted_agent_path(path)?;
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    prepare_safe_agent_directory(parent)?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            ensure_safe_agent_regular_file(&path)?;
            return Ok(false);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    ensure_safe_agent_directory_chain(parent)?;
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure_safe_agent_regular_file(&path)?;
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let write_result = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = write_result {
        if ensure_safe_agent_directory_chain(parent).is_ok()
            && ensure_safe_agent_regular_file(&path).is_ok()
        {
            let _ = std::fs::remove_file(&path);
        }
        return Err(error);
    }
    ensure_safe_agent_directory_chain(parent)?;
    ensure_safe_agent_regular_file(&path)?;
    Ok(true)
}

fn write_agent_control_file(runtime: &AgentControlRuntime, state: &AgentControlState) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if !agent_control_file_rewrite_allowed(isolated_observer_enabled()) {
            return;
        }
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
            view_label: state.view_label.clone(),
            camera_pose: None,
            fire: false,
            scope: false,
            keys: Vec::new(),
            mouse_buttons: Vec::new(),
            game_state: state.game_state.clone(),
            build_mode: state.build_mode.clone(),
            build_tool: String::new(),
            bot_command: None,
            handoff: state.handoff,
            screenshot: false,
            exit: false,
        };
        if let Ok(text) = ron::ser::to_string_pretty(&command, ron::ser::PrettyConfig::default()) {
            if let Err(error) = atomic_write_agent_text(&runtime.control_path, &text) {
                warn!(
                    "agent control: could not atomically rewrite {}: {error}",
                    runtime.control_path.display()
                );
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (runtime, state);
    }
}

fn agent_control_file_rewrite_allowed(isolated_observer: bool) -> bool {
    !isolated_observer
}

#[cfg(not(target_arch = "wasm32"))]
fn atomic_write_agent_text(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    atomic_replace_agent_bytes(path, text.as_bytes(), true)
}

#[cfg(not(target_arch = "wasm32"))]
fn rooted_agent_path_from(
    path: &std::path::Path,
    current_dir: &std::path::Path,
) -> std::io::Result<PathBuf> {
    let rooted = if path.is_absolute() {
        path.to_path_buf()
    } else {
        if !current_dir.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "current directory is not absolute",
            ));
        }
        current_dir.join(path)
    };
    if !rooted.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "agent path did not resolve to an absolute path",
        ));
    }
    Ok(rooted)
}

#[cfg(not(target_arch = "wasm32"))]
fn rooted_agent_path(path: &std::path::Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    rooted_agent_path_from(path, &std::env::current_dir()?)
}

/// Atomically replaces observer telemetry without a durable `sync_all` on the
/// render thread. Telemetry is reconstructible and prioritizes a hitch-free
/// persistent view; control/capability authority requests durability below.
#[cfg(not(target_arch = "wasm32"))]
fn atomic_replace_agent_telemetry_text(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    atomic_replace_agent_bytes(path, text.as_bytes(), false)
}

#[cfg(not(target_arch = "wasm32"))]
fn write_agent_capability_manifest_safely(
    session_dir: &std::path::Path,
    agent_id: &str,
    fleet_id: &str,
) -> std::io::Result<PathBuf> {
    let text = crate::agent_capabilities::agent_capability_manifest_text(agent_id, fleet_id)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let path = session_dir.join("capabilities.ron");
    atomic_replace_agent_bytes(&path, text.as_bytes(), true)?;
    Ok(path)
}

#[cfg(not(target_arch = "wasm32"))]
fn write_agent_screenshot_png(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    const PNG_IEND: &[u8; 12] = b"\0\0\0\0IEND\xaeB`\x82";
    if bytes.len() > AGENT_SCREENSHOT_MAX_ENCODED_BYTES
        || !bytes.starts_with(PNG_SIGNATURE)
        || !bytes.ends_with(PNG_IEND)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "agent screenshot is not one complete bounded PNG",
        ));
    }
    atomic_replace_agent_bytes(path, bytes, false)
}

#[cfg(not(target_arch = "wasm32"))]
fn atomic_replace_agent_bytes(
    path: &std::path::Path,
    bytes: &[u8],
    durable: bool,
) -> std::io::Result<()> {
    use std::io::Write;

    if bytes.len() > AGENT_SCREENSHOT_MAX_ENCODED_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "agent output exceeds the fixed 64 MiB byte budget",
        ));
    }
    let path = rooted_agent_path(path)?;
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    prepare_safe_agent_directory(parent)?;
    ensure_safe_agent_regular_file_or_missing(&path)?;
    let temp_path = path.with_file_name(format!(
        ".{}.agent-tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("agent-output"),
        std::process::id(),
        crate::platform::now_nanos_seed()
    ));
    let result = (|| {
        ensure_safe_agent_directory_chain(parent)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(bytes)?;
        if durable {
            file.sync_all()?;
        }
        drop(file);
        ensure_safe_agent_directory_chain(parent)?;
        ensure_safe_agent_regular_file(&temp_path)?;
        ensure_safe_agent_regular_file_or_missing(&path)?;
        if let Err(rename_error) = std::fs::rename(&temp_path, &path) {
            ensure_safe_agent_regular_file_or_missing(&path)?;
            #[cfg(windows)]
            replace_existing_agent_file_windows(&path, &temp_path).map_err(|replace_error| {
                    std::io::Error::new(
                        replace_error.kind(),
                        format!(
                            "atomic agent-output rename failed ({rename_error}); ReplaceFileW failed ({replace_error})"
                        ),
                    )
                })?;
            #[cfg(not(windows))]
            return Err(rename_error);
        }
        ensure_safe_agent_directory_chain(parent)?;
        ensure_safe_agent_regular_file_or_missing(&path)?;
        Ok(())
    })();
    if result.is_err()
        && ensure_safe_agent_directory_chain(parent).is_ok()
        && ensure_safe_agent_regular_file(&temp_path).is_ok()
    {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

#[cfg(not(target_arch = "wasm32"))]
fn prepare_safe_agent_directory(path: &std::path::Path) -> std::io::Result<()> {
    let path = rooted_agent_path(path)?;
    ensure_safe_agent_directory_ancestors_or_missing(&path)?;
    std::fs::create_dir_all(&path)?;
    ensure_safe_agent_directory_chain(&path)
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_safe_agent_directory_ancestors_or_missing(path: &std::path::Path) -> std::io::Result<()> {
    let mut cursor = Some(path);
    while let Some(candidate) = cursor {
        if candidate.as_os_str().is_empty() {
            break;
        }
        match std::fs::symlink_metadata(candidate) {
            Ok(metadata) => ensure_safe_agent_directory_metadata(candidate, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        cursor = candidate.parent();
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_safe_agent_directory_chain(path: &std::path::Path) -> std::io::Result<()> {
    let mut cursor = Some(path);
    while let Some(candidate) = cursor {
        if candidate.as_os_str().is_empty() {
            break;
        }
        let metadata = std::fs::symlink_metadata(candidate)?;
        ensure_safe_agent_directory_metadata(candidate, &metadata)?;
        cursor = candidate.parent();
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_safe_agent_directory_metadata(
    path: &std::path::Path,
    metadata: &std::fs::Metadata,
) -> std::io::Result<()> {
    if metadata.file_type().is_dir()
        && !metadata.file_type().is_symlink()
        && !agent_native_metadata_is_reparse_point(metadata)
    {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("'{}' is not a safe regular directory", path.display()),
        ))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_safe_agent_regular_file_or_missing(path: &std::path::Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_file()
        && !metadata.file_type().is_symlink()
        && !agent_native_metadata_is_reparse_point(&metadata)
    {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("'{}' is not a safe regular file", path.display()),
        ))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_safe_agent_regular_file(path: &std::path::Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_file()
        && !metadata.file_type().is_symlink()
        && !agent_native_metadata_is_reparse_point(&metadata)
    {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("'{}' is not a safe regular file", path.display()),
        ))
    }
}

#[cfg(all(not(target_arch = "wasm32"), windows))]
fn replace_existing_agent_file_windows(
    final_path: &std::path::Path,
    replacement_path: &std::path::Path,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    let final_wide = final_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement_wide = replacement_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both owned UTF-16 buffers are NUL-terminated and live for the
    // complete synchronous call. Optional backup/exclusion pointers are null.
    let replaced = unsafe {
        ReplaceFileW(
            final_wide.as_ptr(),
            replacement_wide.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn normalized_input_name(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .flat_map(|c| c.to_uppercase())
        .collect()
}

fn parse_agent_scenery_quality(value: &str) -> Option<SceneryQuality> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" | "minimal" => Some(SceneryQuality::Off),
        "lean" | "efficient" => Some(SceneryQuality::Lean),
        "balanced" => Some(SceneryQuality::Balanced),
        "lush" | "immersive" => Some(SceneryQuality::Lush),
        _ => None,
    }
}

fn parse_mouse_button(name: &str) -> Option<MouseButton> {
    match normalized_input_name(name).as_str() {
        "LMB" | "LEFT" | "MOUSELEFT" => Some(MouseButton::Left),
        "RMB" | "RIGHT" | "MOUSERIGHT" => Some(MouseButton::Right),
        "MMB" | "MIDDLE" | "MOUSEMIDDLE" => Some(MouseButton::Middle),
        _ => None,
    }
}

/// Parse both legacy builder names and every modern semantic editor workflow.
/// The result is intentionally the same selection type used by the on-screen
/// toolbox, so headless QA cannot drift into a fake "selected but inactive"
/// state as new tools are added.
fn parse_agent_build_selection(name: &str) -> Option<ToolboxSelection> {
    match normalized_input_name(name).as_str() {
        "NAV" | "NAVIGATE" | "INSPECT" | "NAVIGATEINSPECT" | "SELECT" | "SELECTION" => {
            Some(ToolboxSelection::Tool(ToolbeltTool::Navigate))
        }
        "PENCIL" | "LINE" | "EDGE" => Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Pencil)),
        "FILL" | "DRAWRECT" | "RECT" | "RECTANGLE" | "RECTANGLEFILL" => {
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Sketch))
        }
        "CIRCLE" | "DISC" => Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Circle)),
        "POLYGON" | "HEX" | "HEXAGON" => {
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Polygon))
        }
        "ARC" | "CURVE" => Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Arc)),
        "FREEHAND" | "STROKE" => Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Freehand)),
        "PUSH" | "SCULPT" | "PUSHPULL" | "PUSHPULLFACE" => {
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull))
        }
        "MOVE" | "TRANSLATE" => Some(ToolboxSelection::Tool(ToolbeltTool::TransformMove)),
        "SCALE" | "RESIZE" | "SHRINK" | "MINIMIZE" => {
            Some(ToolboxSelection::Tool(ToolbeltTool::TransformScale))
        }
        "ROTATE" | "TURN" => Some(ToolboxSelection::Tool(ToolbeltTool::TransformRotate)),
        "MATERIAL" | "STYLE" | "PAINT" => {
            Some(ToolboxSelection::Tool(ToolbeltTool::MaterialPicker))
        }
        "ROOM" | "INTERIOR" => Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Room)),
        "OPENING" | "DOOR" | "WINDOW" => {
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Opening))
        }
        "HOUSE" | "MODERNHOUSE" => {
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::ModernHouse))
        }
        "ROAD" | "ROADS" | "CITYROAD" => {
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Roads))
        }
        "AREA" | "BOTAREA" | "ZONE" | "DISTRICT" | "CITYDISTRICT" => {
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::BotArea))
        }
        "LANDSCAPE" | "GARDEN" => Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Landscape)),
        "CITY" | "SHELL" | "BUILDING" | "CITYBUILDING" | "CITYSHELL" => {
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::CityShell))
        }
        "TOWER" | "SMARTTOWER" | "SKYLINE" => {
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Skyline))
        }
        "SHIP" | "SPACECRAFT" | "SHUTTLE" => {
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Spacecraft))
        }
        "PLACE" | "BRUSHPLACE" | "PLACEBRUSH" => {
            Some(ToolboxSelection::Tool(ToolbeltTool::BrushPlace))
        }
        "CUT" | "BRUSHCUT" | "CUTBRUSH" => Some(ToolboxSelection::Tool(ToolbeltTool::BrushCut)),
        "STAMP" | "FACADE" | "CITYFACADE" => Some(ToolboxSelection::Tool(ToolbeltTool::CityFacade)),
        "ANIM" | "ANIMATION" | "ANIMATIONPICK" => {
            Some(ToolboxSelection::Tool(ToolbeltTool::AnimationPick))
        }
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

fn isolated_observer_from_flags(runtime_enabled: bool, isolated_requested: bool) -> bool {
    runtime_enabled && isolated_requested
}

fn agent_world_entry_allows_existing_exact(isolated_observer: bool) -> bool {
    !isolated_observer
}

/// Returns true only for an explicitly isolated agent-control session.
///
/// Agent control on its own preserves the normal persistence contract. The
/// additional environment flag lets long-lived visual observers opt out of
/// reading or writing the user's normal settings without changing other agent
/// workflows.
pub(crate) fn isolated_observer_enabled() -> bool {
    isolated_observer_from_flags(
        agent_runtime_enabled(),
        env_flag("VOXEL_NATIVE_AGENT_ISOLATED"),
    )
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

fn bounded_env_text(name: &str, fallback: &str, max_chars: usize, identifier: bool) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| fallback.to_owned());
    let cleaned = value
        .trim()
        .chars()
        .filter(|character| {
            if identifier {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            } else {
                !character.is_control()
            }
        })
        .take(max_chars)
        .collect::<String>();
    if cleaned.is_empty() {
        fallback.chars().take(max_chars).collect()
    } else {
        cleaned
    }
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

    #[cfg(not(target_arch = "wasm32"))]
    struct AgentPathTestRoot(std::path::PathBuf);

    #[cfg(not(target_arch = "wasm32"))]
    impl AgentPathTestRoot {
        fn new(label: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "voxel-native-agent-path-{label}-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("create isolated agent-path test root");
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl Drop for AgentPathTestRoot {
        fn drop(&mut self) {
            let temp = std::env::temp_dir();
            assert!(
                self.0.starts_with(&temp)
                    && self
                        .0
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("voxel-native-agent-path-")),
                "refuse broad agent-path test cleanup"
            );
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn tiny_agent_png() -> Vec<u8> {
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1, 1)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("encode tiny agent screenshot fixture");
        encoded.into_inner()
    }

    fn test_player() -> Player {
        Player {
            yaw: 0.75,
            pitch: 0.25,
            velocity: Vec3::new(3.0, -2.0, 1.0),
            on_ground: true,
            flying: false,
            walk_speed: 5.5,
            fly_speed: 24.0,
            sensitivity: 0.0025,
            placed_on_surface: false,
            jump_buffer: 0.1,
            coyote_time: 0.1,
            fov_bonus: 2.0,
            space_tap_timer: 0.1,
            w_tap_timer: 0.1,
            sprint_latched: true,
            current_eye_height: 1.62,
            current_height: 1.8,
        }
    }

    fn expect_applied(outcome: AgentBotRequestOutcome) -> CommandId {
        match outcome {
            AgentBotRequestOutcome::Applied(id) => id,
            other => panic!("expected applied bot request, got {other:?}"),
        }
    }

    fn render_agent_panel_size(reduced: bool, low_spec: bool, selected: bool) -> egui::Vec2 {
        let ctx = egui::Context::default();
        crate::theme::set_motion_preferences(&ctx, reduced, low_spec);
        let mut panel_size = egui::Vec2::ZERO;

        for frame in 0..3 {
            let mut input = egui::RawInput::default();
            input.time = Some(frame as f64);
            let _ = ctx.run(input, |ctx| {
                egui::Area::new(egui::Id::new("agent_control_panel_test_area")).show(ctx, |ui| {
                    let panel = agent_control_panel(
                        ui,
                        280.0,
                        crate::theme::ThemeSettings::default(),
                        egui::Id::new("agent_control_panel_test"),
                        selected,
                        |ui| {
                            ui.allocate_exact_size(egui::vec2(180.0, 28.0), egui::Sense::hover());
                        },
                    );
                    panel_size = panel.response.rect.size();
                });
            });
        }

        panel_size
    }

    fn render_agent_status_signal(
        reduced: bool,
        low_spec: bool,
        active: bool,
    ) -> (egui::Vec2, std::time::Duration) {
        let ctx = egui::Context::default();
        crate::theme::set_motion_preferences(&ctx, reduced, low_spec);
        let mut signal_size = egui::Vec2::ZERO;
        let mut repaint_delay = std::time::Duration::MAX;

        for frame in 0..3 {
            let mut input = egui::RawInput::default();
            input.time = Some(frame as f64);
            let output = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    signal_size = agent_control_status_signal(
                        ui,
                        active,
                        crate::theme::ThemeSettings::default(),
                    )
                    .rect
                    .size();
                });
            });
            repaint_delay = output
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .expect("root viewport output")
                .repaint_delay;
        }

        (signal_size, repaint_delay)
    }

    #[test]
    fn sequence_screenshot_waits_two_complete_render_frames() {
        let mut pending_frames = None;

        assert!(!advance_sequence_screenshot(true, &mut pending_frames));
        assert_eq!(pending_frames, Some(2));

        assert!(!advance_sequence_screenshot(false, &mut pending_frames));
        assert_eq!(pending_frames, Some(1));

        assert!(advance_sequence_screenshot(false, &mut pending_frames));
        assert_eq!(pending_frames, None);
        assert!(!advance_sequence_screenshot(false, &mut pending_frames));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn capture_ledger_never_publishes_rejected_or_failed_requests() {
        let mut ledger = AgentCaptureLedger::default();
        ledger.record_failure(0, "renderer already has a pending request");
        let rejected = ledger.snapshot();
        assert_eq!(rejected.successful_count, 0);
        assert_eq!(rejected.last_screenshot, None);
        assert!(rejected
            .latest_error
            .as_deref()
            .is_some_and(|error| error.starts_with("screenshot: capture 0000 failed:")));

        let root = AgentPathTestRoot::new("capture-failure-ledger");
        let invalid_path = root.path().join("live_0000.png");
        let write_error = write_agent_screenshot_png(&invalid_path, b"not a PNG")
            .expect_err("invalid PNG must be rejected before publication");
        ledger.record_failure(0, write_error);
        let failed = ledger.snapshot();
        assert_eq!(failed.successful_count, 0);
        assert_eq!(failed.last_screenshot, None);
        assert!(!invalid_path.exists());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn capture_ledger_counts_real_successes_and_keeps_highest_out_of_order_index() {
        let root = AgentPathTestRoot::new("capture-success-ledger");
        let session = root.path().join("agent_runs").join("session");
        prepare_safe_agent_directory(&session).expect("prepare capture-ledger session");
        let png = tiny_agent_png();
        let path_two = session.join("live_0002.png");
        let path_one = session.join("live_0001.png");
        write_agent_screenshot_png(&path_two, &png).expect("publish capture two");
        write_agent_screenshot_png(&path_one, &png).expect("publish capture one");

        let mut ledger = AgentCaptureLedger::default();
        ledger.record_failure(2, "pending failure before successful retry");
        ledger.record_success(2, path_two.clone());
        let first_success = ledger.snapshot();
        assert_eq!(first_success.successful_count, 1);
        assert_eq!(first_success.last_screenshot, Some(path_two.clone()));
        assert_eq!(first_success.latest_error, None);

        ledger.record_success(1, path_one);
        ledger.record_failure(0, "late failure from an older request");
        let out_of_order = ledger.snapshot();
        assert_eq!(out_of_order.successful_count, 2);
        assert_eq!(out_of_order.last_screenshot, Some(path_two));
        assert_eq!(out_of_order.latest_error, None);

        ledger.record_failure(3, "newest request failed");
        let newest_failure = ledger.snapshot();
        assert_eq!(newest_failure.successful_count, 2);
        assert!(newest_failure.last_screenshot.is_some());
        assert!(newest_failure
            .latest_error
            .as_deref()
            .is_some_and(|error| error.contains("capture 0003 failed")));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn status_projection_keeps_runtime_and_screenshot_errors_independent() {
        let mut ledger = AgentCaptureLedger::default();
        ledger.record_failure(4, "PNG publication failed");
        let capture_snapshot = ledger.snapshot();
        let runtime_error = Some("control command: stale sequence 8; cursor is 9".to_owned());

        let (last_error, last_screenshot_error) =
            agent_status_error_projection(&runtime_error, &capture_snapshot);

        assert_eq!(last_error, runtime_error);
        assert!(last_screenshot_error
            .as_deref()
            .is_some_and(|error| error.contains("capture 0004 failed")));
    }

    #[test]
    fn mission_feed_counts_runtime_and_screenshot_errors_independently() {
        let runtime_error = Some("control command: stale sequence".to_owned());
        let screenshot_error = Some("screenshot: capture 0004 failed".to_owned());

        assert_eq!(
            mission_feed_error_count(2, &runtime_error, &screenshot_error),
            4
        );
        assert_eq!(
            mission_feed_error_count(usize::MAX, &runtime_error, &screenshot_error),
            usize::MAX,
            "optional mission telemetry must not wrap its aggregate"
        );
    }

    #[test]
    fn visual_state_separates_selection_from_status_activity() {
        assert_eq!(
            agent_control_visual_state(true, true, false),
            AgentControlVisualState {
                selected: true,
                status_active: true,
            }
        );
        assert_eq!(
            agent_control_visual_state(true, true, true),
            AgentControlVisualState {
                selected: true,
                status_active: false,
            }
        );
        assert_eq!(
            agent_control_visual_state(true, false, false),
            AgentControlVisualState {
                selected: false,
                status_active: false,
            }
        );
        assert_eq!(
            agent_control_visual_state(false, true, false),
            AgentControlVisualState {
                selected: false,
                status_active: false,
            }
        );
    }

    #[test]
    fn shared_panel_motion_preserves_geometry_for_every_motion_profile() {
        let full_idle = render_agent_panel_size(false, false, false);
        let full_selected = render_agent_panel_size(false, false, true);
        let low_spec_selected = render_agent_panel_size(false, true, true);
        let reduced_selected = render_agent_panel_size(true, false, true);

        assert_eq!(full_idle, full_selected);
        assert_eq!(full_idle, low_spec_selected);
        assert_eq!(full_idle, reduced_selected);
        assert_eq!(full_idle.x, 280.0);
        assert!(full_idle.y > 28.0);
    }

    #[test]
    fn status_signal_is_fixed_size_and_only_loops_with_full_motion() {
        let full_idle = render_agent_status_signal(false, false, false);
        let full_active = render_agent_status_signal(false, false, true);
        let low_spec_active = render_agent_status_signal(false, true, true);
        let reduced_active = render_agent_status_signal(true, false, true);
        let expected_size = egui::Vec2::splat(crate::theme::KANSO_LAYOUT.signal_reactor_size);

        assert_eq!(full_idle.0, expected_size);
        assert_eq!(full_active.0, expected_size);
        assert_eq!(low_spec_active.0, expected_size);
        assert_eq!(reduced_active.0, expected_size);
        assert_eq!(full_idle.1, std::time::Duration::MAX);
        assert!(full_active.1 < std::time::Duration::MAX);
        assert_eq!(low_spec_active.1, std::time::Duration::MAX);
        assert_eq!(reduced_active.1, std::time::Duration::MAX);
    }

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
            ..default()
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
    fn ui_control_sequence_reaches_max_then_fails_closed_without_wraparound() {
        let mut state = AgentControlState {
            runtime_enabled: true,
            enabled: false,
            sequence: u64::MAX - 1,
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
        assert!(!set_agent_control_enabled(&mut state, false));
        assert!(state.enabled);
        assert_eq!(state.sequence, u64::MAX);
    }

    #[test]
    fn live_link_suppression_setter_only_reports_real_state_changes() {
        let mut state = AgentControlState::default();

        assert!(!state.live_link_suppressed());
        assert!(!state.set_live_link_suppressed(false));
        assert!(state.set_live_link_suppressed(true));
        assert!(state.live_link_suppressed());
        assert!(!state.set_live_link_suppressed(true));
        assert!(state.set_live_link_suppressed(false));
        assert!(!state.live_link_suppressed());
    }

    #[test]
    fn input_polling_only_clears_errors_that_it_owns() {
        let mut error = Some("unknown build tool 'warp'".to_string());
        update_agent_input_error(&mut error, Vec::new());
        assert_eq!(error.as_deref(), Some("unknown build tool 'warp'"));

        update_agent_input_error(&mut error, vec!["unknown key 'WARP'".to_string()]);
        assert_eq!(error.as_deref(), Some("input: unknown key 'WARP'"));

        update_agent_input_error(&mut error, Vec::new());
        assert_eq!(error, None);
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

    #[test]
    fn live_overlay_reserves_the_toolbox_and_compacts_on_small_viewports() {
        let desktop = agent_overlay_layout(egui::vec2(1_920.0, 1_080.0));
        assert_eq!(desktop.left, AGENT_OVERLAY_TOOLBOX_CLEARANCE);
        assert_eq!(desktop.width, AGENT_OVERLAY_PANEL_WIDTH);
        assert!(!desktop.compact);

        let narrow = agent_overlay_layout(egui::vec2(320.0, 480.0));
        assert_eq!(narrow.left, AGENT_OVERLAY_TOOLBOX_CLEARANCE);
        assert_eq!(narrow.width, 220.0);
        assert!(narrow.compact);
        assert!(narrow.left + narrow.width <= 320.0);

        let tiny = agent_overlay_layout(egui::vec2(64.0, 200.0));
        assert_eq!(tiny.left, 63.0);
        assert_eq!(tiny.width, 1.0);
        assert!(tiny.compact);

        let ultrawide = agent_overlay_layout(egui::vec2(3_440.0, 1_440.0));
        assert_eq!(ultrawide.left, AGENT_OVERLAY_TOOLBOX_CLEARANCE);
        assert_eq!(ultrawide.width, AGENT_OVERLAY_PANEL_WIDTH);
        assert!(!ultrawide.compact);
    }

    #[test]
    fn semantic_build_parser_reaches_every_modern_editor_tool() {
        use crate::sketch_model::EditorToolId;

        let canonical = [
            ("select", EditorToolId::Select),
            ("pencil", EditorToolId::Pencil),
            ("rectangle", EditorToolId::Rectangle),
            ("circle", EditorToolId::Circle),
            ("polygon", EditorToolId::Polygon),
            ("arc", EditorToolId::Arc),
            ("freehand", EditorToolId::Freehand),
            ("house", EditorToolId::House),
            ("pushpull", EditorToolId::PushPull),
            ("move", EditorToolId::Move),
            ("scale", EditorToolId::Scale),
            ("rotate", EditorToolId::Rotate),
            ("room", EditorToolId::Room),
            ("opening", EditorToolId::CutOpening),
            ("road", EditorToolId::Road),
            ("botarea", EditorToolId::BotArea),
            ("material", EditorToolId::Material),
        ];
        let reached: std::collections::BTreeSet<_> = canonical
            .into_iter()
            .map(|(name, expected)| {
                let selection = parse_agent_build_selection(name).expect(name);
                assert_eq!(selection.editor_tool(), expected, "alias {name}");
                selection.editor_tool()
            })
            .collect();

        assert_eq!(reached.len(), 17, "every EditorToolId needs a QA route");
        assert_eq!(parse_agent_build_selection("unknown-tool"), None);
    }

    #[test]
    fn semantic_build_parser_keeps_legacy_builder_routes() {
        assert_eq!(
            parse_agent_build_selection("place"),
            Some(ToolboxSelection::Tool(ToolbeltTool::BrushPlace))
        );
        assert_eq!(
            parse_agent_build_selection("cut"),
            Some(ToolboxSelection::Tool(ToolbeltTool::BrushCut))
        );
        assert_eq!(
            parse_agent_build_selection("facade"),
            Some(ToolboxSelection::Tool(ToolbeltTool::CityFacade))
        );
        assert_eq!(
            parse_agent_build_selection("animation"),
            Some(ToolboxSelection::Tool(ToolbeltTool::AnimationPick))
        );
    }

    #[test]
    fn legacy_agent_control_files_remain_compatible_without_bot_commands() {
        let command: AgentControlCommand =
            ron::from_str("(enabled: true, sequence: 5)").expect("legacy control file");

        assert!(command.enabled);
        assert_eq!(command.sequence, 5);
        assert!(command.view_label.is_empty());
        assert!(command.camera_pose.is_none());
        assert!(command.bot_command.is_none());
    }

    #[test]
    fn global_control_cursor_rejects_stale_exit_and_same_sequence_mutation() {
        let accepted = "(enabled:true,sequence:10,exit:false)";
        let identical = accepted;
        let reused = "(enabled:true,sequence:10,exit:true)";
        let stale_exit = "(enabled:true,sequence:9,exit:true)";

        assert_eq!(
            agent_control_payload_decision(10, accepted, None, None),
            AgentControlPayloadDecision::Apply
        );
        assert_eq!(
            agent_control_payload_decision(10, identical, Some(10), Some(accepted)),
            AgentControlPayloadDecision::Identical,
            "an identical poll is a true no-op"
        );
        assert_eq!(
            agent_control_payload_decision(10, reused, Some(10), Some(accepted)),
            AgentControlPayloadDecision::ReusedWithDifferentPayload,
            "exit cannot be armed by changing an accepted sequence"
        );
        assert_eq!(
            agent_control_payload_decision(9, stale_exit, Some(10), Some(accepted)),
            AgentControlPayloadDecision::Stale,
            "a stale exit command cannot apply"
        );
        assert_eq!(
            agent_control_payload_decision(
                u64::MAX,
                "(sequence:18446744073709551615)",
                Some(10),
                Some(accepted)
            ),
            AgentControlPayloadDecision::Apply
        );
        assert_eq!(
            agent_control_payload_decision(0, "(sequence:0)", Some(u64::MAX), None),
            AgentControlPayloadDecision::Stale,
            "wraparound remains fail-closed for every command action"
        );
    }

    #[test]
    fn accepted_repeat_clears_transient_boundary_error_without_reapplication() {
        let payload = "(enabled:true,sequence:10)";
        assert!(repeated_payload_matches_accepted_identity(
            payload,
            Some(payload)
        ));
        assert!(!repeated_payload_matches_accepted_identity(
            payload,
            Some("(enabled:false,sequence:10)")
        ));

        let mut error = Some("control file: transient sharing violation".to_string());
        if repeated_payload_matches_accepted_identity(payload, Some(payload)) {
            clear_owned_control_boundary_error(&mut error);
        }
        assert_eq!(error, None);
        assert_eq!(
            agent_control_payload_decision(10, payload, Some(10), Some(payload)),
            AgentControlPayloadDecision::Identical
        );
    }

    #[test]
    fn bridge_heartbeat_is_capped_at_one_hertz_and_ignores_non_finite_time() {
        let mut timer = AGENT_BRIDGE_HEARTBEAT_SECONDS;
        for _ in 0..3 {
            assert!(!advance_agent_bridge_heartbeat(&mut timer, 0.25));
        }
        assert!(advance_agent_bridge_heartbeat(&mut timer, 0.25));
        assert_eq!(timer, AGENT_BRIDGE_HEARTBEAT_SECONDS);
        assert!(!advance_agent_bridge_heartbeat(&mut timer, f32::NAN));
        assert_eq!(timer, AGENT_BRIDGE_HEARTBEAT_SECONDS);
        assert!(advance_agent_bridge_heartbeat(&mut timer, 99.0));
        assert_eq!(timer, AGENT_BRIDGE_HEARTBEAT_SECONDS);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn oversized_control_payload_is_rejected_before_ron_deserialization() {
        let error = decode_bounded_control_bytes(vec![b' '; AGENT_CONTROL_MAX_BYTES + 1])
            .expect_err("oversized control payload");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("byte limit"));

        let exact = decode_bounded_control_bytes(vec![b' '; AGENT_CONTROL_MAX_BYTES])
            .expect("exact byte ceiling remains readable");
        assert_eq!(exact.len(), AGENT_CONTROL_MAX_BYTES);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn relative_agent_paths_are_rooted_before_safety_validation() {
        let root = AgentPathTestRoot::new("relative-root");
        let relative = std::path::Path::new("agent_runs")
            .join("session")
            .join("status.ron");
        let rooted = rooted_agent_path_from(&relative, root.path())
            .expect("absolute test root must resolve a relative endpoint");

        assert!(rooted.is_absolute());
        assert_eq!(rooted, root.path().join(relative));
        assert_eq!(
            rooted_agent_path_from(
                std::path::Path::new("status.ron"),
                std::path::Path::new("relative-cwd")
            )
            .expect_err("a relative current directory must fail closed")
            .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn safe_agent_paths_support_normal_nested_session_io() {
        let root = AgentPathTestRoot::new("normal-chain");
        let session = root.path().join("agent_runs").join("session");
        prepare_safe_agent_directory(&session).expect("prepare ordinary session chain");

        let control = session.join("agent_control.ron");
        assert!(
            create_agent_control_file_if_missing(&control, "(enabled:true,sequence:1)")
                .expect("write ordinary control fixture")
        );
        assert_eq!(
            read_bounded_control_file(&control).expect("read ordinary control path"),
            "(enabled:true,sequence:1)"
        );
        atomic_write_agent_text(&control, "(enabled:false,sequence:2)")
            .expect("durably replace ordinary control fixture");
        assert_eq!(
            read_bounded_control_file(&control).expect("read replaced control path"),
            "(enabled:false,sequence:2)"
        );

        let status = session.join("status.ron");
        atomic_replace_agent_telemetry_text(&status, "(status:\"ready\")")
            .expect("write ordinary telemetry path");
        assert_eq!(
            std::fs::read_to_string(status).expect("read ordinary telemetry fixture"),
            "(status:\"ready\")"
        );

        let capability_path =
            write_agent_capability_manifest_safely(&session, "observer-fixture", "fleet-fixture")
                .expect("write ordinary capability manifest");
        let capability_text =
            std::fs::read_to_string(capability_path).expect("read ordinary capability manifest");
        assert!(capability_text.contains("observer-fixture"));

        let screenshot = session.join("live_0000.png");
        let png = tiny_agent_png();
        write_agent_screenshot_png(&screenshot, &png).expect("write ordinary agent screenshot");
        assert_eq!(
            std::fs::read(screenshot).expect("read ordinary agent screenshot"),
            png
        );
    }

    #[cfg(all(not(target_arch = "wasm32"), any(unix, windows)))]
    #[test]
    fn agent_io_rejects_a_symlink_or_junction_ancestor() {
        let root = AgentPathTestRoot::new("linked-ancestor");
        let target = root.path().join("outside");
        let linked = root.path().join("agent_runs");
        std::fs::create_dir(&target).expect("create linked target fixture");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &linked).expect("create directory symlink fixture");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&target, &linked)
            .expect("create directory symlink fixture");

        let escaped_session = linked.join("session");
        let prepare_error = prepare_safe_agent_directory(&escaped_session)
            .expect_err("linked ancestor must block session preparation");
        assert_eq!(prepare_error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!target.join("session").exists());

        std::fs::create_dir(target.join("existing")).expect("create target session fixture");
        let relative_from_linked_cwd = rooted_agent_path_from(
            &std::path::Path::new("existing").join("relative_status.ron"),
            &linked,
        )
        .expect("relative endpoint must become absolute before validation");
        assert_eq!(
            atomic_replace_agent_telemetry_text(
                &relative_from_linked_cwd,
                "(status:\"unsafe-relative\")"
            )
            .expect_err("a junction in the supplied current-directory chain must be rejected")
            .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(!target.join("existing").join("relative_status.ron").exists());

        let escaped_control = linked.join("existing").join("agent_control.ron");
        std::fs::write(
            target.join("existing").join("agent_control.ron"),
            "(sequence:1)",
        )
        .expect("write target control fixture");
        assert_eq!(
            read_bounded_control_file(&escaped_control)
                .expect_err("linked ancestor must block control reads")
                .kind(),
            std::io::ErrorKind::InvalidInput
        );

        let escaped_status = linked.join("existing").join("status.ron");
        assert_eq!(
            atomic_replace_agent_telemetry_text(&escaped_status, "(status:\"unsafe\")")
                .expect_err("linked ancestor must block telemetry writes")
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(!target.join("existing").join("status.ron").exists());

        let escaped_capability_session = linked.join("existing");
        assert_eq!(
            write_agent_capability_manifest_safely(
                &escaped_capability_session,
                "observer-fixture",
                "fleet-fixture"
            )
            .expect_err("linked ancestor must block capability writes")
            .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(!target.join("existing").join("capabilities.ron").exists());

        let escaped_screenshot = linked.join("existing").join("live_0000.png");
        assert_eq!(
            write_agent_screenshot_png(&escaped_screenshot, &tiny_agent_png())
                .expect_err("linked ancestor must block screenshot writes")
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(!target.join("existing").join("live_0000.png").exists());

        let escaped_new_control = linked.join("existing").join("new_control.ron");
        assert_eq!(
            create_agent_control_file_if_missing(&escaped_new_control, "(sequence:0)")
                .expect_err("linked ancestor must block control creation")
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(!target.join("existing").join("new_control.ron").exists());
    }

    #[cfg(all(not(target_arch = "wasm32"), any(unix, windows)))]
    #[test]
    fn agent_output_endpoints_reject_symlink_or_reparse_leaves() {
        let root = AgentPathTestRoot::new("linked-leaves");
        let session = root.path().join("agent_runs").join("session");
        let targets = root.path().join("outside");
        prepare_safe_agent_directory(&session).expect("prepare safe session fixture");
        std::fs::create_dir(&targets).expect("create linked-leaf target directory");

        let cases = [
            ("agent_control.ron", b"original-control".as_slice()),
            ("capabilities.ron", b"original-capability".as_slice()),
            ("live_0000.png", b"original-screenshot".as_slice()),
            ("status.ron", b"original-status".as_slice()),
        ];
        for (name, original) in cases {
            let target = targets.join(name);
            let linked = session.join(name);
            std::fs::write(&target, original).expect("write linked-leaf target fixture");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &linked).expect("create file symlink fixture");
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(&target, &linked)
                .expect("create file symlink fixture");

            let error = match name {
                "agent_control.ron" => {
                    let create_error =
                        create_agent_control_file_if_missing(&linked, "(sequence:0)")
                            .expect_err("control creation must reject linked leaf");
                    assert_eq!(create_error.kind(), std::io::ErrorKind::InvalidInput);
                    atomic_write_agent_text(&linked, "(sequence:1)")
                        .expect_err("control replacement must reject linked leaf")
                }
                "capabilities.ron" => write_agent_capability_manifest_safely(
                    &session,
                    "observer-fixture",
                    "fleet-fixture",
                )
                .expect_err("capability write must reject linked leaf"),
                "live_0000.png" => write_agent_screenshot_png(&linked, &tiny_agent_png())
                    .expect_err("screenshot write must reject linked leaf"),
                "status.ron" => atomic_replace_agent_telemetry_text(&linked, "(status:\"unsafe\")")
                    .expect_err("telemetry replacement must reject linked leaf"),
                _ => unreachable!(),
            };
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert_eq!(
                std::fs::read(&target).expect("read preserved linked-leaf target"),
                original
            );
            std::fs::remove_file(&linked).expect("remove linked-leaf fixture");
        }
    }

    #[test]
    fn command_vectors_and_strings_are_deduplicated_and_bounded_before_use() {
        let mut command = AgentControlCommand {
            keys: (0..(AGENT_CONTROL_MAX_INPUT_NAMES + 20))
                .flat_map(|index| [format!(" key_{index} "), format!("KEY-{index}")])
                .collect(),
            mouse_buttons: vec![
                "left".into(),
                "MOUSE_LEFT".into(),
                "right".into(),
                "right".into(),
            ],
            game_state: "g".repeat(AGENT_CONTROL_MAX_COMMAND_CHARS + 20),
            build_mode: "b".repeat(AGENT_CONTROL_MAX_COMMAND_CHARS + 20),
            build_tool: "t".repeat(AGENT_CONTROL_MAX_COMMAND_CHARS + 20),
            bot_command: Some(AgentBotCommandRequest {
                action: "a".repeat(AGENT_CONTROL_MAX_COMMAND_CHARS + 20),
                operation: "o".repeat(AGENT_CONTROL_MAX_COMMAND_CHARS + 20),
                block_reason: "r".repeat(AGENT_CONTROL_MAX_COMMAND_CHARS + 20),
                target: AgentBotTargetRequest::Path(vec![[1, 2, 3], [1, 2, 3], [4, 5, 6]]),
                recipients: AgentBotRecipientsRequest::Selected(vec![7, 7, 9]),
                ..default()
            }),
            ..default()
        };

        sanitize_agent_control_command(&mut command).expect("bounded command");

        assert!(command.keys.len() <= AGENT_CONTROL_MAX_INPUT_NAMES);
        let key_identities = command
            .keys
            .iter()
            .map(|key| normalized_input_name(key))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(key_identities.len(), command.keys.len());
        assert!(command
            .keys
            .iter()
            .all(|key| key.chars().count() <= AGENT_CONTROL_MAX_INPUT_NAME_CHARS));
        assert_eq!(command.mouse_buttons, vec!["left", "right"]);
        assert_eq!(
            command.game_state.chars().count(),
            AGENT_CONTROL_MAX_COMMAND_CHARS
        );
        assert_eq!(
            command.build_mode.chars().count(),
            AGENT_CONTROL_MAX_COMMAND_CHARS
        );
        assert_eq!(
            command.build_tool.chars().count(),
            AGENT_CONTROL_MAX_COMMAND_CHARS
        );

        let bot = command.bot_command.expect("bot command");
        assert_eq!(bot.action.chars().count(), AGENT_CONTROL_MAX_COMMAND_CHARS);
        assert_eq!(
            bot.operation.chars().count(),
            AGENT_CONTROL_MAX_COMMAND_CHARS
        );
        assert_eq!(
            bot.block_reason.chars().count(),
            AGENT_CONTROL_MAX_COMMAND_CHARS
        );
        assert_eq!(
            bot.target,
            AgentBotTargetRequest::Path(vec![[1, 2, 3], [4, 5, 6]])
        );
        assert_eq!(
            bot.recipients,
            AgentBotRecipientsRequest::Selected(vec![7, 9])
        );

        let mut oversized = AgentControlCommand {
            bot_command: Some(AgentBotCommandRequest {
                target: AgentBotTargetRequest::Selection(vec![
                    [0, 0, 0];
                    AGENT_CONTROL_MAX_BOT_POINTS + 1
                ]),
                ..default()
            }),
            ..default()
        };
        assert!(sanitize_agent_control_command(&mut oversized)
            .expect_err("oversized bot selection")
            .contains("limit"));
    }

    #[test]
    fn non_finite_legacy_command_floats_are_sanitized_at_the_boundary() {
        let command: AgentControlCommand = ron::from_str(
            r#"(
                enabled: true,
                sequence: 9,
                forward: NaN,
                right: inf,
                up: -inf,
                look_x: inf,
                look_y: NaN,
                yaw: Some(-inf),
                pitch: Some(NaN),
            )"#,
        )
        .expect("RON 0.8 accepts explicit non-finite floats");
        let mut state = AgentControlState::default();

        apply_command(&mut state, command);

        assert_eq!((state.forward, state.right, state.up), (0.0, 0.0, 0.0));
        assert_eq!((state.look_x, state.look_y), (0.0, 0.0));
        assert_eq!(state.yaw, None);
        assert_eq!(state.pitch, None);
        assert!(state.forward.is_finite());
        assert!(state.right.is_finite());
        assert!(state.up.is_finite());
        assert!(state.look_x.is_finite());
        assert!(state.look_y.is_finite());
    }

    #[test]
    fn finite_legacy_command_floats_keep_their_existing_clamp_contract() {
        let mut state = AgentControlState::default();
        apply_command(
            &mut state,
            AgentControlCommand {
                forward: 2.0,
                right: -2.0,
                up: 0.25,
                look_x: 8.0,
                look_y: -8.0,
                yaw: Some(12.0),
                pitch: Some(2.0),
                ..default()
            },
        );

        assert_eq!((state.forward, state.right, state.up), (1.0, -1.0, 0.25));
        assert_eq!((state.look_x, state.look_y), (4.0, -4.0));
        assert_eq!(state.yaw, Some(12.0));
        assert_eq!(state.pitch, Some(1.54));
    }

    #[test]
    fn camera_pose_command_remains_ron_serializable_with_tuple_position() {
        let command: AgentControlCommand = ron::from_str(
            r#"(
                enabled: true,
                sequence: 6,
                view_label: "Near/far seam",
                camera_pose: Some((
                    position: (12.5, 96.0, -24.25),
                    yaw: 0.5,
                    pitch: -0.25,
                )),
            )"#,
        )
        .expect("observer camera command");

        assert_eq!(command.view_label, "Near/far seam");
        assert_eq!(
            command.camera_pose,
            Some(AgentCameraPoseRequest {
                position: [12.5, 96.0, -24.25],
                yaw: 0.5,
                pitch: -0.25,
            })
        );
    }

    #[test]
    fn view_labels_are_single_line_control_free_and_character_bounded() {
        assert_eq!(
            sanitize_view_label("  Near\n/ far\t seam\u{0000} inspection  "),
            "Near / far seam inspection"
        );
        assert_eq!(sanitize_view_label("\r\n\t"), "");

        let multibyte = sanitize_view_label(&"é".repeat(AGENT_VIEW_LABEL_MAX_CHARS + 8));
        assert_eq!(multibyte.chars().count(), AGENT_VIEW_LABEL_MAX_CHARS);
        assert!(multibyte.chars().all(|character| character == 'é'));
    }

    #[test]
    fn observer_camera_validation_rejects_non_finite_and_out_of_range_values() {
        let valid = AgentCameraPoseRequest {
            position: [
                AGENT_CAMERA_SAFE_COORDINATE_LIMIT,
                AGENT_CAMERA_SAFE_Y_LIMIT,
                -AGENT_CAMERA_SAFE_COORDINATE_LIMIT,
            ],
            yaw: 7.0,
            pitch: AGENT_CAMERA_PITCH_LIMIT,
        };
        let pose = validate_agent_camera_pose(valid).expect("inclusive safety boundaries");
        assert_eq!(pose.position, Vec3::from_array(valid.position));
        assert!(pose.yaw >= -std::f32::consts::PI && pose.yaw < std::f32::consts::PI);
        assert_eq!(pose.pitch, AGENT_CAMERA_PITCH_LIMIT);

        for invalid in [
            AgentCameraPoseRequest {
                position: [f32::NAN, 0.0, 0.0],
                ..valid
            },
            AgentCameraPoseRequest {
                position: [AGENT_CAMERA_SAFE_COORDINATE_LIMIT + 2.0, 0.0, 0.0],
                ..valid
            },
            AgentCameraPoseRequest {
                position: [0.0, 0.0, -AGENT_CAMERA_SAFE_COORDINATE_LIMIT - 2.0],
                ..valid
            },
            AgentCameraPoseRequest {
                position: [0.0, AGENT_CAMERA_SAFE_Y_LIMIT + 1.0, 0.0],
                ..valid
            },
            AgentCameraPoseRequest {
                yaw: f32::INFINITY,
                ..valid
            },
            AgentCameraPoseRequest {
                pitch: AGENT_CAMERA_PITCH_LIMIT + 0.01,
                ..valid
            },
        ] {
            assert!(validate_agent_camera_pose(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn observer_camera_application_sets_an_exact_stable_flying_pose() {
        let pose = validate_agent_camera_pose(AgentCameraPoseRequest {
            position: [124.5, 88.0, -450.25],
            yaw: 0.75,
            pitch: -0.35,
        })
        .expect("valid observer pose");
        let mut transform = Transform::from_translation(Vec3::splat(4.0));
        let mut player = test_player();

        apply_validated_agent_camera_pose(pose, &mut transform, &mut player);

        assert_eq!(transform.translation, pose.position);
        let expected_rotation =
            Quat::from_axis_angle(Vec3::Y, pose.yaw) * Quat::from_axis_angle(Vec3::X, pose.pitch);
        assert!((transform.rotation.dot(expected_rotation).abs() - 1.0).abs() < 1.0e-6);
        assert_eq!(player.yaw, pose.yaw);
        assert_eq!(player.pitch, pose.pitch);
        assert_eq!(player.velocity, Vec3::ZERO);
        assert!(!player.on_ground);
        assert!(player.flying);
        assert!(player.placed_on_surface);
    }

    #[test]
    fn observer_camera_pose_sequence_is_strictly_monotonic_and_fail_closed() {
        let mut state = AgentControlState {
            runtime_enabled: true,
            enabled: true,
            sequence: 41,
            camera_pose: Some(AgentCameraPoseRequest::default()),
            ..default()
        };

        assert!(agent_camera_pose_pending(&state, &GameState::InGame, None));
        assert!(!agent_camera_pose_pending(
            &state,
            &GameState::InGame,
            Some(41)
        ));
        state.camera_pose = Some(AgentCameraPoseRequest {
            position: [900.0, 300.0, -700.0],
            ..default()
        });
        assert!(
            !agent_camera_pose_pending(&state, &GameState::InGame, Some(41)),
            "a changed payload cannot reuse an already handled sequence"
        );
        assert!(!agent_camera_pose_pending(&state, &GameState::Paused, None));

        state.sequence = 40;
        assert!(
            !agent_camera_pose_pending(&state, &GameState::InGame, Some(41)),
            "a stale sequence cannot replay a teleport"
        );
        state.sequence = 42;
        assert!(agent_camera_pose_pending(
            &state,
            &GameState::InGame,
            Some(41)
        ));
        state.sequence = u64::MAX;
        assert!(agent_camera_pose_pending(
            &state,
            &GameState::InGame,
            Some(u64::MAX - 1)
        ));
        assert!(
            !agent_camera_pose_pending(&state, &GameState::InGame, Some(u64::MAX)),
            "the sequence cursor must fail closed after u64::MAX"
        );
        state.sequence = 0;
        assert!(
            !agent_camera_pose_pending(&state, &GameState::InGame, Some(u64::MAX)),
            "overflow-style wraparound cannot reopen the cursor"
        );
        state.sequence = 42;
        state.enabled = false;
        assert!(!agent_camera_pose_pending(
            &state,
            &GameState::InGame,
            Some(41)
        ));
    }

    #[test]
    fn camera_command_cursor_consumes_pose_less_sequences_and_rejects_identity_reuse() {
        let mut last_sequence = None;
        let mut last_pose = None;
        let original = AgentCameraPoseRequest::default();
        let changed = AgentCameraPoseRequest {
            position: [10.0, 20.0, 30.0],
            ..original
        };

        assert_eq!(
            observe_agent_camera_command(41, None, &mut last_sequence, &mut last_pose),
            AgentCameraCommandObservation::Fresh
        );
        assert_eq!(last_sequence, Some(41));
        assert_eq!(last_pose, None);
        assert_eq!(
            observe_agent_camera_command(41, Some(original), &mut last_sequence, &mut last_pose),
            AgentCameraCommandObservation::ReusedWithDifferentPose,
            "a sequence first observed without a pose cannot gain one later"
        );
        assert_eq!(
            observe_agent_camera_command(40, Some(original), &mut last_sequence, &mut last_pose),
            AgentCameraCommandObservation::Stale
        );
        assert_eq!(
            observe_agent_camera_command(42, Some(original), &mut last_sequence, &mut last_pose),
            AgentCameraCommandObservation::Fresh
        );
        assert_eq!(
            observe_agent_camera_command(42, Some(original), &mut last_sequence, &mut last_pose),
            AgentCameraCommandObservation::Repeat
        );
        assert_eq!(
            observe_agent_camera_command(42, Some(changed), &mut last_sequence, &mut last_pose),
            AgentCameraCommandObservation::ReusedWithDifferentPose
        );
        assert_eq!(
            observe_agent_camera_command(
                u64::MAX,
                Some(changed),
                &mut last_sequence,
                &mut last_pose
            ),
            AgentCameraCommandObservation::Fresh
        );
        assert_eq!(
            observe_agent_camera_command(0, None, &mut last_sequence, &mut last_pose),
            AgentCameraCommandObservation::Stale,
            "sequence wraparound remains fail-closed"
        );
    }

    #[test]
    fn isolated_observer_requires_both_runtime_and_explicit_isolation() {
        assert!(!isolated_observer_from_flags(false, false));
        assert!(!isolated_observer_from_flags(true, false));
        assert!(!isolated_observer_from_flags(false, true));
        assert!(isolated_observer_from_flags(true, true));
    }

    #[test]
    fn bridge_contract_has_a_stable_marker_and_explicit_isolation_truth_table() {
        assert_eq!(LIVE_OBSERVER_PROTOCOL_VERSION, "live-observer-v1");
        for (runtime_enabled, isolated_requested, expected_isolated) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (true, true, true),
        ] {
            let contract = observer_bridge_contract(runtime_enabled, isolated_requested);
            assert_eq!(contract.protocol_version, "live-observer-v1");
            assert_eq!(contract.isolated_observer, expected_isolated);
        }
    }

    #[test]
    fn isolated_observer_requires_a_fresh_world_and_external_control_authority() {
        assert!(agent_world_entry_allows_existing_exact(false));
        assert!(!agent_world_entry_allows_existing_exact(true));
        assert!(agent_control_file_rewrite_allowed(false));
        assert!(!agent_control_file_rewrite_allowed(true));
    }

    #[test]
    fn observer_scenery_parser_accepts_public_and_ui_tier_names() {
        for (name, expected) in [
            ("off", SceneryQuality::Off),
            ("minimal", SceneryQuality::Off),
            ("lean", SceneryQuality::Lean),
            ("efficient", SceneryQuality::Lean),
            ("balanced", SceneryQuality::Balanced),
            ("lush", SceneryQuality::Lush),
            ("immersive", SceneryQuality::Lush),
        ] {
            assert_eq!(parse_agent_scenery_quality(name), Some(expected));
            assert_eq!(
                parse_agent_scenery_quality(&name.to_ascii_uppercase()),
                Some(expected)
            );
        }
        assert_eq!(parse_agent_scenery_quality("unknown"), None);
    }

    #[test]
    fn bot_preview_requests_are_idempotent_and_publish_cost_telemetry() {
        let request = AgentBotCommandRequest {
            request_id: 1,
            action: "create_preview".into(),
            operation: "road".into(),
            target: AgentBotTargetRequest::Area {
                min: [0, 0, 0],
                max: [31, 3, 31],
            },
            ..default()
        };
        let mut commands = BotCommandStateMachine::default();
        let mut last_applied = None;

        let id = expect_applied(
            process_agent_bot_request(&request, &mut commands, &mut last_applied, true)
                .expect("valid preview"),
        );
        assert_eq!(
            commands.command(id).expect("command").state(),
            crate::bot_command::CommandState::PreviewReady
        );
        assert_eq!(commands.commands().len(), 1);

        let telemetry = bot_command_telemetry(&commands, Some(id.get()));
        assert_eq!(telemetry.operation, "ROAD");
        assert_eq!(telemetry.state, "PREVIEWREADY");
        assert!(telemetry.estimated_voxel_cost.unwrap_or_default() > 0);
        assert!(telemetry.estimated_chunk_cost.unwrap_or_default() > 0);

        assert_eq!(
            process_agent_bot_request(&request, &mut commands, &mut last_applied, true)
                .expect("duplicate request is a no-op"),
            AgentBotRequestOutcome::Duplicate
        );
        assert_eq!(commands.commands().len(), 1);
        assert_eq!(
            commands.command(id).expect("same command").revision().get(),
            telemetry.revision.expect("preview revision")
        );
    }

    #[test]
    fn bot_lifecycle_requires_unique_requests_and_explicit_approval() {
        let mut commands = BotCommandStateMachine::default();
        let mut last_applied = None;
        let create = AgentBotCommandRequest {
            request_id: 10,
            action: "create_preview".into(),
            operation: "inspect".into(),
            target: AgentBotTargetRequest::Point([4, 8, 12]),
            ..default()
        };
        let id = expect_applied(
            process_agent_bot_request(&create, &mut commands, &mut last_applied, true)
                .expect("create preview"),
        );

        let execute_before_approval = AgentBotCommandRequest {
            request_id: 11,
            action: "execute".into(),
            command_id: Some(id.get()),
            ..default()
        };
        assert!(process_agent_bot_request(
            &execute_before_approval,
            &mut commands,
            &mut last_applied,
            true,
        )
        .is_err());
        assert_eq!(
            last_applied.as_ref().map(|request| request.request_id),
            Some(10)
        );
        assert_eq!(
            commands.command(id).expect("command").state(),
            crate::bot_command::CommandState::PreviewReady
        );

        let approve = AgentBotCommandRequest {
            request_id: 12,
            action: "approve".into(),
            command_id: Some(id.get()),
            ..default()
        };
        process_agent_bot_request(&approve, &mut commands, &mut last_applied, true)
            .expect("approve command");
        let execute = AgentBotCommandRequest {
            request_id: 13,
            action: "execute".into(),
            command_id: Some(id.get()),
            ..default()
        };
        process_agent_bot_request(&execute, &mut commands, &mut last_applied, true)
            .expect("execute command");

        assert_eq!(
            commands.command(id).expect("running command").state(),
            crate::bot_command::CommandState::Running
        );
        assert_eq!(
            bot_command_telemetry(&commands, Some(id.get())).state,
            "RUNNING"
        );
    }

    #[test]
    fn zero_bot_request_id_is_rejected_without_creating_work() {
        let mut commands = BotCommandStateMachine::default();
        let mut last_applied = None;

        assert!(process_agent_bot_request(
            &AgentBotCommandRequest::default(),
            &mut commands,
            &mut last_applied,
            true,
        )
        .is_err());
        assert_eq!(commands.commands().len(), 0);
        assert!(last_applied.is_none());
    }

    #[test]
    fn same_request_id_with_different_payload_is_rejected() {
        let mut commands = BotCommandStateMachine::default();
        let mut last_applied = None;
        let original = AgentBotCommandRequest {
            request_id: 10,
            operation: "inspect".into(),
            target: AgentBotTargetRequest::Point([1, 2, 3]),
            ..default()
        };
        expect_applied(
            process_agent_bot_request(&original, &mut commands, &mut last_applied, true)
                .expect("first request"),
        );

        let mut reused = original.clone();
        reused.operation = "road".into();
        let error = process_agent_bot_request(&reused, &mut commands, &mut last_applied, true)
            .expect_err("changed payload must not reuse an applied id");

        assert!(error.contains("reused with a different payload"));
        assert_eq!(commands.commands().len(), 1);
        assert_eq!(last_applied, Some(original));
    }

    #[test]
    fn stale_request_is_rejected_after_a_newer_request() {
        let mut commands = BotCommandStateMachine::default();
        let mut last_applied = None;
        let first = AgentBotCommandRequest {
            request_id: 10,
            target: AgentBotTargetRequest::Point([1, 2, 3]),
            ..default()
        };
        let second = AgentBotCommandRequest {
            request_id: 11,
            target: AgentBotTargetRequest::Point([4, 5, 6]),
            ..default()
        };
        expect_applied(
            process_agent_bot_request(&first, &mut commands, &mut last_applied, true)
                .expect("first request"),
        );
        expect_applied(
            process_agent_bot_request(&second, &mut commands, &mut last_applied, true)
                .expect("second request"),
        );

        let error = process_agent_bot_request(&first, &mut commands, &mut last_applied, true)
            .expect_err("old request must not replay");

        assert!(error.contains("stale request_id 10"));
        assert_eq!(commands.commands().len(), 2);
        assert_eq!(last_applied, Some(second));
    }

    #[test]
    fn invalid_request_does_not_advance_cursor_and_can_be_corrected() {
        let mut commands = BotCommandStateMachine::default();
        let mut last_applied = None;
        let first = AgentBotCommandRequest {
            request_id: 10,
            ..default()
        };
        expect_applied(
            process_agent_bot_request(&first, &mut commands, &mut last_applied, true)
                .expect("first request"),
        );

        let invalid = AgentBotCommandRequest {
            request_id: 11,
            operation: "not-a-tool".into(),
            ..default()
        };
        assert!(
            process_agent_bot_request(&invalid, &mut commands, &mut last_applied, true).is_err()
        );
        assert_eq!(
            last_applied.as_ref().map(|request| request.request_id),
            Some(10)
        );

        let corrected = AgentBotCommandRequest {
            operation: "inspect".into(),
            ..invalid
        };
        expect_applied(
            process_agent_bot_request(&corrected, &mut commands, &mut last_applied, true)
                .expect("corrected request"),
        );
        assert_eq!(commands.commands().len(), 2);
        assert_eq!(last_applied, Some(corrected));
    }

    #[test]
    fn paused_agent_defers_progress_without_consuming_the_request() {
        let request = AgentBotCommandRequest {
            request_id: 20,
            target: AgentBotTargetRequest::Point([3, 4, 5]),
            ..default()
        };
        let mut commands = BotCommandStateMachine::default();
        let mut last_applied = None;

        assert_eq!(
            process_agent_bot_request(&request, &mut commands, &mut last_applied, false)
                .expect("paused request is deferred"),
            AgentBotRequestOutcome::DeferredWhilePaused
        );
        assert!(last_applied.is_none());
        assert_eq!(commands.commands().len(), 0);

        expect_applied(
            process_agent_bot_request(&request, &mut commands, &mut last_applied, true)
                .expect("same request applies after resume"),
        );
        assert_eq!(
            process_agent_bot_request(&request, &mut commands, &mut last_applied, true)
                .expect("exact retry"),
            AgentBotRequestOutcome::Duplicate
        );
        assert_eq!(commands.commands().len(), 1);
    }

    #[test]
    fn paused_agent_can_cancel_running_work() {
        let mut commands = BotCommandStateMachine::default();
        let mut last_applied = None;
        let create = AgentBotCommandRequest {
            request_id: 1,
            target: AgentBotTargetRequest::Point([3, 4, 5]),
            ..default()
        };
        let id = expect_applied(
            process_agent_bot_request(&create, &mut commands, &mut last_applied, true)
                .expect("create"),
        );
        for (request_id, action) in [(2, "approve"), (3, "execute")] {
            expect_applied(
                process_agent_bot_request(
                    &AgentBotCommandRequest {
                        request_id,
                        action: action.into(),
                        command_id: Some(id.get()),
                        ..default()
                    },
                    &mut commands,
                    &mut last_applied,
                    true,
                )
                .expect(action),
            );
        }

        expect_applied(
            process_agent_bot_request(
                &AgentBotCommandRequest {
                    request_id: 4,
                    action: "cancel".into(),
                    command_id: Some(id.get()),
                    ..default()
                },
                &mut commands,
                &mut last_applied,
                false,
            )
            .expect("cancel remains available while paused"),
        );
        assert_eq!(
            commands.command(id).expect("command").state(),
            crate::bot_command::CommandState::Cancelled
        );
    }

    #[test]
    fn external_complete_is_rejected_without_consuming_its_request_id() {
        let mut commands = BotCommandStateMachine::default();
        let mut last_applied = None;
        let create = AgentBotCommandRequest {
            request_id: 40,
            target: AgentBotTargetRequest::Point([3, 4, 5]),
            ..default()
        };
        let id = expect_applied(
            process_agent_bot_request(&create, &mut commands, &mut last_applied, true)
                .expect("create"),
        );
        for (request_id, action) in [(41, "approve"), (42, "execute")] {
            expect_applied(
                process_agent_bot_request(
                    &AgentBotCommandRequest {
                        request_id,
                        action: action.into(),
                        command_id: Some(id.get()),
                        ..default()
                    },
                    &mut commands,
                    &mut last_applied,
                    true,
                )
                .expect(action),
            );
        }

        let complete = AgentBotCommandRequest {
            request_id: 43,
            action: "complete".into(),
            command_id: Some(id.get()),
            ..default()
        };
        let error = process_agent_bot_request(&complete, &mut commands, &mut last_applied, true)
            .expect_err("external completion needs executor proof");
        assert!(error.contains("executor-owned"));
        assert_eq!(
            commands.command(id).expect("command").state(),
            crate::bot_command::CommandState::Running
        );
        assert_eq!(
            last_applied.as_ref().map(|request| request.request_id),
            Some(42)
        );

        let cancel = AgentBotCommandRequest {
            action: "cancel".into(),
            ..complete
        };
        expect_applied(
            process_agent_bot_request(&cancel, &mut commands, &mut last_applied, true)
                .expect("rejected request id can be corrected"),
        );
        assert_eq!(
            commands.command(id).expect("command").state(),
            crate::bot_command::CommandState::Cancelled
        );
    }
}
