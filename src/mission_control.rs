//! Multi-agent mission-control registry and live-feed wall.
//!
//! Agent-Control instances publish a compact, versioned `mission_feed.ron`
//! next to their screenshots.  A `--mission-control` instance discovers the
//! feeds without hard-coded agent counts, shows a responsive wall of bounded
//! PNG previews, and can launch the existing deterministic Live Link in
//! SPECTATE or JOIN mode for one selected agent.
//!
//! This is deliberately a local-only observability bridge.  Feed paths must
//! remain under the configured mission root and Live Link endpoints must be
//! loopback addresses; a future agent cannot turn the dashboard into an
//! arbitrary file viewer or network client by editing its status file.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiSet};
use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
use image::ImageDecoder;
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::io::Cursor;
#[cfg(not(target_arch = "wasm32"))]
use std::net::SocketAddr;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const MISSION_FEED_SCHEMA_VERSION: u32 = 1;
const MISSION_FEED_FILE: &str = "mission_feed.ron";
const DEFAULT_MISSION_ROOT: &str = "agent_runs";
const ACTIVE_FEED_SECONDS: u64 = 4;
const RECENT_FEED_SECONDS: u64 = 15 * 60;
const MAX_DISCOVERED_FEEDS: usize = 48;
const MAX_SCAN_DEPTH: usize = 4;
const MAX_PREVIEW_FILE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_PREVIEW_EDGE: u32 = 4096;
const MAX_PREVIEW_PIXELS: u64 = 16_777_216;
const DEFAULT_SCAN_INTERVAL: f32 = 0.5;

pub(crate) struct MissionControlPlugin;

impl Plugin for MissionControlPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(MissionControl::from_env()).add_systems(
            Update,
            (
                toggle_mission_control,
                scan_mission_control_feeds,
                draw_mission_control.after(EguiSet::InitContexts),
            )
                .chain(),
        );
    }
}

/// Small cross-process contract.  Keep this independent from the much larger
/// `AgentLiveStatus`: old dashboards can ignore new fields and future agents
/// only need to publish this stable observation surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct MissionFeedSnapshot {
    pub schema_version: u32,
    pub agent_id: String,
    pub fleet_id: String,
    pub display_name: String,
    pub role: String,
    pub task: String,
    pub process_id: u32,
    pub heartbeat_epoch: u64,
    pub status: String,
    pub game_state: String,
    pub world_name: String,
    pub world_profile: String,
    pub world_seed: u32,
    pub time_of_day: f32,
    pub position: [f32; 3],
    pub fps: f32,
    pub frame_ms: f32,
    pub stall_count: u64,
    pub loaded_chunks: usize,
    pub pending_work: usize,
    pub warning_count: usize,
    pub error_count: usize,
    pub control_enabled: bool,
    pub capability_schema_version: u32,
    pub power_profile_id: String,
    pub direct_bridge_ready: bool,
    pub ron_fallback_ready: bool,
    pub visual_capture_ready: bool,
    pub last_screenshot: Option<String>,
    pub session_dir: String,
    pub live_link_side: Option<String>,
    pub live_link_bind: Option<String>,
    pub live_link_peer: Option<String>,
}

impl Default for MissionFeedSnapshot {
    fn default() -> Self {
        Self {
            schema_version: MISSION_FEED_SCHEMA_VERSION,
            agent_id: "unidentified".into(),
            fleet_id: String::new(),
            display_name: "UNIDENTIFIED AGENT".into(),
            role: "ENGINE QA".into(),
            task: "Waiting for task telemetry".into(),
            process_id: 0,
            heartbeat_epoch: 0,
            status: "starting".into(),
            game_state: "unknown".into(),
            world_name: "unknown".into(),
            world_profile: "unknown".into(),
            world_seed: 0,
            time_of_day: 0.0,
            position: [0.0; 3],
            fps: 0.0,
            frame_ms: 0.0,
            stall_count: 0,
            loaded_chunks: 0,
            pending_work: 0,
            warning_count: 0,
            error_count: 0,
            control_enabled: false,
            capability_schema_version: 0,
            power_profile_id: String::new(),
            direct_bridge_ready: false,
            ron_fallback_ready: false,
            visual_capture_ready: false,
            last_screenshot: None,
            session_dir: String::new(),
            live_link_side: None,
            live_link_bind: None,
            live_link_peer: None,
        }
    }
}

impl MissionFeedSnapshot {
    fn normalize(mut self) -> Option<Self> {
        if self.schema_version != MISSION_FEED_SCHEMA_VERSION
            || !self.position.iter().all(|value| value.is_finite())
            || !self.fps.is_finite()
            || !self.frame_ms.is_finite()
            || !self.time_of_day.is_finite()
        {
            return None;
        }
        self.agent_id = clean_agent_id(&self.agent_id);
        if self.agent_id.is_empty() {
            return None;
        }
        self.display_name = clean_label(&self.display_name, 48);
        self.fleet_id = clean_agent_id(&self.fleet_id);
        self.role = clean_label(&self.role, 36);
        self.task = clean_label(&self.task, 180);
        self.status = clean_label(&self.status, 180);
        self.game_state = clean_label(&self.game_state, 32);
        self.world_name = clean_label(&self.world_name, 80);
        self.world_profile = clean_label(&self.world_profile, 32);
        self.fps = self.fps.clamp(0.0, 2_000.0);
        self.frame_ms = self.frame_ms.clamp(0.0, 10_000.0);
        self.time_of_day = self.time_of_day.rem_euclid(24.0);
        Some(self)
    }

    fn has_live_link_pair(&self) -> bool {
        validate_live_link_pair(
            self.live_link_side.as_deref(),
            self.live_link_bind.as_deref(),
            self.live_link_peer.as_deref(),
        )
    }

    fn has_shared_power_parity(&self) -> bool {
        self.capability_schema_version == crate::agent_capabilities::AGENT_CAPABILITY_SCHEMA_VERSION
            && self.power_profile_id == crate::agent_capabilities::SHARED_POWER_PROFILE_ID
            && self.ron_fallback_ready
            && self.visual_capture_ready
    }
}

#[derive(Clone)]
struct DiscoveredFeed {
    snapshot: MissionFeedSnapshot,
    feed_path: PathBuf,
    screenshot_path: Option<PathBuf>,
    modified_epoch: u64,
    active: bool,
}

struct CachedPreview {
    path: PathBuf,
    modified_nanos: u128,
    handle: egui::TextureHandle,
}

#[derive(Resource)]
struct MissionControl {
    enabled: bool,
    visible: bool,
    show_recent: bool,
    scan_timer: f32,
    scan_interval: f32,
    root: PathBuf,
    feeds: Vec<DiscoveredFeed>,
    selected_key: Option<String>,
    previews: HashMap<String, CachedPreview>,
    status: String,
}

impl MissionControl {
    fn from_env() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let enabled = mission_control_requested();
        #[cfg(target_arch = "wasm32")]
        let enabled = false;
        let root = std::env::var("VOXEL_NATIVE_MISSION_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_MISSION_ROOT));
        if enabled {
            let _ = std::fs::create_dir_all(&root);
        }
        Self {
            enabled,
            visible: enabled,
            show_recent: false,
            scan_timer: 0.0,
            scan_interval: DEFAULT_SCAN_INTERVAL,
            root,
            feeds: Vec::new(),
            selected_key: None,
            previews: HashMap::new(),
            status: "SCANNING LOCAL AGENT REGISTRY".into(),
        }
    }
}

fn mission_control_requested() -> bool {
    std::env::var("VOXEL_NATIVE_MISSION_CONTROL")
        .map(|value| env_truthy(&value))
        .unwrap_or(false)
        || std::env::args().any(|arg| arg == "--mission-control")
}

fn env_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn toggle_mission_control(keys: Res<ButtonInput<KeyCode>>, mut control: ResMut<MissionControl>) {
    if control.enabled && keys.just_pressed(KeyCode::F9) {
        control.visible = !control.visible;
    }
}

fn scan_mission_control_feeds(time: Res<Time>, mut control: ResMut<MissionControl>) {
    if !control.enabled {
        return;
    }
    control.scan_timer -= time.delta_seconds();
    if control.scan_timer > 0.0 {
        return;
    }
    control.scan_timer = control.scan_interval;

    #[cfg(not(target_arch = "wasm32"))]
    {
        match discover_mission_feeds(&control.root, now_epoch()) {
            Ok(feeds) => {
                let active = feeds.iter().filter(|feed| feed.active).count();
                let parity_mismatches = feeds
                    .iter()
                    .filter(|feed| !feed.snapshot.has_shared_power_parity())
                    .count();
                control.status = format!(
                    "{} ACTIVE // {} RECENT // {} POWER MISMATCH // REGISTRY {}",
                    active,
                    feeds.len().saturating_sub(active),
                    parity_mismatches,
                    control.root.display()
                );
                control.feeds = feeds;
                if control.selected_key.as_ref().is_some_and(|selected| {
                    !control.feeds.iter().any(|feed| feed_key(feed) == *selected)
                }) {
                    control.selected_key = None;
                }
                let live_keys = control.feeds.iter().map(feed_key).collect::<Vec<_>>();
                control
                    .previews
                    .retain(|key, _| live_keys.iter().any(|candidate| candidate == key));
            }
            Err(error) => {
                control.status = format!("REGISTRY ERROR // {error}");
            }
        }
    }
}

fn draw_mission_control(
    mut contexts: EguiContexts,
    mut control: ResMut<MissionControl>,
    settings: Option<Res<crate::settings::WorldSettings>>,
) {
    if !control.enabled || !control.visible {
        return;
    }
    let ctx = contexts.ctx_mut();
    refresh_preview_textures(ctx, &mut control);

    let screen = ctx.screen_rect();
    let rect = screen.shrink2(egui::vec2(12.0, 12.0));
    let theme = settings
        .as_deref()
        .map(|settings| settings.theme)
        .unwrap_or_default();
    let accent = theme.color.primary();
    let feeds = control
        .feeds
        .iter()
        .filter(|feed| control.show_recent || feed.active)
        .cloned()
        .collect::<Vec<_>>();
    let selected = control.selected_key.clone();
    let mut next_selected = selected.clone();
    let mut launch_request: Option<(DiscoveredFeed, bool)> = None;
    let mut show_recent = control.show_recent;

    egui::Area::new(egui::Id::new("voxel_native_mission_control"))
        .fixed_pos(rect.min)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            egui::Frame::none()
                .fill(egui::Color32::from_rgba_unmultiplied(3, 8, 17, 246))
                .stroke(egui::Stroke::new(1.3, accent))
                .rounding(egui::Rounding::same(10.0))
                .inner_margin(egui::Margin::same(14.0))
                .show(ui, |ui| {
                    draw_mission_header(ui, &control.status, &mut show_recent, accent);
                    ui.add_space(8.0);

                    if let Some(selected_feed) = selected
                        .as_ref()
                        .and_then(|key| feeds.iter().find(|feed| feed_key(feed) == *key))
                    {
                        draw_focus_feed(
                            ui,
                            selected_feed,
                            control.previews.get(&feed_key(selected_feed)),
                            accent,
                            &mut launch_request,
                        );
                        ui.add_space(9.0);
                    }

                    let footer_height = 26.0;
                    let available = (ui.available_height() - footer_height).max(120.0);
                    egui::ScrollArea::vertical()
                        .id_source("mission_feed_wall")
                        .max_height(available)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if feeds.is_empty() {
                                draw_empty_registry(ui, accent);
                                return;
                            }
                            let width = ui.available_width().max(280.0);
                            let columns = responsive_feed_columns(width);
                            let gap = 9.0;
                            let card_width = ((width - gap * (columns.saturating_sub(1)) as f32)
                                / columns as f32)
                                .max(240.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                                for feed in &feeds {
                                    let key = feed_key(feed);
                                    let clicked = draw_feed_card(
                                        ui,
                                        feed,
                                        control.previews.get(&key),
                                        card_width,
                                        selected.as_deref() == Some(key.as_str()),
                                        accent,
                                    );
                                    if clicked {
                                        next_selected = Some(key);
                                    }
                                }
                            });
                        });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("F9 CLOSE // LOCAL LOOPBACK ONLY // PNG + RON v1")
                                .monospace()
                                .size(10.0)
                                .color(egui::Color32::from_gray(120)),
                        );
                    });
                });
        });

    control.selected_key = next_selected;
    control.show_recent = show_recent;
    if let Some((feed, join)) = launch_request {
        match launch_live_link_view(&feed.snapshot, join) {
            Ok(()) => {
                control.status = format!(
                    "{} LAUNCHED // {}",
                    if join { "JOIN" } else { "SPECTATOR" },
                    feed.snapshot.display_name
                );
            }
            Err(error) => control.status = format!("LAUNCH BLOCKED // {error}"),
        }
    }
}

fn draw_mission_header(
    ui: &mut egui::Ui,
    status: &str,
    show_recent: &mut bool,
    accent: egui::Color32,
) {
    if mission_header_is_stacked(ui.available_width()) {
        ui.vertical(|ui| {
            draw_mission_title(ui, status, accent);
            ui.add_space(4.0);
            ui.checkbox(show_recent, "SHOW RECENT / OFFLINE");
            ui.label(
                egui::RichText::new("ALL AGENTS // ONE WORLD LAB")
                    .monospace()
                    .size(11.0)
                    .color(egui::Color32::from_gray(160)),
            );
        });
        ui.separator();
        return;
    }

    ui.horizontal(|ui| {
        ui.vertical(|ui| draw_mission_title(ui, status, accent));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.checkbox(show_recent, "SHOW RECENT / OFFLINE");
            ui.label(
                egui::RichText::new("ALL AGENTS // ONE WORLD LAB")
                    .monospace()
                    .size(11.0)
                    .color(egui::Color32::from_gray(160)),
            );
        });
    });
    ui.separator();
}

fn draw_mission_title(ui: &mut egui::Ui, status: &str, accent: egui::Color32) {
    ui.label(
        egui::RichText::new("MISSION CONTROL // OMNISCOPE")
            .monospace()
            .strong()
            .size(22.0)
            .color(egui::Color32::WHITE),
    );
    ui.label(
        egui::RichText::new(status)
            .monospace()
            .size(11.0)
            .color(accent),
    );
}

fn draw_focus_feed(
    ui: &mut egui::Ui,
    feed: &DiscoveredFeed,
    preview: Option<&CachedPreview>,
    accent: egui::Color32,
    launch_request: &mut Option<(DiscoveredFeed, bool)>,
) {
    let state_color = feed_state_color(feed);
    egui::Frame::none()
        .fill(egui::Color32::from_rgba_unmultiplied(7, 15, 27, 235))
        .stroke(egui::Stroke::new(1.2, state_color))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            let available_width = ui.available_width();
            if focus_layout_is_stacked(available_width) {
                let preview_height = (available_width * 9.0 / 16.0).clamp(160.0, 320.0);
                draw_preview(
                    ui,
                    preview,
                    egui::vec2(available_width, preview_height),
                    feed.active,
                );
                ui.add_space(8.0);
                draw_focus_details(ui, feed, accent, state_color, launch_request, true);
            } else {
                let focus_height = (available_width * 0.26).clamp(180.0, 320.0);
                ui.columns(2, |columns| {
                    let preview_width = columns[0].available_width();
                    draw_preview(
                        &mut columns[0],
                        preview,
                        egui::vec2(preview_width, focus_height),
                        feed.active,
                    );
                    columns[1].vertical(|ui| {
                        draw_focus_details(ui, feed, accent, state_color, launch_request, false);
                    });
                });
            }
        });
}

fn draw_focus_details(
    ui: &mut egui::Ui,
    feed: &DiscoveredFeed,
    accent: egui::Color32,
    state_color: egui::Color32,
    launch_request: &mut Option<(DiscoveredFeed, bool)>,
    compact: bool,
) {
    ui.label(
        egui::RichText::new(&feed.snapshot.display_name)
            .monospace()
            .strong()
            .size(18.0)
            .color(egui::Color32::WHITE),
    );
    ui.label(
        egui::RichText::new(format!(
            "{} // {}",
            feed.snapshot.role,
            if feed.active { "LIVE" } else { "OFFLINE" }
        ))
        .monospace()
        .size(11.0)
        .color(state_color),
    );
    ui.add_space(5.0);
    ui.label(
        egui::RichText::new(&feed.snapshot.task)
            .size(13.0)
            .color(egui::Color32::from_gray(205)),
    );
    ui.add_space(8.0);
    metric_row(ui, "WORLD", &feed.snapshot.world_name, accent);
    metric_row(
        ui,
        "FLEET",
        if feed.snapshot.fleet_id.is_empty() {
            "unregistered"
        } else {
            &feed.snapshot.fleet_id
        },
        accent,
    );
    metric_row(
        ui,
        "POWER",
        &power_parity_label(&feed.snapshot),
        if feed.snapshot.has_shared_power_parity() {
            accent
        } else {
            egui::Color32::from_rgb(255, 84, 108)
        },
    );
    metric_row(
        ui,
        "PROFILE",
        &format!(
            "{} // seed {} // {:04.1}h",
            feed.snapshot.world_profile, feed.snapshot.world_seed, feed.snapshot.time_of_day
        ),
        accent,
    );
    metric_row(
        ui,
        "POSITION",
        &format!(
            "{:.1}  {:.1}  {:.1}",
            feed.snapshot.position[0], feed.snapshot.position[1], feed.snapshot.position[2]
        ),
        accent,
    );
    metric_row(
        ui,
        "PERF",
        &format!(
            "{:.1} FPS // {:.1} ms // {} stalls",
            feed.snapshot.fps, feed.snapshot.frame_ms, feed.snapshot.stall_count
        ),
        accent,
    );
    metric_row(
        ui,
        "STREAM",
        &format!(
            "{} chunks // {} pending",
            feed.snapshot.loaded_chunks, feed.snapshot.pending_work
        ),
        accent,
    );
    metric_row(ui, "STATUS", &feed.snapshot.status, accent);
    ui.add_space(8.0);
    let linked = feed.snapshot.has_live_link_pair() && feed.active;
    let mut draw_launch = |ui: &mut egui::Ui| {
        let width = if compact {
            ui.available_width().max(220.0)
        } else {
            0.0
        };
        let spectate = if compact {
            ui.add_enabled(
                linked,
                egui::Button::new("SPECTATE // OPEN LIVE ENGINE").min_size(egui::vec2(width, 0.0)),
            )
        } else {
            ui.add_enabled(linked, egui::Button::new("SPECTATE // OPEN LIVE ENGINE"))
        };
        if spectate.clicked() {
            *launch_request = Some((feed.clone(), false));
        }
        let join = if compact {
            ui.add_enabled(
                linked,
                egui::Button::new("JOIN // YOU LEAD").min_size(egui::vec2(width, 0.0)),
            )
        } else {
            ui.add_enabled(linked, egui::Button::new("JOIN // YOU LEAD"))
        };
        if join.clicked() {
            *launch_request = Some((feed.clone(), true));
        }
    };
    if compact {
        ui.vertical(|ui| draw_launch(ui));
    } else {
        ui.horizontal(|ui| draw_launch(ui));
    }
    if !linked {
        ui.label(
            egui::RichText::new(
                "Live launch requires an active loopback pair published by this agent.",
            )
            .monospace()
            .size(10.0)
            .color(egui::Color32::from_rgb(235, 169, 91)),
        );
    }
}

fn draw_feed_card(
    ui: &mut egui::Ui,
    feed: &DiscoveredFeed,
    preview: Option<&CachedPreview>,
    width: f32,
    selected: bool,
    accent: egui::Color32,
) -> bool {
    let state_color = feed_state_color(feed);
    let mut clicked = false;
    egui::Frame::none()
        .fill(if selected {
            egui::Color32::from_rgba_unmultiplied(13, 27, 44, 245)
        } else {
            egui::Color32::from_rgba_unmultiplied(7, 14, 25, 232)
        })
        .stroke(egui::Stroke::new(
            if selected { 1.8 } else { 1.0 },
            state_color,
        ))
        .rounding(egui::Rounding::same(7.0))
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            ui.set_width(width);
            let response = draw_preview(
                ui,
                preview,
                egui::vec2(width - 16.0, ((width - 16.0) * 9.0 / 16.0).max(112.0)),
                feed.active,
            );
            clicked |= response.clicked();
            ui.horizontal(|ui| {
                ui.colored_label(
                    state_color,
                    if feed.active {
                        "● LIVE"
                    } else {
                        "○ RECENT"
                    },
                );
                ui.label(
                    egui::RichText::new(&feed.snapshot.display_name)
                        .monospace()
                        .strong()
                        .color(egui::Color32::WHITE),
                );
            });
            ui.label(
                egui::RichText::new(&feed.snapshot.task)
                    .size(11.0)
                    .color(egui::Color32::from_gray(180)),
            );
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{:>5.1} FPS", feed.snapshot.fps))
                        .monospace()
                        .color(accent),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "{} CH // {} Q",
                        feed.snapshot.loaded_chunks, feed.snapshot.pending_work
                    ))
                    .monospace()
                    .color(egui::Color32::from_gray(150)),
                );
                if feed.snapshot.error_count > 0 {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 92, 110),
                        format!("{} ERR", feed.snapshot.error_count),
                    );
                }
            });
            ui.label(
                egui::RichText::new(power_parity_label(&feed.snapshot))
                    .monospace()
                    .size(10.0)
                    .color(if feed.snapshot.has_shared_power_parity() {
                        accent
                    } else {
                        egui::Color32::from_rgb(255, 84, 108)
                    }),
            );
            clicked |= ui
                .add_sized([width - 16.0, 24.0], egui::Button::new("FOCUS FEED"))
                .clicked();
        });
    clicked
}

fn draw_preview(
    ui: &mut egui::Ui,
    preview: Option<&CachedPreview>,
    size: egui::Vec2,
    active: bool,
) -> egui::Response {
    if let Some(preview) = preview {
        let tint = if active {
            egui::Color32::WHITE
        } else {
            egui::Color32::from_gray(105)
        };
        return ui.add(
            egui::Image::from_texture(&preview.handle)
                .fit_to_exact_size(size)
                .tint(tint)
                .sense(egui::Sense::click()),
        );
    }
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    ui.painter()
        .rect_filled(rect, 4.0, egui::Color32::from_rgb(3, 6, 11));
    ui.painter().rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(48)),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "WAITING FOR SAFE FRAME",
        egui::FontId::monospace(11.0),
        egui::Color32::from_gray(115),
    );
    response
}

fn draw_empty_registry(ui: &mut egui::Ui, accent: egui::Color32) {
    ui.vertical_centered(|ui| {
        ui.add_space(70.0);
        ui.label(
            egui::RichText::new("NO ACTIVE AGENT FEEDS")
                .monospace()
                .strong()
                .size(18.0)
                .color(accent),
        );
        ui.label(
            egui::RichText::new(
                "Launch Agent Control with VOXEL_NATIVE_MISSION_FEED=1. Feeds appear automatically.",
            )
            .monospace()
            .size(11.0)
            .color(egui::Color32::from_gray(160)),
        );
    });
}

fn metric_row(ui: &mut egui::Ui, label: &str, value: &str, accent: egui::Color32) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{label:<9}"))
                .monospace()
                .size(10.5)
                .color(accent),
        );
        ui.label(
            egui::RichText::new(value)
                .monospace()
                .size(10.5)
                .color(egui::Color32::from_gray(190)),
        );
    });
}

fn feed_state_color(feed: &DiscoveredFeed) -> egui::Color32 {
    if !feed.active {
        egui::Color32::from_gray(104)
    } else if feed.snapshot.error_count > 0 || !feed.snapshot.has_shared_power_parity() {
        egui::Color32::from_rgb(255, 84, 108)
    } else if feed.snapshot.warning_count > 0 || feed.snapshot.pending_work > 128 {
        egui::Color32::from_rgb(255, 188, 82)
    } else {
        egui::Color32::from_rgb(74, 224, 255)
    }
}

fn power_parity_label(snapshot: &MissionFeedSnapshot) -> String {
    if snapshot.has_shared_power_parity() {
        format!(
            "SHARED v{} // {} // {}",
            snapshot.capability_schema_version,
            if snapshot.direct_bridge_ready {
                "DIRECT"
            } else {
                "RON"
            },
            snapshot.power_profile_id
        )
    } else {
        format!(
            "POWER MISMATCH // schema {} // {}",
            snapshot.capability_schema_version,
            if snapshot.power_profile_id.is_empty() {
                "missing profile"
            } else {
                &snapshot.power_profile_id
            }
        )
    }
}

fn responsive_feed_columns(width: f32) -> usize {
    if width < 620.0 {
        1
    } else if width < 1050.0 {
        2
    } else if width < 1480.0 {
        3
    } else {
        4
    }
}

fn mission_header_is_stacked(width: f32) -> bool {
    width < 760.0
}

fn focus_layout_is_stacked(width: f32) -> bool {
    width < 820.0
}

fn feed_key(feed: &DiscoveredFeed) -> String {
    format!("{}::{}", feed.snapshot.agent_id, feed.feed_path.display())
}

#[cfg(not(target_arch = "wasm32"))]
fn discover_mission_feeds(root: &Path, now: u64) -> Result<Vec<DiscoveredFeed>, String> {
    let root_absolute = canonical_or_absolute(root).map_err(|error| error.to_string())?;
    let mut files = Vec::new();
    collect_feed_files(&root_absolute, 0, &mut files);
    let mut feeds = Vec::new();
    for feed_path in files.into_iter().take(MAX_DISCOVERED_FEEDS * 2) {
        let Ok(metadata) = std::fs::metadata(&feed_path) else {
            continue;
        };
        if metadata.len() > 256 * 1024 {
            continue;
        }
        let modified_epoch = metadata
            .modified()
            .ok()
            .and_then(system_time_epoch)
            .unwrap_or(0);
        let age = now.saturating_sub(modified_epoch);
        if age > RECENT_FEED_SECONDS {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&feed_path) else {
            continue;
        };
        let Ok(snapshot) = ron::from_str::<MissionFeedSnapshot>(&text) else {
            // A writer may be between two filesystem pages. Keep the previous
            // dashboard scan instead of treating a transient partial RON as a
            // fatal registry failure.
            continue;
        };
        let Some(snapshot) = snapshot.normalize() else {
            continue;
        };
        let screenshot_path = snapshot
            .last_screenshot
            .as_deref()
            .and_then(|path| resolve_safe_preview(&root_absolute, path));
        feeds.push(DiscoveredFeed {
            snapshot,
            feed_path,
            screenshot_path,
            modified_epoch,
            active: age <= ACTIVE_FEED_SECONDS,
        });
    }
    feeds.sort_by(|left, right| {
        right
            .active
            .cmp(&left.active)
            .then_with(|| right.modified_epoch.cmp(&left.modified_epoch))
            .then_with(|| left.snapshot.display_name.cmp(&right.snapshot.display_name))
    });
    feeds.truncate(MAX_DISCOVERED_FEEDS);
    Ok(feeds)
}

#[cfg(not(target_arch = "wasm32"))]
fn collect_feed_files(directory: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth > MAX_SCAN_DEPTH || files.len() >= MAX_DISCOVERED_FEEDS * 2 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if files.len() >= MAX_DISCOVERED_FEEDS * 2 {
            return;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_feed_files(&path, depth + 1, files);
        } else if file_type.is_file()
            && path
                .file_name()
                .is_some_and(|name| name == MISSION_FEED_FILE)
        {
            files.push(path);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn canonical_or_absolute(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        path.canonicalize()
    } else if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn resolve_safe_preview(root: &Path, path: &str) -> Option<PathBuf> {
    let raw = PathBuf::from(path);
    let candidate = if raw.is_absolute() {
        raw
    } else {
        std::env::current_dir().ok()?.join(raw)
    };
    let canonical = candidate.canonicalize().ok()?;
    if !canonical.starts_with(root)
        || canonical
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("png"))
    {
        return None;
    }
    Some(canonical)
}

#[cfg(not(target_arch = "wasm32"))]
fn refresh_preview_textures(ctx: &egui::Context, control: &mut MissionControl) {
    for feed in &control.feeds {
        let Some(path) = feed.screenshot_path.as_ref() else {
            continue;
        };
        let key = feed_key(feed);
        let Ok(metadata) = std::fs::metadata(path) else {
            continue;
        };
        if metadata.len() == 0 || metadata.len() > MAX_PREVIEW_FILE_BYTES {
            continue;
        }
        let modified_nanos = metadata
            .modified()
            .ok()
            .and_then(system_time_nanos)
            .unwrap_or(0);
        if control
            .previews
            .get(&key)
            .is_some_and(|cached| cached.path == *path && cached.modified_nanos == modified_nanos)
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(decoder) = image::codecs::png::PngDecoder::new(Cursor::new(bytes)) else {
            continue;
        };
        let (width, height) = decoder.dimensions();
        let pixel_count = u64::from(width) * u64::from(height);
        if width == 0
            || height == 0
            || width > MAX_PREVIEW_EDGE
            || height > MAX_PREVIEW_EDGE
            || pixel_count > MAX_PREVIEW_PIXELS
        {
            continue;
        }
        let Ok(rgba) = image::DynamicImage::from_decoder(decoder).map(|image| image.to_rgba8())
        else {
            continue;
        };
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [width as usize, height as usize],
            rgba.as_raw(),
        );
        let handle = ctx.load_texture(
            format!("mission-feed-{key}"),
            color_image,
            egui::TextureOptions::LINEAR,
        );
        control.previews.insert(
            key,
            CachedPreview {
                path: path.clone(),
                modified_nanos,
                handle,
            },
        );
    }
}

#[cfg(target_arch = "wasm32")]
fn refresh_preview_textures(_ctx: &egui::Context, _control: &mut MissionControl) {}

#[cfg(not(target_arch = "wasm32"))]
fn validate_live_link_pair(side: Option<&str>, bind: Option<&str>, peer: Option<&str>) -> bool {
    if !side.is_some_and(|side| side.eq_ignore_ascii_case("codex")) {
        return false;
    }
    let (Ok(bind), Ok(peer)) = (
        bind.unwrap_or_default().parse::<SocketAddr>(),
        peer.unwrap_or_default().parse::<SocketAddr>(),
    ) else {
        return false;
    };
    bind.ip().is_loopback()
        && peer.ip().is_loopback()
        && bind.port() != 0
        && peer.port() != 0
        && bind != peer
        && bind.is_ipv4() == peer.is_ipv4()
}

#[cfg(target_arch = "wasm32")]
fn validate_live_link_pair(_side: Option<&str>, _bind: Option<&str>, _peer: Option<&str>) -> bool {
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn launch_live_link_view(snapshot: &MissionFeedSnapshot, join: bool) -> Result<(), String> {
    if !snapshot.has_live_link_pair() {
        return Err("agent did not publish a valid loopback Live Link pair".into());
    }
    let bind = snapshot.live_link_peer.as_deref().unwrap_or_default();
    let peer = snapshot.live_link_bind.as_deref().unwrap_or_default();
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate current engine executable: {error}"))?;
    let cwd = std::env::current_dir()
        .map_err(|error| format!("cannot resolve project directory: {error}"))?;
    let view_id = format!(
        "{}-{}-{}",
        snapshot.agent_id,
        if join { "join" } else { "spectate" },
        crate::platform::now_epoch()
    );
    let session_dir = cwd
        .join(DEFAULT_MISSION_ROOT)
        .join("viewers")
        .join(&view_id);
    let control_file = session_dir.join("viewer_control.ron");
    std::fs::create_dir_all(&session_dir)
        .map_err(|error| format!("cannot create viewer session: {error}"))?;

    let mut command = std::process::Command::new(executable);
    command
        .current_dir(cwd)
        .arg("--agent-control")
        .env("VOXEL_NATIVE_AGENT_CONTROL", "1")
        .env("VOXEL_NATIVE_AGENT_CONTROL_FILE", control_file)
        .env("VOXEL_NATIVE_AGENT_SESSION_DIR", &session_dir)
        .env("VOXEL_NATIVE_AGENT_WORLD", &snapshot.world_name)
        .env("VOXEL_NATIVE_AGENT_SEED", snapshot.world_seed.to_string())
        .env("VOXEL_NATIVE_AGENT_PROFILE", &snapshot.world_profile)
        .env("VOXEL_NATIVE_AGENT_HOUR", snapshot.time_of_day.to_string())
        .env("VOXEL_NATIVE_LIVE_LINK_SIDE", "USER")
        .env("VOXEL_NATIVE_LIVE_LINK_BIND", bind)
        .env("VOXEL_NATIVE_LIVE_LINK_PEER", peer)
        .env(
            "VOXEL_NATIVE_LIVE_LINK_START_JOIN",
            if join { "1" } else { "0" },
        )
        .env(
            "VOXEL_NATIVE_INSTANCE_LABEL",
            format!(
                "{} // {}",
                if join { "JOIN" } else { "SPECTATE" },
                snapshot.display_name
            ),
        );
    command
        .spawn()
        .map_err(|error| format!("could not launch spectator engine: {error}"))?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn launch_live_link_view(_snapshot: &MissionFeedSnapshot, _join: bool) -> Result<(), String> {
    Err("Mission Control viewer launch is native-only".into())
}

fn clean_agent_id(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(64)
        .collect()
}

fn clean_label(value: &str, max_chars: usize) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(not(target_arch = "wasm32"))]
fn system_time_epoch(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

#[cfg(not(target_arch = "wasm32"))]
fn system_time_nanos(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_normalization_rejects_invalid_schema_and_non_finite_telemetry() {
        let mut invalid_schema = MissionFeedSnapshot::default();
        invalid_schema.schema_version += 1;
        assert!(invalid_schema.normalize().is_none());

        let mut invalid_position = MissionFeedSnapshot::default();
        invalid_position.position[1] = f32::NAN;
        assert!(invalid_position.normalize().is_none());

        let mut valid = MissionFeedSnapshot::default();
        valid.agent_id = "Agent 01/unsafe".into();
        valid.task = "  inspect\nworld\0  ".into();
        let valid = valid.normalize().expect("valid bounded feed");
        assert_eq!(valid.agent_id, "Agent01unsafe");
        assert_eq!(valid.task, "inspectworld");
    }

    #[test]
    fn mission_wall_columns_adapt_at_documented_breakpoints() {
        assert_eq!(responsive_feed_columns(320.0), 1);
        assert_eq!(responsive_feed_columns(619.0), 1);
        assert_eq!(responsive_feed_columns(620.0), 2);
        assert_eq!(responsive_feed_columns(1049.0), 2);
        assert_eq!(responsive_feed_columns(1050.0), 3);
        assert_eq!(responsive_feed_columns(1480.0), 4);
        assert_eq!(responsive_feed_columns(3840.0), 4);
    }

    #[test]
    fn mission_focus_and_header_stack_before_controls_can_overlap() {
        assert!(mission_header_is_stacked(320.0));
        assert!(mission_header_is_stacked(759.0));
        assert!(!mission_header_is_stacked(760.0));
        assert!(focus_layout_is_stacked(320.0));
        assert!(focus_layout_is_stacked(819.0));
        assert!(!focus_layout_is_stacked(820.0));
    }

    #[test]
    fn shared_power_parity_is_explicit_and_old_feeds_fail_closed() {
        let old_or_missing = MissionFeedSnapshot::default();
        assert!(!old_or_missing.has_shared_power_parity());
        assert!(power_parity_label(&old_or_missing).contains("POWER MISMATCH"));

        let mut current = MissionFeedSnapshot::default();
        current.capability_schema_version =
            crate::agent_capabilities::AGENT_CAPABILITY_SCHEMA_VERSION;
        current.power_profile_id = crate::agent_capabilities::SHARED_POWER_PROFILE_ID.into();
        current.ron_fallback_ready = true;
        current.visual_capture_ready = true;
        assert!(current.has_shared_power_parity());
        assert!(power_parity_label(&current).starts_with("SHARED v1"));

        current.power_profile_id = "voxel-native/shared-agent-power/obsolete".into();
        assert!(!current.has_shared_power_parity());
    }

    #[test]
    fn fleet_identity_is_bounded_like_agent_identity() {
        let mut feed = MissionFeedSnapshot::default();
        feed.agent_id = "agent-01".into();
        feed.fleet_id = " shared fleet / unsafe ".into();
        let feed = feed.normalize().expect("otherwise valid feed");
        assert_eq!(feed.fleet_id, "sharedfleetunsafe");
    }

    #[test]
    fn live_link_launch_contract_accepts_only_codex_loopback_pairs() {
        assert!(validate_live_link_pair(
            Some("CODEX"),
            Some("127.0.0.1:48101"),
            Some("127.0.0.1:48102")
        ));
        assert!(!validate_live_link_pair(
            Some("USER"),
            Some("127.0.0.1:48101"),
            Some("127.0.0.1:48102")
        ));
        assert!(!validate_live_link_pair(
            Some("CODEX"),
            Some("0.0.0.0:48101"),
            Some("127.0.0.1:48102")
        ));
        assert!(!validate_live_link_pair(
            Some("CODEX"),
            Some("127.0.0.1:48101"),
            Some("127.0.0.1:48101")
        ));
    }
}
