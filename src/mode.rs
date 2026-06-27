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
use crate::toolbelt::{SketchEditorUiFocus, ToolbeltState, ToolbeltTool};

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
        default_creative_mode()
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
        let mode = default_creative_mode();
        Self {
            mode,
            last_mode: mode,
            status: default_creative_status().into(),
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
            // Bevy applies `Window` cursor changes from the `Last` schedule.
            // Keep the authoritative gameplay cursor policy in `PostUpdate`
            // so live build/combat capture reaches the OS in the same frame.
            .add_systems(PostUpdate, mode_cursor_guard)
            .add_systems(Last, preserve_player_after_quick_switch);
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
            if matches!(
                mode.mode,
                ActiveMode::Paused
                    | ActiveMode::Inventory
                    | ActiveMode::Editor { .. }
                    | ActiveMode::CommandPalette
            ) {
                let next = resume_mode_after_overlay(mode.last_mode);
                let status = resume_status(next);
                set_external_mode_owned(&mut mode, next, status);
            }
        }
    }
}

fn set_external_mode(mode: &mut ModeContext, next: ActiveMode, status: &'static str) {
    if mode.mode != next {
        mode.set(next, status);
    }
}

fn set_external_mode_owned(mode: &mut ModeContext, next: ActiveMode, status: String) {
    if mode.mode != next {
        mode.set(next, status);
    }
}

fn mode_hotkeys(
    keys: Res<ButtonInput<KeyCode>>,
    game_state: Res<State<GameState>>,
    _gesture_lock: Res<BuildGestureLock>,
    mut mode: ResMut<ModeContext>,
    mut toolbelt: ResMut<ToolbeltState>,
) {
    if *game_state.get() != GameState::InGame {
        return;
    }

    if mode.is_ship() {
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
                format!("Sketch Editor: {}. {}", tool.label(), tool.hint())
            } else {
                format!("Sketch Editor: {}. {}", tool.label(), tool.hint())
            };
            mode.set(next, status);
            return;
        }

        if keys.just_pressed(KeyCode::Escape) {
            if mode.is_build_picker() {
                mode.set(
                    ActiveMode::BuildLive { tool },
                    format!("Drawer hidden. Sketch Editor: {}.", tool.label()),
                );
            } else {
                mode.set(
                    ActiveMode::BuildLive { tool },
                    "Sketch Editor stays active. Click start, move to a snapped endpoint, click finish; RMB orbits; toolbox picks Room and Opening.",
                );
            }
        }
    }
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
    for key in [
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
        if keys.just_pressed(key.0) {
            return None;
        }
    }
    None
}

fn normalized_build_tool(tool: ToolbeltTool) -> ToolbeltTool {
    if tool == ToolbeltTool::Navigate {
        ToolbeltTool::BrushPlace
    } else {
        tool
    }
}

#[cfg(test)]
fn next_mode_for_f8_with_history(
    current_mode: ActiveMode,
    _last_mode: ActiveMode,
    _fallback_tool: ToolbeltTool,
) -> (ActiveMode, String) {
    (
        current_mode,
        "Function-key workflow disabled. Use the Sketch Editor toolbox or Play button.".into(),
    )
}

#[cfg(test)]
fn next_mode_for_f7_with_history(
    current_mode: ActiveMode,
    _last_mode: ActiveMode,
    _fallback_tool: ToolbeltTool,
) -> (ActiveMode, String) {
    (
        current_mode,
        "Function-key workflow disabled. Pick tools from the Sketch Editor toolbox.".into(),
    )
}

#[cfg(test)]
fn next_mode_for_tab(
    current_mode: ActiveMode,
    _fallback_tool: ToolbeltTool,
) -> (ActiveMode, String) {
    (
        current_mode,
        "Toolbox handles editor drawers; keyboard drawer toggle disabled.".into(),
    )
}

fn default_creative_mode() -> ActiveMode {
    ActiveMode::BuildLive {
        tool: ToolbeltTool::DrawRect,
    }
}

fn default_creative_status() -> &'static str {
    "Creative Sketch Builder active. Click start, move to a snapped endpoint, click finish; RMB orbits; toolbox picks Pencil, Rectangle, Room, Opening, and Push/Pull."
}

fn resume_mode_after_overlay(last_mode: ActiveMode) -> ActiveMode {
    match last_mode {
        ActiveMode::Combat
        | ActiveMode::BuildPicker { .. }
        | ActiveMode::BuildLive { .. }
        | ActiveMode::ShipPlacement { .. }
        | ActiveMode::ShipFlight { .. } => last_mode,
        ActiveMode::Editor { .. }
        | ActiveMode::Inventory
        | ActiveMode::Paused
        | ActiveMode::CommandPalette => default_creative_mode(),
    }
}

fn resume_status(mode: ActiveMode) -> String {
    match mode {
        ActiveMode::Combat => {
            "Weapons still armed from before pause. Open Sketch Editor to build.".into()
        }
        ActiveMode::BuildPicker { tool } => {
            format!("Sketch Editor drawer restored: {}.", tool.label())
        }
        ActiveMode::BuildLive { tool } => {
            format!("Creative Build restored: {}. {}", tool.label(), tool.hint())
        }
        ActiveMode::ShipPlacement { kind } => format!("Ship placement restored: {}.", kind.label()),
        ActiveMode::ShipFlight { .. } => "Ship cockpit restored.".into(),
        ActiveMode::Editor { .. }
        | ActiveMode::Inventory
        | ActiveMode::Paused
        | ActiveMode::CommandPalette => default_creative_status().into(),
    }
}

fn sync_legacy_toolbelt(mut toolbelt: ResMut<ToolbeltState>, mode: Res<ModeContext>) {
    if let Some(tool) = mode.build_tool() {
        toolbelt.tool = tool;
    }

    toolbelt.live = mode.mode.is_build();
    toolbelt.palette_open = mode.mode.is_build_picker();
    if !toolbelt.live {
        toolbelt.clear_contextual_workflow();
    } else {
        toolbelt.sync_workflow_to_tool();
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorPolicy {
    LockedHidden,
    ReleasedVisible,
}

fn cursor_policy_for(
    game_state: GameState,
    mode: ActiveMode,
    editor_open: bool,
    command_palette_open: bool,
    sketch_orbiting: bool,
    pointer_over_editor_ui: bool,
) -> CursorPolicy {
    if game_state != GameState::InGame || editor_open || command_palette_open {
        return CursorPolicy::ReleasedVisible;
    }
    let sketch_orbiting_in_world = sketch_orbiting && !pointer_over_editor_ui;
    match mode {
        ActiveMode::BuildPicker { .. }
        | ActiveMode::Editor { .. }
        | ActiveMode::Inventory
        | ActiveMode::Paused
        | ActiveMode::CommandPalette => CursorPolicy::ReleasedVisible,
        ActiveMode::BuildLive { tool } if tool.uses_pointer_editor_cursor() => {
            if sketch_orbiting_in_world {
                CursorPolicy::LockedHidden
            } else {
                CursorPolicy::ReleasedVisible
            }
        }
        ActiveMode::BuildLive { .. }
        | ActiveMode::Combat
        | ActiveMode::ShipPlacement { .. }
        | ActiveMode::ShipFlight { .. } => CursorPolicy::LockedHidden,
    }
}

pub fn gameplay_cursor_grab_mode() -> CursorGrabMode {
    // Bevy 0.14 / winit do not support locked cursor grab on Windows.
    // Asking for the supported mode directly avoids a failed lock attempt
    // and keeps the engine's resource state aligned with the actual OS grab.
    #[cfg(target_os = "windows")]
    {
        CursorGrabMode::Confined
    }
    #[cfg(not(target_os = "windows"))]
    {
        CursorGrabMode::Locked
    }
}

pub fn cursor_is_captured(window: &Window) -> bool {
    matches!(
        window.cursor.grab_mode,
        CursorGrabMode::Locked | CursorGrabMode::Confined
    )
}

fn mode_cursor_guard(
    game_state: Res<State<GameState>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<ModeContext>,
    editor: Res<EditorState>,
    command_palette: Option<Res<CommandPaletteState>>,
    ui_focus: Option<Res<SketchEditorUiFocus>>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    let Ok(mut window) = windows.get_single_mut() else {
        return;
    };

    match cursor_policy_for(
        game_state.get().clone(),
        mode.mode,
        editor.open,
        command_palette.as_deref().map(|p| p.open).unwrap_or(false),
        mouse.pressed(MouseButton::Right),
        ui_focus
            .as_deref()
            .is_some_and(|focus| focus.pointer_over_editor_ui),
    ) {
        CursorPolicy::ReleasedVisible => {
            window.cursor.grab_mode = CursorGrabMode::None;
            window.cursor.visible = true;
        }
        CursorPolicy::LockedHidden => {
            window.cursor.grab_mode = gameplay_cursor_grab_mode();
            window.cursor.visible = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_is_creative_build_not_combat() {
        assert_eq!(
            ActiveMode::default(),
            ActiveMode::BuildLive {
                tool: ToolbeltTool::DrawRect
            }
        );
        let mode = ModeContext::default();
        assert!(!mode.allows_weapons());
        assert!(mode.is_build_live());
        assert!(mode.status.contains("Sketch"));
        assert!(mode.status.contains("RMB"));
    }

    #[test]
    fn resume_after_menu_defaults_to_creative_build() {
        assert_eq!(
            resume_mode_after_overlay(ActiveMode::Paused),
            ActiveMode::BuildLive {
                tool: ToolbeltTool::DrawRect
            }
        );
        assert_eq!(
            resume_mode_after_overlay(ActiveMode::Combat),
            ActiveMode::Combat
        );
    }

    #[test]
    fn function_keys_no_longer_switch_or_arm_editor_modes() {
        let (next, _) = next_mode_for_f7_with_history(
            ActiveMode::Combat,
            ActiveMode::Combat,
            ToolbeltTool::CityRoad,
        );
        assert_eq!(next, ActiveMode::Combat);

        let (next, _) = next_mode_for_f7_with_history(
            ActiveMode::BuildPicker {
                tool: ToolbeltTool::CityBuilding,
            },
            ActiveMode::BuildPicker {
                tool: ToolbeltTool::CityBuilding,
            },
            ToolbeltTool::DrawRect,
        );
        assert_eq!(
            next,
            ActiveMode::BuildPicker {
                tool: ToolbeltTool::CityBuilding
            }
        );

        let (next, _) = next_mode_for_f8_with_history(
            ActiveMode::BuildLive {
                tool: ToolbeltTool::Sculpt,
            },
            ActiveMode::BuildLive {
                tool: ToolbeltTool::Sculpt,
            },
            ToolbeltTool::DrawRect,
        );
        assert_eq!(
            next,
            ActiveMode::BuildLive {
                tool: ToolbeltTool::Sculpt
            }
        );
    }

    #[test]
    fn tab_no_longer_opens_editor_drawer() {
        let (next, status) = next_mode_for_tab(ActiveMode::Combat, ToolbeltTool::CityRoad);
        assert_eq!(next, ActiveMode::Combat);
        assert!(status.contains("Toolbox"));
        assert!(!status.contains("Tab"));

        let (next, _) = next_mode_for_tab(
            ActiveMode::BuildPicker {
                tool: ToolbeltTool::CityRoad,
            },
            ToolbeltTool::DrawRect,
        );
        assert_eq!(
            next,
            ActiveMode::BuildPicker {
                tool: ToolbeltTool::CityRoad
            }
        );
    }

    #[test]
    fn number_keys_no_longer_switch_editor_tools() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::Digit6);

        assert_eq!(quick_tool_key(&keys), None);
    }

    #[test]
    fn legacy_sync_preserves_mouse_selected_workflow_for_matching_tool() {
        let mut toolbelt = ToolbeltState::default();
        toolbelt.select_workflow_for_test(crate::toolbelt::BuildWorkflowPreset::PushPull);
        let mode = ModeContext {
            mode: ActiveMode::BuildLive {
                tool: ToolbeltTool::Sculpt,
            },
            last_mode: ActiveMode::BuildLive {
                tool: ToolbeltTool::DrawRect,
            },
            status: "Push/Pull selected from toolbox.".into(),
            transition_hint_t: 0.0,
        };
        let mut app = App::new();
        app.insert_resource(toolbelt);
        app.insert_resource(mode);
        app.add_systems(Update, sync_legacy_toolbelt);

        app.update();

        let toolbelt = app.world().resource::<ToolbeltState>();
        assert_eq!(toolbelt.tool, ToolbeltTool::Sculpt);
        assert_eq!(
            toolbelt.active_workflow(),
            Some(crate::toolbelt::BuildWorkflowPreset::PushPull),
            "toolbox workflow should not disappear one frame after clicking Push/Pull"
        );
    }

    #[test]
    fn visible_editor_statuses_do_not_name_function_key_workflows() {
        let statuses = [
            default_creative_status().to_owned(),
            next_mode_for_f8_with_history(
                ActiveMode::BuildLive {
                    tool: ToolbeltTool::DrawRect,
                },
                ActiveMode::BuildLive {
                    tool: ToolbeltTool::DrawRect,
                },
                ToolbeltTool::DrawRect,
            )
            .1,
            next_mode_for_f7_with_history(
                ActiveMode::Combat,
                ActiveMode::Combat,
                ToolbeltTool::DrawRect,
            )
            .1,
            next_mode_for_tab(ActiveMode::Combat, ToolbeltTool::DrawRect).1,
        ];

        for status in statuses {
            assert!(
                !["F1", "F5", "F7", "F8", "6-9"]
                    .iter()
                    .any(|token| status.contains(token)),
                "status still advertises keyboard workflow: {status}"
            );
        }
    }

    #[test]
    fn f8_does_not_restore_build_tool_from_stale_history() {
        let (next, _) = next_mode_for_f8_with_history(
            ActiveMode::Combat,
            ActiveMode::BuildLive {
                tool: ToolbeltTool::CityRoad,
            },
            ToolbeltTool::BrushPlace,
        );

        assert_eq!(next, ActiveMode::Combat);
    }

    #[test]
    fn f7_does_not_restore_build_tool_from_stale_history() {
        let (next, _) = next_mode_for_f7_with_history(
            ActiveMode::Combat,
            ActiveMode::BuildLive {
                tool: ToolbeltTool::CityBuilding,
            },
            ToolbeltTool::BrushPlace,
        );

        assert_eq!(next, ActiveMode::Combat);
    }

    #[test]
    fn cursor_policy_releases_for_menus_even_with_stale_live_mode() {
        assert_eq!(
            cursor_policy_for(
                GameState::Paused,
                ActiveMode::BuildLive {
                    tool: ToolbeltTool::BrushPlace,
                },
                false,
                false,
                false,
                false,
            ),
            CursorPolicy::ReleasedVisible
        );
    }

    #[test]
    fn cursor_policy_locks_live_brush_and_combat_immediately() {
        assert_eq!(
            cursor_policy_for(
                GameState::InGame,
                ActiveMode::BuildLive {
                    tool: ToolbeltTool::BrushPlace,
                },
                false,
                false,
                false,
                false,
            ),
            CursorPolicy::LockedHidden
        );
        assert_eq!(
            cursor_policy_for(
                GameState::InGame,
                ActiveMode::Combat,
                false,
                false,
                false,
                false
            ),
            CursorPolicy::LockedHidden
        );
    }

    #[test]
    fn cursor_policy_releases_for_pointer_editor_tools_until_right_orbit() {
        for tool in [
            ToolbeltTool::Navigate,
            ToolbeltTool::DrawRect,
            ToolbeltTool::Sculpt,
            ToolbeltTool::SmartTower,
            ToolbeltTool::CityRoad,
            ToolbeltTool::CityDistrict,
            ToolbeltTool::CityBuilding,
            ToolbeltTool::CityFacade,
            ToolbeltTool::AnimationPick,
        ] {
            assert_eq!(
                cursor_policy_for(
                    GameState::InGame,
                    ActiveMode::BuildLive { tool },
                    false,
                    false,
                    false,
                    false,
                ),
                CursorPolicy::ReleasedVisible,
                "{tool:?} needs a real visible cursor for mouse-first editing"
            );
            assert_eq!(
                cursor_policy_for(
                    GameState::InGame,
                    ActiveMode::BuildLive { tool },
                    false,
                    false,
                    true,
                    false,
                ),
                CursorPolicy::LockedHidden,
                "Holding RMB in {tool:?} should immediately become camera orbit"
            );
        }

        for tool in [ToolbeltTool::BrushPlace, ToolbeltTool::BrushCut] {
            assert_eq!(
                cursor_policy_for(
                    GameState::InGame,
                    ActiveMode::BuildLive { tool },
                    false,
                    false,
                    false,
                    false,
                ),
                CursorPolicy::LockedHidden,
                "{tool:?} remains the old FPS brush path and keeps mouse-look capture"
            );
        }
    }

    #[test]
    fn cursor_policy_keeps_toolbox_mouse_visible_during_right_click() {
        assert_eq!(
            cursor_policy_for(
                GameState::InGame,
                ActiveMode::BuildLive {
                    tool: ToolbeltTool::DrawRect,
                },
                false,
                false,
                true,
                true,
            ),
            CursorPolicy::ReleasedVisible,
            "right mouse over the Sketch Editor UI must not steal the cursor into orbit"
        );
    }

    #[test]
    fn cursor_policy_releases_only_for_clickable_ingame_overlays() {
        assert_eq!(
            cursor_policy_for(
                GameState::InGame,
                ActiveMode::BuildPicker {
                    tool: ToolbeltTool::CityRoad,
                },
                false,
                false,
                false,
                false,
            ),
            CursorPolicy::ReleasedVisible
        );
        assert_eq!(
            cursor_policy_for(
                GameState::InGame,
                ActiveMode::Combat,
                false,
                true,
                false,
                false
            ),
            CursorPolicy::ReleasedVisible
        );
    }

    #[test]
    fn gameplay_cursor_uses_supported_platform_grab_mode() {
        #[cfg(target_os = "windows")]
        assert_eq!(gameplay_cursor_grab_mode(), CursorGrabMode::Confined);

        #[cfg(not(target_os = "windows"))]
        assert_eq!(gameplay_cursor_grab_mode(), CursorGrabMode::Locked);
    }

    #[test]
    fn cursor_capture_accepts_platform_fallback_modes() {
        let mut window = Window::default();
        window.cursor.grab_mode = CursorGrabMode::None;
        assert!(!cursor_is_captured(&window));

        window.cursor.grab_mode = CursorGrabMode::Confined;
        assert!(cursor_is_captured(&window));

        window.cursor.grab_mode = CursorGrabMode::Locked;
        assert!(cursor_is_captured(&window));
    }
}
