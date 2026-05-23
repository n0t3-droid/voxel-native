//! Hacker-terminal egui theme (phosphor-on-black).
//!
//! Replaces the earlier cyberpunk-neon look with a green/amber CRT
//! aesthetic: monospace fonts, tight corners, ASCII frame helpers,
//! a blinking-cursor header banner, a scanline overlay, and a status
//! bar. All in a single small module so [`crate::editor`] can call one
//! function (`apply_hacker_theme`) and use a handful of widget helpers.
//!
//! Performance budget: <0.10 ms/frame on Vega 8.
//!   * Theme application is one-shot at startup (no per-frame setup).
//!   * Scanline overlay = single tiled image quad / frame.
//!   * Status bar = ~5 short text labels.
//!   * No proportional fonts loaded -> small glyph atlas.
//!
//! The theme palette is selectable at runtime (green/amber/blue/red)
//! via [`ThemeColor`]. The active variant is stored persistently in
//! `WorldSettings.theme` so the look survives restarts.

use bevy_egui::egui;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------
// Persistent settings (mirrored into WorldSettings.theme)
// ---------------------------------------------------------------------

/// Colour variant for the phosphor look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeColor {
    /// Classic green CRT (#00FF66).
    Green,
    /// Amber 1980s monochrome.
    Amber,
    /// Blue ANSI hacker.
    Blue,
    /// Aggressive red alert.
    Red,
}

impl Default for ThemeColor {
    fn default() -> Self {
        ThemeColor::Green
    }
}

impl ThemeColor {
    /// Bright "primary" phosphor.
    pub fn primary(self) -> egui::Color32 {
        match self {
            ThemeColor::Green => egui::Color32::from_rgb(0x00, 0xFF, 0x66),
            ThemeColor::Amber => egui::Color32::from_rgb(0xFF, 0xB0, 0x00),
            ThemeColor::Blue => egui::Color32::from_rgb(0x40, 0xC8, 0xFF),
            ThemeColor::Red => egui::Color32::from_rgb(0xFF, 0x40, 0x40),
        }
    }
    /// Dimmed primary, used for non-selected text + thin strokes.
    pub fn dim(self) -> egui::Color32 {
        match self {
            ThemeColor::Green => egui::Color32::from_rgb(0x00, 0xB0, 0x50),
            ThemeColor::Amber => egui::Color32::from_rgb(0xB0, 0x70, 0x00),
            ThemeColor::Blue => egui::Color32::from_rgb(0x20, 0x80, 0xB0),
            ThemeColor::Red => egui::Color32::from_rgb(0xB0, 0x20, 0x20),
        }
    }
    /// Even darker, used for disabled widgets and grid-like fills.
    pub fn deep(self) -> egui::Color32 {
        match self {
            ThemeColor::Green => egui::Color32::from_rgb(0x00, 0x33, 0x15),
            ThemeColor::Amber => egui::Color32::from_rgb(0x33, 0x22, 0x00),
            ThemeColor::Blue => egui::Color32::from_rgb(0x10, 0x28, 0x38),
            ThemeColor::Red => egui::Color32::from_rgb(0x33, 0x10, 0x10),
        }
    }
}

/// High-level visual language. `ThemeColor` is now only the accent;
/// this selector controls the shared surface system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeStyle {
    NeonToolbench,
    ClassicCrt,
}

impl Default for ThemeStyle {
    fn default() -> Self {
        Self::NeonToolbench
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiDensity {
    Compact,
    Comfortable,
    Spacious,
}

impl Default for UiDensity {
    fn default() -> Self {
        Self::Comfortable
    }
}

impl UiDensity {
    pub fn item_spacing(self) -> egui::Vec2 {
        match self {
            Self::Compact => egui::vec2(5.0, 4.0),
            Self::Comfortable => egui::vec2(7.0, 6.0),
            Self::Spacious => egui::vec2(9.0, 8.0),
        }
    }

    pub fn button_padding(self) -> egui::Vec2 {
        match self {
            Self::Compact => egui::vec2(8.0, 4.0),
            Self::Comfortable => egui::vec2(10.0, 6.0),
            Self::Spacious => egui::vec2(12.0, 8.0),
        }
    }

    pub fn row_height(self) -> f32 {
        match self {
            Self::Compact => 30.0,
            Self::Comfortable => 36.0,
            Self::Spacious => 42.0,
        }
    }
}

/// Semantic palette for every Toolbench surface.
#[derive(Debug, Clone, Copy)]
pub struct SemanticColors {
    pub background: egui::Color32,
    pub surface: egui::Color32,
    pub surface_strong: egui::Color32,
    pub text: egui::Color32,
    pub text_muted: egui::Color32,
    pub success: egui::Color32,
    pub warning: egui::Color32,
    pub danger: egui::Color32,
    pub info: egui::Color32,
    pub accent: egui::Color32,
    pub stroke: egui::Color32,
    pub selected: egui::Color32,
    pub disabled: egui::Color32,
}

fn default_scanlines() -> bool {
    true
}

/// Persistent theme preferences. Lives inside [`crate::settings::WorldSettings`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ThemeSettings {
    #[serde(default)]
    pub color: ThemeColor,
    #[serde(default)]
    pub style: ThemeStyle,
    #[serde(default)]
    pub density: UiDensity,
    /// Subtle CRT scanline overlay over the editor panel.
    #[serde(default = "default_scanlines")]
    pub scanlines: bool,
    /// Click beeps (reserved for a later phase).
    #[serde(default)]
    pub beeps: bool,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            color: ThemeColor::default(),
            style: ThemeStyle::default(),
            density: UiDensity::default(),
            scanlines: true,
            beeps: false,
        }
    }
}

impl ThemeSettings {
    pub fn semantic(self) -> SemanticColors {
        let accent = self.color.primary();
        let dim = self.color.dim();
        let deep = self.color.deep();
        match self.style {
            ThemeStyle::NeonToolbench => SemanticColors {
                background: egui::Color32::from_rgba_premultiplied(2, 7, 10, 245),
                surface: egui::Color32::from_rgba_premultiplied(8, 17, 22, 224),
                surface_strong: egui::Color32::from_rgba_premultiplied(13, 26, 34, 240),
                text: egui::Color32::from_rgb(232, 248, 246),
                text_muted: egui::Color32::from_rgb(143, 178, 186),
                success: egui::Color32::from_rgb(86, 238, 146),
                warning: egui::Color32::from_rgb(0xFF, 0xB0, 0x00),
                danger: egui::Color32::from_rgb(0xFF, 0x30, 0x30),
                info: egui::Color32::from_rgb(0x32, 0xD7, 0xFF),
                accent,
                stroke: egui::Color32::from_rgba_unmultiplied(
                    accent.r(),
                    accent.g(),
                    accent.b(),
                    150,
                ),
                selected: egui::Color32::from_rgba_premultiplied(
                    accent.r() / 3,
                    accent.g() / 3,
                    accent.b() / 3,
                    230,
                ),
                disabled: egui::Color32::from_rgb(66, 78, 82),
            },
            ThemeStyle::ClassicCrt => SemanticColors {
                background: egui::Color32::from_rgba_premultiplied(0, 0, 0, 245),
                surface: egui::Color32::from_rgba_premultiplied(5, 10, 5, 242),
                surface_strong: egui::Color32::from_rgba_premultiplied(4, 12, 8, 242),
                text: egui::Color32::from_rgb(0xC8, 0xE8, 0xC8),
                text_muted: dim,
                success: egui::Color32::from_rgb(96, 245, 138),
                warning: egui::Color32::from_rgb(0xFF, 0xB0, 0x00),
                danger: egui::Color32::from_rgb(0xFF, 0x30, 0x30),
                info: egui::Color32::from_rgb(0x32, 0xD7, 0xFF),
                accent,
                stroke: dim,
                selected: deep,
                disabled: egui::Color32::from_rgb(48, 58, 48),
            },
        }
    }

    pub fn text_on(self, fill: egui::Color32) -> egui::Color32 {
        let lum =
            (0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32) / 255.0;
        if lum > 0.55 {
            egui::Color32::from_rgb(4, 10, 13)
        } else {
            self.semantic().text
        }
    }

    pub fn panel_fill(self, opacity: f32) -> egui::Color32 {
        let c = self.semantic().surface;
        egui::Color32::from_rgba_premultiplied(
            c.r(),
            c.g(),
            c.b(),
            (opacity.clamp(0.30, 0.96) * 255.0) as u8,
        )
    }
}

// ---------------------------------------------------------------------
// One-shot egui style application
// ---------------------------------------------------------------------

/// Amber warning / "danger zone" colour, shared across all variants so
/// the user always sees consistent semantic feedback regardless of
/// the chosen primary phosphor.
pub const AMBER: egui::Color32 = egui::Color32::from_rgb(0xFF, 0xB0, 0x00);
/// Hard alert colour for irreversible / destructive actions.
pub const ALERT: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x30, 0x30);
/// Text colour on dark panels (slightly off-white to read as monochrome).
pub const TEXT: egui::Color32 = egui::Color32::from_rgb(0xC8, 0xE8, 0xC8);
/// Cool secondary accent for navigation / links.
pub const CYAN: egui::Color32 = egui::Color32::from_rgb(0x32, 0xD7, 0xFF);
/// Pure black background.
pub const BG: egui::Color32 = egui::Color32::from_rgba_premultiplied(0, 0, 0, 245);
/// Panel fill — near-black with a hint of green so the look feels lit.
pub const PANEL: egui::Color32 = egui::Color32::from_rgba_premultiplied(5, 10, 5, 242);

/// Install the hacker theme on the given egui context. Idempotent —
/// safe to call once at startup or on every theme-color change.
pub fn apply_hacker_theme(ctx: &egui::Context, settings: ThemeSettings) {
    let primary = settings.color.primary();
    let dim = settings.color.dim();
    let deep = settings.color.deep();

    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = BG;
    visuals.panel_fill = PANEL;
    visuals.window_stroke = egui::Stroke::new(1.0, primary);
    visuals.window_rounding = egui::Rounding::same(6.0);
    visuals.menu_rounding = egui::Rounding::same(4.0);

    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, deep);
    visuals.widgets.noninteractive.rounding = egui::Rounding::same(4.0);

    visuals.widgets.inactive.bg_fill = deep;
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, dim);
    visuals.widgets.inactive.rounding = egui::Rounding::same(4.0);
    visuals.widgets.inactive.weak_bg_fill = deep;

    visuals.widgets.hovered.bg_fill = deep;
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, primary);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.5, primary);
    visuals.widgets.hovered.rounding = egui::Rounding::same(4.0);
    visuals.widgets.hovered.weak_bg_fill = deep;

    visuals.widgets.active.bg_fill = dim;
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::BLACK);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.5, primary);
    visuals.widgets.active.rounding = egui::Rounding::same(4.0);
    visuals.widgets.active.weak_bg_fill = dim;

    visuals.widgets.open.bg_fill = deep;
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, primary);
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, primary);
    visuals.widgets.open.rounding = egui::Rounding::same(4.0);

    visuals.selection.bg_fill = dim.linear_multiply(0.55);
    visuals.selection.stroke = egui::Stroke::new(1.0, primary);
    visuals.hyperlink_color = primary;
    visuals.override_text_color = Some(TEXT);
    visuals.extreme_bg_color = egui::Color32::BLACK;
    visuals.faint_bg_color = deep;

    ctx.set_visuals(visuals);

    // Force every text style to monospace.
    let mut style: egui::Style = (*ctx.style()).clone();
    let sizes: [(egui::TextStyle, f32); 5] = [
        (egui::TextStyle::Small, 11.0),
        (egui::TextStyle::Body, 13.0),
        (egui::TextStyle::Monospace, 13.0),
        (egui::TextStyle::Button, 13.0),
        (egui::TextStyle::Heading, 16.0),
    ];
    for (style_id, size) in sizes.iter() {
        style.text_styles.insert(
            style_id.clone(),
            egui::FontId::new(*size, egui::FontFamily::Monospace),
        );
    }
    style.spacing.item_spacing = settings.density.item_spacing();
    style.spacing.button_padding = settings.density.button_padding();
    style.spacing.slider_width = 220.0;
    style.spacing.window_margin = egui::Margin::symmetric(10.0, 8.0);
    style.spacing.interact_size = egui::vec2(32.0, settings.density.row_height());
    ctx.set_style(style);
}

/// Premium command-deck frame shared by menus, editor and modal panels.
pub fn command_frame(theme: ThemeSettings) -> egui::Frame {
    egui::Frame::none()
        .fill(egui::Color32::from_rgba_premultiplied(4, 8, 7, 242))
        .stroke(egui::Stroke::new(
            1.0,
            theme.color.primary().linear_multiply(0.82),
        ))
        .inner_margin(egui::Margin::symmetric(18.0, 16.0))
        .rounding(egui::Rounding::same(6.0))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 10.0),
            blur: 30.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(190),
        })
}

/// Animated full-screen hacker backdrop: gradient, perspective grid,
/// scanlines and deterministic data rain. No textures, no allocations
/// outside the small formatted data glyph strings.
pub fn draw_neural_backdrop(ctx: &egui::Context, theme: ThemeSettings, time: f32) {
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    let primary = theme.color.primary();
    let dim = theme.color.dim();

    let bands = 36;
    for i in 0..bands {
        let k = i as f32 / (bands - 1) as f32;
        let r = (2.0 + (1.0 - k) * 4.0) as u8;
        let g = (5.0 + (1.0 - k) * 14.0) as u8;
        let b = (7.0 + (1.0 - k) * 10.0) as u8;
        let rect = egui::Rect::from_min_size(
            egui::pos2(screen.left(), screen.top() + k * screen.height()),
            egui::vec2(screen.width(), screen.height() / bands as f32 + 1.0),
        );
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(r, g, b));
    }

    let horizon_y = screen.top() + screen.height() * 0.60;
    let grid_alpha = 54;
    let grid =
        egui::Color32::from_rgba_unmultiplied(primary.r(), primary.g(), primary.b(), grid_alpha);
    let scroll = (time * 0.22).fract();
    for i in 0..18 {
        let k = (i as f32 + scroll) / 18.0;
        let y = horizon_y + k * k * (screen.bottom() - horizon_y);
        let alpha = ((1.0 - k) * 120.0) as u8;
        painter.line_segment(
            [egui::pos2(screen.left(), y), egui::pos2(screen.right(), y)],
            egui::Stroke::new(
                1.0,
                egui::Color32::from_rgba_unmultiplied(primary.r(), primary.g(), primary.b(), alpha),
            ),
        );
    }
    let vp = egui::pos2(screen.center().x, horizon_y);
    for i in -12..=12 {
        let x = screen.center().x + i as f32 * (screen.width() / 12.0);
        painter.line_segment(
            [vp, egui::pos2(x, screen.bottom())],
            egui::Stroke::new(1.0, grid),
        );
    }

    let columns = (screen.width() / 46.0).ceil() as i32;
    for col in 0..columns {
        let x = screen.left() + col as f32 * 46.0 + 14.0;
        let phase = time * (18.0 + (col % 5) as f32 * 2.0) + col as f32 * 13.7;
        let y = screen.top() + phase.rem_euclid(screen.height() + 180.0) - 180.0;
        let alpha = 30 + ((phase.sin() * 0.5 + 0.5) * 75.0) as u8;
        let bits = if col % 3 == 0 {
            "1011"
        } else if col % 3 == 1 {
            "0110"
        } else {
            "1101"
        };
        painter.text(
            egui::pos2(x, y),
            egui::Align2::CENTER_TOP,
            bits,
            egui::FontId::monospace(10.0),
            egui::Color32::from_rgba_unmultiplied(dim.r(), dim.g(), dim.b(), alpha),
        );
    }

    let mut y = screen.top();
    while y < screen.bottom() {
        painter.line_segment(
            [egui::pos2(screen.left(), y), egui::pos2(screen.right(), y)],
            egui::Stroke::new(1.0, egui::Color32::from_black_alpha(34)),
        );
        y += 4.0;
    }

    let top = egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, screen.top() + 96.0));
    painter.rect_filled(top, 0.0, egui::Color32::from_black_alpha(70));
    let bottom = egui::Rect::from_min_max(
        egui::pos2(screen.min.x, screen.bottom() - 140.0),
        screen.max,
    );
    painter.rect_filled(bottom, 0.0, egui::Color32::from_black_alpha(92));
    let left =
        egui::Rect::from_min_max(screen.min, egui::pos2(screen.left() + 120.0, screen.max.y));
    let right =
        egui::Rect::from_min_max(egui::pos2(screen.right() - 120.0, screen.min.y), screen.max);
    painter.rect_filled(left, 0.0, egui::Color32::from_black_alpha(58));
    painter.rect_filled(right, 0.0, egui::Color32::from_black_alpha(58));
}

/// Compact, accessible status pill for dense command surfaces.
pub fn metric_pill(ui: &mut egui::Ui, theme: ThemeSettings, label: &str, value: &str) {
    egui::Frame::none()
        .fill(egui::Color32::from_rgba_premultiplied(0, 0, 0, 150))
        .stroke(egui::Stroke::new(1.0, theme.color.dim()))
        .rounding(egui::Rounding::same(4.0))
        .inner_margin(egui::Margin::symmetric(8.0, 5.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(label)
                        .color(theme.color.dim())
                        .small()
                        .monospace(),
                );
                ui.label(
                    egui::RichText::new(value)
                        .color(theme.color.primary())
                        .strong()
                        .monospace(),
                );
            });
        });
}

// ---------------------------------------------------------------------
// Drawing helpers
// ---------------------------------------------------------------------

/// Hacker-style header banner with blinking block cursor:
///   `▓▓▓ [ ROOT@VOXEL-NATIVE:~$ EDITOR ] █ ▓▓▓`.
pub fn draw_banner(ui: &mut egui::Ui, theme: ThemeSettings, label: &str) {
    let primary = theme.color.primary();
    let dim = theme.color.dim();
    // ~2 Hz blink driven directly off egui's input time so we don't
    // need a Bevy Time resource to render.
    let t = ui.input(|i| i.time);
    let blink = (t * 2.0).sin() > 0.0;
    let cursor = if blink { "█" } else { " " };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("▓▓▓").color(dim).monospace());
        ui.label(
            egui::RichText::new(format!("[ ROOT@VOXEL-NATIVE:~$ {label} ]"))
                .color(primary)
                .strong()
                .monospace(),
        );
        ui.label(egui::RichText::new(cursor).color(primary).monospace());
        ui.label(egui::RichText::new("▓▓▓").color(dim).monospace());
        // Right-aligned tiny hint.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new("F3 // ESC")
                    .color(dim)
                    .small()
                    .monospace(),
            );
        });
    });
    // Thin underline.
    let rect = ui.max_rect();
    let p = ui.painter();
    let y = ui.cursor().min.y + 2.0;
    p.line_segment(
        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
        egui::Stroke::new(1.0, dim),
    );
    ui.add_space(6.0);
}

/// ASCII section header: `╭─[ TITLE ]──...──╮`. Cheap — one `Label`.
pub fn section_box(ui: &mut egui::Ui, theme: ThemeSettings, title: &str) {
    let primary = theme.color.primary();
    let dim = theme.color.dim();
    let avail = ui.available_width();
    // Each monospace glyph is ~7.5 px at 13 px font; estimate fill count.
    let title_run = format!("─[ {} ]", title);
    let glyph = 7.5_f32;
    let total_chars = (avail / glyph).max(8.0) as usize;
    let used = title_run.chars().count() + 2; // ╭ + ╮
    let pad = total_chars.saturating_sub(used);
    let line = format!("╭{}{}╮", title_run, "─".repeat(pad));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        // Split so the title pops in `primary`, the box rules stay `dim`.
        let pre = format!("╭─[ ");
        let post_dashes = "─".repeat(pad);
        let post = format!(" ]{}╮", post_dashes);
        ui.label(egui::RichText::new(pre).color(dim).monospace());
        ui.label(
            egui::RichText::new(title)
                .color(primary)
                .strong()
                .monospace(),
        );
        ui.label(egui::RichText::new(post).color(dim).monospace());
    });
    let _ = line; // (single-string fallback retained for future log/debug)
}

/// Status bar string assembled from running game state. The data is
/// sampled from already-existing resources; this function only formats.
pub fn status_line(
    fps: f32,
    chunks: usize,
    seed: u32,
    time_of_day_h: f32,
    mem_mb: Option<u32>,
) -> String {
    let h = time_of_day_h.floor() as i32;
    let m = ((time_of_day_h - h as f32) * 60.0).floor() as i32;
    let mem = mem_mb
        .map(|m| format!("[MEM {:>4}M]", m))
        .unwrap_or_else(|| String::from("[MEM ----]"));
    format!(
        "[FPS {:>3.0}] {} [CHUNKS {:>4}] [SEED 0x{:08X}] [TIME {:02}:{:02}]",
        fps, mem, chunks, seed, h, m
    )
}

/// Render the status line as a full-width strip at the bottom of the panel.
pub fn draw_status_bar(ui: &mut egui::Ui, theme: ThemeSettings, text: &str) {
    let dim = theme.color.dim();
    let primary = theme.color.primary();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("└").color(dim).monospace());
        ui.label(egui::RichText::new(text).color(primary).small().monospace());
    });
}

/// Paint a CRT scanline overlay across the rect, alpha ≈ 0.05. Uses
/// straight horizontal lines drawn in the foreground layer; cheaper
/// than uploading a tiled texture for the area we cover.
pub fn paint_scanlines(ctx: &egui::Context, rect: egui::Rect, theme: ThemeSettings) {
    if !theme.scanlines {
        return;
    }
    let dim = theme.color.dim().linear_multiply(0.10);
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("editor_scanlines"));
    let painter = ctx.layer_painter(layer);
    let mut y = rect.top().floor();
    while y < rect.bottom() {
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, dim),
        );
        y += 3.0;
    }
}

/// Hacker button styled with `>` prefix for non-selected, `█` for selected.
pub fn term_button(text: &str, selected: bool, theme: ThemeSettings) -> egui::Button<'static> {
    let primary = theme.color.primary();
    let prefix = if selected { "█ " } else { "> " };
    let label = format!("{prefix}{text}");
    let color = if selected {
        egui::Color32::BLACK
    } else {
        primary
    };
    let fill = if selected {
        primary
    } else {
        theme.color.deep()
    };
    egui::Button::new(egui::RichText::new(label).color(color).monospace())
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, theme.color.dim()))
        .rounding(egui::Rounding::ZERO)
}
