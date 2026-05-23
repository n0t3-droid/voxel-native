//! Authoritative interaction mode for the in-game UI and build tools.
//!
//! Older systems still read `ToolbeltState::live/palette_open`; this
//! module keeps those fields synchronized while moving mode decisions to
//! one resource.

use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};

use crate::animation::AnimationStudio;
use crate::builder::BuilderState;
use crate::city::{CityState, CityTool};
use crate::commands::CommandPaletteState;
use crate::editor::{EditorState, EditorTab};
use crate::menu::{GameState, PauseScreen};
use crate::player::Player;
use crate::ships::ShipKind;
use crate::toolbelt::{ToolbeltState, ToolbeltTool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveMode {
    Combat,
    BuildPicker { tool: ToolbeltTool },
    BuildLive { tool: ToolbeltTool },
    Editor { tab: EditorTab },
    Inventory,
    ShipPlacement { kind: ShipKind },
    ShipFlight { entity: Entity },
    Paused,
    CommandPalette,
}

impl Default for ActiveMode {
    fn default() -> Self {
        Self::Combat
    }
}

impl ActiveMode {
    pub fn build_tool(self) -> Option<ToolbeltTool> {
        match self {
            Self::BuildPicker { tool } | Self::BuildLive { tool } => Some(tool),
            _ => None,
        }
    }

    pub fn is_build(self) -> bool {
        self.build_tool().is_some()
    }

    pub fn is_build_picker(self) -> bool {
        matches!(self, Self::BuildPicker { .. })
    }

    pub fn is_build_live(self) -> bool {
        matches!(self, Self::BuildLive { .. })
    }

    pub fn allows_weapons(self) -> bool {
        matches!(self, Self::Combat)
    }

    pub fn is_ship(self) -> bool {
        matches!(self, Self::ShipPlacement { .. } | Self::ShipFlight { .. })
    }

    #[allow(dead_code)]
    pub fn is_ship_placement(self) -> bool {
        matches!(self, Self::ShipPlacement { .. })
    }

    pub fn is_ship_flight(self) -> bool {
        matches!(self, Self::ShipFlight { .. })
    }
}

#[derive(Resource, Debug, Clone)]
pub struct ModeContext {
    pub mode: ActiveMode,
    pub last_mode: ActiveMode,
    pub status: String,
    pub transition_hint_t: f32,
}

#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct BuildGestureLock {
    pub active: bool,
    pub owner: Option<&'static str>,
}

#[derive(Resource, Debug, Clone, Copy)]
pub struct ModeContinuityGuard {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub velocity: Vec3,
    pub flying: bool,
    pub guard_t: f32,
}

impl Default for ModeContinuityGuard {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            velocity: Vec3::ZERO,
            flying: true,
            guard_t: 0.0,
        }
    }
}

impl BuildGestureLock {
    pub fn lock(&mut self, owner: &'static str) {
        self.active = true;
        self.owner = Some(owner);
    }

    pub fn release(&mut self, owner: &'static str) {
        if self.owner == Some(owner) {
            self.active = false;
            self.owner = None;
        }
    }
}

impl Default for ModeContext {
    fn default() -> Self {
        Self {
            mode: ActiveMode::Combat,
            last_mode: ActiveMode::Combat,
            status: "Combat controls active.".into(),
            transition_hint_t: 0.0,
        }
    }
}

impl ModeContext {
    pub fn set(&mut self, next: ActiveMode, status: impl Into<String>) {
        if self.mode != next {
            self.last_mode = self.mode;
            self.mode = next;
            self.transition_hint_t = 2.2;
        }
        self.status = status.into();
    }

    pub fn build_tool(&self) -> Option<ToolbeltTool> {
        self.mode.build_tool()
    }

    pub fn is_build(&self) -> bool {
        self.mode.is_build()
    }

    pub fn is_build_picker(&self) -> bool {
        self.mode.is_build_picker()
    }

    pub fn is_build_live(&self) -> bool {
        self.mode.is_build_live()
    }

    pub fn allows_weapons(&self) -> bool {
        self.mode.allows_weapons()
    }

    pub fn is_ship(&self) -> bool {
        self.mode.is_ship()
    }

    #[allow(dead_code)]
    pub fn is_ship_placement(&self) -> bool {
        self.mode.is_ship_placement()
    }

    pub fn is_ship_flight(&self) -> bool {
        self.mode.is_ship_flight()
    }
}

pub struct ModePlugin;

impl Plugin for ModePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ModeContext::default())
            .insert_resource(BuildGestureLock::default())
            .insert_resource(ModeContinuityGuard::default())
            .add_systems(
                Update,
                (
                    reconcile_external_mode,
                    mode_hotkeys,
                    decay_mode_transition_hint,
                    sync_legacy_toolbelt,
                    sync_tool_side_effects,
                )
                    .chain(),
            )
            .add_systems(
                Last,
                (mode_cursor_guard, preserve_player_after_quick_switch).chain(),
            );
    }
}

fn decay_mode_transition_hint(time: Res<Time>, mut mode: ResMut<ModeContext>) {
    mode.transition_hint_t = (mode.transition_hint_t - time.delta_seconds()).max(0.0);
}

fn reconcile_external_mode(
    game_state: Res<State<GameState>>,
    pause_screen: Res<PauseScreen>,
    editor: Res<EditorState>,
    command_palette: Option<Res<CommandPaletteState>>,
    mut mode: ResMut<ModeContext>,
) {
    if command_palette.as_deref().map(|p| p.open).unwrap_or(false) {
        mode.set(ActiveMode::CommandPalette, "Command Deck open.");
        return;
    }

    match game_state.get() {
        GameState::MainMenu => {
            set_external_mode(&mut mode, ActiveMode::Paused, "Main menu open.");
        }
        GameState::Paused => {
            if editor.open {
                set_external_mode(
                    &mut mode,
                    ActiveMode::Editor { tab: editor.tab },
                    "Editor panel open.",
                );
            } else if *pause_screen == PauseScreen::Inventory {
                set_external_mode(&mut mode, ActiveMode::Inventory, "Inventory open.");
            } else {
                set_external_mode(&mut mode, ActiveMode::Paused, "Pause menu open.");
            }
        }
        GameState::InGame => {
            if !mode.mode.is_build() && !mode.mode.is_ship() {
                set_external_mode(&mut mode, ActiveMode::Combat, "Combat controls active.");
            }
        }
    }
}

fn set_external_mode(mode: &mut ModeContext, next: ActiveMode, status: &'static str) {
    if mode.mode != next {
        mode.set(next, status);
    }
}

fn mode_hotkeys(
    keys: Res<ButtonInput<KeyCode>>,
    game_state: Res<State<GameState>>,
    gesture_lock: Res<BuildGestureLock>,
    mut mode: ResMut<ModeContext>,
    mut toolbelt: ResMut<ToolbeltState>,
    player_q: Query<(&Transform, &Player)>,
    mut continuity: ResMut<ModeContinuityGuard>,
) {
    if *game_state.get() != GameState::InGame {
        return;
    }

    if keys.just_pressed(KeyCode::F3) || keys.just_pressed(KeyCode::F8) {
        capture_player_continuity(&player_q, &mut continuity);
        if mode.is_build() {
            mode.set(
                ActiveMode::Combat,
                "Weapons ready. Same position, same view.",
            );
        } else {
            let tool = normalized_build_tool(toolbelt.tool);
            toolbelt.tool = tool;
            mode.set(
                ActiveMode::BuildLive { tool },
                format!(
                    "Build Live: {}. F8 returns to weapons without moving you.",
                    tool.label()
                ),
            );
        }
        return;
    }

    if keys.just_pressed(KeyCode::Tab) || keys.just_pressed(KeyCode::F7) {
        let tool = normalized_build_tool(mode.build_tool().unwrap_or(toolbelt.tool));
        toolbelt.tool = tool;
        if mode.is_build_picker() {
            mode.set(
                ActiveMode::BuildLive { tool },
                format!("Build Live: {}. {}", tool.label(), tool.hint()),
            );
        } else if mode.is_build_live() {
            mode.set(
                ActiveMode::BuildPicker { tool },
                "Build Studio picker visible. Pick a tool or press Tab/F7 to hide it.",
            );
        } else {
            mode.set(
                ActiveMode::BuildPicker { tool },
                "Build Studio picker: choose a named tool, then build with LMB.",
            );
        }
        return;
    }

    if mode.is_build() {
        let mut tool = mode.build_tool().unwrap_or(toolbelt.tool);
        let mut changed = false;
        let mut force_live = false;
        if let Some(slot_tool) = quick_tool_key(&keys) {
            tool = slot_tool;
            changed = true;
            force_live = true;
        }
        if keys.just_pressed(KeyCode::KeyQ) {
            tool = tool.stepped(-1);
            changed = true;
        }
        if keys.just_pressed(KeyCode::KeyE) {
            tool = tool.stepped(1);
            changed = true;
        }
        if keys.just_pressed(KeyCode::KeyG)
            && !gesture_lock.active
            && matches!(tool, ToolbeltTool::DrawRect | ToolbeltTool::Sculpt)
        {
            tool = if tool == ToolbeltTool::DrawRect {
                ToolbeltTool::Sculpt
            } else {
                ToolbeltTool::DrawRect
            };
            changed = true;
        }
        if changed {
            tool = normalized_build_tool(tool);
            toolbelt.tool = tool;
            let next = if force_live {
                ActiveMode::BuildLive { tool }
            } else if mode.is_build_picker() {
                ActiveMode::BuildPicker { tool }
            } else {
                ActiveMode::BuildLive { tool }
            };
            let status = if force_live {
                format!(
                    "Build Live: [{}] {}. {}",
                    tool.quick_slot_label(),
                    tool.label(),
                    tool.hint()
                )
            } else if keys.just_pressed(KeyCode::KeyG)
                && matches!(tool, ToolbeltTool::DrawRect | ToolbeltTool::Sculpt)
            {
                format!(
                    "Shape swapped to {}. Alt+LMB uses {} temporarily.",
                    tool.label(),
                    if tool == ToolbeltTool::DrawRect {
                        "Push Pull"
                    } else {
                        "Rectangle Fill"
                    }
                )
            } else {
                format!("Build Live: {}. {}", tool.label(), tool.hint())
            };
            mode.set(next, status);
            return;
        }

        if keys.just_pressed(KeyCode::Escape) {
            if mode.is_build_picker() {
                mode.set(
                    ActiveMode::BuildLive { tool },
                    format!("Picker hidden. Build Live: {}.", tool.label()),
                );
            } else {
                mode.set(
                    ActiveMode::BuildLive { tool },
                    "No active build gesture to cancel. Press F3 to exit Build Studio.",
                );
            }
        }
    }
}

fn capture_player_continuity(
    player_q: &Query<(&Transform, &Player)>,
    continuity: &mut ModeContinuityGuard,
) {
    let Ok((tf, player)) = player_q.get_single() else {
        return;
    };
    continuity.position = tf.translation;
    continuity.yaw = player.yaw;
    continuity.pitch = player.pitch;
    continuity.velocity = player.velocity;
    continuity.flying = player.flying;
    continuity.guard_t = 0.45;
}

fn preserve_player_after_quick_switch(
    time: Res<Time>,
    mut continuity: ResMut<ModeContinuityGuard>,
    mut player_q: Query<(&mut Transform, &mut Player)>,
) {
    if continuity.guard_t <= 0.0 {
        return;
    }
    continuity.guard_t = (continuity.guard_t - time.delta_seconds()).max(0.0);
    let Ok((mut tf, mut player)) = player_q.get_single_mut() else {
        return;
    };
    if tf.translation.distance_squared(continuity.position) > 64.0 {
        tf.translation = continuity.position;
        player.yaw = continuity.yaw;
        player.pitch = continuity.pitch;
        player.velocity = continuity.velocity;
        player.flying = continuity.flying;
    }
}

fn quick_tool_key(keys: &ButtonInput<KeyCode>) -> Option<ToolbeltTool> {
    for (key, slot) in [
        (KeyCode::Digit1, 1),
        (KeyCode::Digit2, 2),
        (KeyCode::Digit3, 3),
        (KeyCode::Digit4, 4),
        (KeyCode::Digit5, 5),
        (KeyCode::Digit6, 6),
        (KeyCode::Digit7, 7),
        (KeyCode::Digit8, 8),
        (KeyCode::Digit9, 9),
        (KeyCode::Digit0, 0),
    ] {
        if keys.just_pressed(key) {
            return ToolbeltTool::quick_slot(slot);
        }
    }
    None
}

fn normalized_build_tool(tool: ToolbeltTool) -> ToolbeltTool {
    if tool == ToolbeltTool::Navigate {
        ToolbeltTool::DrawRect
    } else {
        tool
    }
}

fn sync_legacy_toolbelt(mut toolbelt: ResMut<ToolbeltState>, mode: Res<ModeContext>) {
    if let Some(tool) = mode.build_tool() {
        toolbelt.tool = tool;
    }

    toolbelt.live = mode.mode.is_build();
    toolbelt.palette_open = mode.mode.is_build_picker();

    if mode.is_changed() && !mode.status.is_empty() {
        toolbelt.status = mode.status.clone();
    }
}

fn sync_tool_side_effects(
    mode: Res<ModeContext>,
    mut city: ResMut<CityState>,
    mut studio: ResMut<AnimationStudio>,
    mut builder: ResMut<BuilderState>,
) {
    if matches!(mode.mode, ActiveMode::Editor { .. }) {
        return;
    }

    let active_build_tool = mode.build_tool().filter(|_| mode.is_build_live());
    let city_tool = active_build_tool.and_then(ToolbeltTool::city_tool);

    if mode.is_build() && city.tool != city_tool.unwrap_or(CityTool::None) {
        city.tool = city_tool.unwrap_or(CityTool::None);
        city.pending_road_a = None;
        city.pending_building_a = None;
    }

    studio.picking = active_build_tool == Some(ToolbeltTool::AnimationPick);

    if active_build_tool == Some(ToolbeltTool::BrushCut) && builder.brush == IVec3::ONE {
        builder.brush = IVec3::new(2, 3, 1);
    }
}

fn mode_cursor_guard(mode: Res<ModeContext>, mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    let Ok(mut window) = windows.get_single_mut() else {
        return;
    };

    match mode.mode {
        ActiveMode::BuildPicker { .. }
        | ActiveMode::Editor { .. }
        | ActiveMode::Inventory
        | ActiveMode::Paused
        | ActiveMode::CommandPalette => {
            window.cursor.grab_mode = CursorGrabMode::None;
            window.cursor.visible = true;
        }
        ActiveMode::BuildLive { .. } => {
            window.cursor.grab_mode = CursorGrabMode::Locked;
            window.cursor.visible = false;
        }
        ActiveMode::ShipPlacement { .. } | ActiveMode::ShipFlight { .. } => {
            window.cursor.grab_mode = CursorGrabMode::Locked;
            window.cursor.visible = false;
        }
        ActiveMode::Combat => {}
    }
}
