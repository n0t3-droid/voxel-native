//! R93G Kanso theme tokens for calm, low-cost sci-fi surfaces.
//!
//! The original editor UI started as a hacker-terminal skin. The current
//! default keeps the same cheap immediate-mode implementation, but uses a
//! neutral ink foundation, restrained accent light, and explicit interaction
//! outlines so the engine reads as a focused tool instead of a debug panel.
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
    /// Default sakura rose accent for the Zen engine look.
    Sakura,
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
        ThemeColor::Sakura
    }
}

impl ThemeColor {
    /// Bright accent light. Values are vivid without clipping against dark UI.
    pub fn primary(self) -> egui::Color32 {
        match self {
            ThemeColor::Sakura => egui::Color32::from_rgb(0xFF, 0x8F, 0xB7),
            ThemeColor::Green => egui::Color32::from_rgb(0x65, 0xE6, 0xA1),
            ThemeColor::Amber => egui::Color32::from_rgb(0xFF, 0xC4, 0x66),
            ThemeColor::Blue => egui::Color32::from_rgb(0x65, 0xD8, 0xFF),
            ThemeColor::Red => egui::Color32::from_rgb(0xFF, 0x73, 0x7D),
        }
    }
    /// Dimmed primary, used for non-selected text + thin strokes.
    pub fn dim(self) -> egui::Color32 {
        match self {
            ThemeColor::Sakura => egui::Color32::from_rgb(0xA8, 0x62, 0x7D),
            ThemeColor::Green => egui::Color32::from_rgb(0x4A, 0x9C, 0x72),
            ThemeColor::Amber => egui::Color32::from_rgb(0xA8, 0x7B, 0x3F),
            ThemeColor::Blue => egui::Color32::from_rgb(0x48, 0x91, 0xA8),
            ThemeColor::Red => egui::Color32::from_rgb(0xA8, 0x50, 0x58),
        }
    }
    /// Even darker, used for disabled widgets and grid-like fills.
    pub fn deep(self) -> egui::Color32 {
        match self {
            ThemeColor::Sakura => egui::Color32::from_rgb(0x2D, 0x1B, 0x23),
            ThemeColor::Green => egui::Color32::from_rgb(0x16, 0x2A, 0x20),
            ThemeColor::Amber => egui::Color32::from_rgb(0x2D, 0x25, 0x18),
            ThemeColor::Blue => egui::Color32::from_rgb(0x16, 0x27, 0x2D),
            ThemeColor::Red => egui::Color32::from_rgb(0x2D, 0x19, 0x1B),
        }
    }
}

/// High-level visual language. `ThemeColor` is now only the accent;
/// this selector controls the shared surface system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeStyle {
    LiquidGlass,
    NeonToolbench,
    ClassicCrt,
}

impl Default for ThemeStyle {
    fn default() -> Self {
        Self::LiquidGlass
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

/// Geometry tokens shared by Kanso panels and interactive controls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisualTokens {
    pub corner_radius: f32,
    pub outline_width: f32,
    pub focus_width: f32,
    pub focus_gap: f32,
    pub hover_lift: f32,
}

pub const KANSO_VISUALS: VisualTokens = VisualTokens {
    corner_radius: 6.0,
    outline_width: 1.0,
    focus_width: 1.5,
    focus_gap: 2.0,
    hover_lift: 1.0,
};

/// Named finite transition durations. None of these imply a repaint loop once
/// the target value has been reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionRole {
    Feedback,
    State,
    Panel,
}

/// Timing tokens are public so non-widget UI can use the same cadence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionTokens {
    pub feedback_seconds: f32,
    pub state_seconds: f32,
    pub panel_seconds: f32,
}

pub const KANSO_MOTION: MotionTokens = MotionTokens {
    feedback_seconds: 0.10,
    state_seconds: 0.16,
    panel_seconds: 0.22,
};

impl MotionRole {
    pub const fn seconds(self) -> f32 {
        match self {
            Self::Feedback => KANSO_MOTION.feedback_seconds,
            Self::State => KANSO_MOTION.state_seconds,
            Self::Panel => KANSO_MOTION.panel_seconds,
        }
    }
}

const REDUCED_MOTION_ID: &str = "r93g_kanso_reduced_motion";

/// A clamped cubic-out curve for responsive controls with a quiet finish.
pub fn kanso_ease_out(value: f32) -> f32 {
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    1.0 - (1.0 - value).powi(3)
}

/// Set the context-wide accessibility preference once. Shared widgets read
/// this state automatically, so their call sites do not need a motion flag.
pub fn set_reduced_motion(ctx: &egui::Context, reduced: bool) {
    ctx.data_mut(|data| data.insert_temp(egui::Id::new(REDUCED_MOTION_ID), reduced));
    ctx.style_mut(|style| {
        style.animation_time = if reduced {
            0.0
        } else {
            MotionRole::Feedback.seconds()
        };
    });
    if reduced {
        ctx.clear_animations();
    }
}

/// Returns the explicit Kanso preference, falling back to egui's animation
/// duration so hosts that already disable animation are respected.
pub fn prefers_reduced_motion(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<bool>(egui::Id::new(REDUCED_MOTION_ID)))
        .unwrap_or_else(|| ctx.style().animation_time <= f32::EPSILON)
}

/// Resolve a token duration against the current accessibility preference.
pub fn motion_seconds(ctx: &egui::Context, role: MotionRole) -> f32 {
    if prefers_reduced_motion(ctx) {
        0.0
    } else {
        role.seconds()
    }
}

/// Animate a boolean only until it reaches its target. Reduced motion snaps
/// immediately and does not enqueue repaint requests.
pub fn animate_bool_finite(
    ctx: &egui::Context,
    id: egui::Id,
    target: bool,
    role: MotionRole,
) -> f32 {
    let duration = motion_seconds(ctx, role);
    if duration <= f32::EPSILON {
        if target {
            1.0
        } else {
            0.0
        }
    } else {
        ctx.animate_bool_with_time_and_easing(id, target, duration, kanso_ease_out)
    }
}

/// Semantic palette for every Toolbench surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Neutral separator and resting control outline.
    pub outline: egui::Color32,
    /// Higher-contrast structural outline for raised surfaces.
    pub outline_strong: egui::Color32,
    /// Keyboard focus ring; intentionally brighter than hover.
    pub focus: egui::Color32,
    /// Low-alpha outer focus ring used as a restrained neon halo.
    pub focus_glow: egui::Color32,
    /// Backward-compatible alias for the resting outline.
    pub stroke: egui::Color32,
    pub selected: egui::Color32,
    pub disabled: egui::Color32,
}

fn default_scanlines() -> bool {
    false
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
            scanlines: default_scanlines(),
            beeps: false,
        }
    }
}

fn mix_rgb(a: egui::Color32, b: egui::Color32, amount: f32) -> egui::Color32 {
    let amount = amount.clamp(0.0, 1.0);
    let [a_red, a_green, a_blue, _] = a.to_srgba_unmultiplied();
    let [b_red, b_green, b_blue, _] = b.to_srgba_unmultiplied();
    let mix =
        |left: u8, right: u8| (left as f32 + (right as f32 - left as f32) * amount).round() as u8;
    egui::Color32::from_rgb(
        mix(a_red, b_red),
        mix(a_green, b_green),
        mix(a_blue, b_blue),
    )
}

fn relative_luminance(color: egui::Color32) -> f32 {
    let [red, green, blue, _] = color.to_srgba_unmultiplied();
    let linear = |channel: u8| {
        let channel = channel as f32 / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
}

fn contrast_ratio(a: egui::Color32, b: egui::Color32) -> f32 {
    let a = relative_luminance(a);
    let b = relative_luminance(b);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

impl ThemeSettings {
    pub fn semantic(self) -> SemanticColors {
        let accent = self.color.primary();
        let (background, surface, surface_strong, text, text_muted, outline, outline_strong) =
            match self.style {
                ThemeStyle::LiquidGlass => (
                    egui::Color32::from_rgb(0x08, 0x0B, 0x0D),
                    egui::Color32::from_rgb(0x11, 0x17, 0x19),
                    egui::Color32::from_rgb(0x19, 0x22, 0x26),
                    egui::Color32::from_rgb(0xEA, 0xF2, 0xEE),
                    egui::Color32::from_rgb(0x98, 0xAA, 0xA2),
                    egui::Color32::from_rgb(0x34, 0x45, 0x3E),
                    egui::Color32::from_rgb(0x5B, 0x70, 0x67),
                ),
                ThemeStyle::NeonToolbench => (
                    egui::Color32::from_rgb(0x05, 0x09, 0x0C),
                    egui::Color32::from_rgb(0x0C, 0x14, 0x18),
                    egui::Color32::from_rgb(0x13, 0x21, 0x28),
                    egui::Color32::from_rgb(0xE6, 0xF1, 0xF3),
                    egui::Color32::from_rgb(0x91, 0xA8, 0xAE),
                    egui::Color32::from_rgb(0x29, 0x43, 0x4D),
                    egui::Color32::from_rgb(0x50, 0x70, 0x7C),
                ),
                ThemeStyle::ClassicCrt => (
                    egui::Color32::from_rgb(0x05, 0x08, 0x06),
                    egui::Color32::from_rgb(0x0B, 0x11, 0x0D),
                    egui::Color32::from_rgb(0x11, 0x1A, 0x14),
                    egui::Color32::from_rgb(0xDC, 0xE9, 0xDE),
                    egui::Color32::from_rgb(0x91, 0xA7, 0x96),
                    egui::Color32::from_rgb(0x2C, 0x3D, 0x31),
                    egui::Color32::from_rgb(0x51, 0x69, 0x58),
                ),
            };
        let focus = mix_rgb(accent, egui::Color32::WHITE, 0.18);
        let focus_glow = egui::Color32::from_rgba_unmultiplied(focus.r(), focus.g(), focus.b(), 72);
        let selected = mix_rgb(
            surface_strong,
            accent,
            if matches!(self.style, ThemeStyle::NeonToolbench) {
                0.24
            } else {
                0.18
            },
        );

        SemanticColors {
            background,
            surface,
            surface_strong,
            text,
            text_muted,
            success: egui::Color32::from_rgb(0x63, 0xD6, 0x9A),
            warning: egui::Color32::from_rgb(0xE8, 0xB8, 0x5C),
            danger: egui::Color32::from_rgb(0xF2, 0x6D, 0x78),
            info: egui::Color32::from_rgb(0x6C, 0xD5, 0xE8),
            accent,
            outline,
            outline_strong,
            focus,
            focus_glow,
            stroke: outline,
            selected,
            disabled: egui::Color32::from_rgb(0x62, 0x70, 0x6A),
        }
    }

    pub fn text_on(self, fill: egui::Color32) -> egui::Color32 {
        let dark = egui::Color32::from_rgb(0x04, 0x09, 0x0B);
        let light = self.semantic().text;
        if contrast_ratio(dark, fill) >= contrast_ratio(light, fill) {
            dark
        } else {
            light
        }
    }

    pub fn panel_fill(self, opacity: f32) -> egui::Color32 {
        let c = self.semantic().surface;
        let [red, green, blue, _] = c.to_srgba_unmultiplied();
        egui::Color32::from_rgba_unmultiplied(
            red,
            green,
            blue,
            (opacity.clamp(0.30, 0.96) * 255.0) as u8,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePreset {
    pub id: &'static str,
    pub name: &'static str,
    pub tagline: &'static str,
    pub style: ThemeStyle,
    pub color: ThemeColor,
}

pub const THEME_PRESETS: [ThemePreset; 4] = [
    ThemePreset {
        id: "sakura_zen",
        name: "Sakura Zen",
        tagline: "pink glass, torii dusk, soft neon petals",
        style: ThemeStyle::LiquidGlass,
        color: ThemeColor::Sakura,
    },
    ThemePreset {
        id: "neo_tokyo",
        name: "Neo Tokyo",
        tagline: "cyan city grid, bright sci-fi tooling",
        style: ThemeStyle::NeonToolbench,
        color: ThemeColor::Blue,
    },
    ThemePreset {
        id: "jade_garden",
        name: "Jade Garden",
        tagline: "green editor glass, calm forest workflow",
        style: ThemeStyle::LiquidGlass,
        color: ThemeColor::Green,
    },
    ThemePreset {
        id: "amber_dojo",
        name: "Amber Dojo",
        tagline: "warm CRT, compact low-end focus",
        style: ThemeStyle::ClassicCrt,
        color: ThemeColor::Amber,
    },
];

pub fn selected_theme_preset(settings: ThemeSettings) -> Option<&'static ThemePreset> {
    THEME_PRESETS
        .iter()
        .find(|preset| preset.style == settings.style && preset.color == settings.color)
}

pub fn draw_theme_preview_card(
    ui: &mut egui::Ui,
    preset: &ThemePreset,
    selected: bool,
    _time: f32,
) -> egui::Response {
    let desired = egui::vec2(188.0, 108.0);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
    let hovered = response.hovered();
    let theme = ThemeSettings {
        color: preset.color,
        style: preset.style,
        ..Default::default()
    };
    let colors = theme.semantic();
    let painter = ui.painter_at(rect.expand(5.0));
    let focused = response.has_focus();
    let glow = animate_bool_finite(
        ui.ctx(),
        response.id.with("theme_preview_hover"),
        hovered || focused,
        MotionRole::Feedback,
    );
    let card = rect.translate(egui::vec2(0.0, -glow * KANSO_VISUALS.hover_lift));

    painter.rect_filled(
        card,
        egui::Rounding::same(KANSO_VISUALS.corner_radius),
        colors.background,
    );
    let bands = 8;
    for i in 0..bands {
        let k = i as f32 / (bands - 1) as f32;
        let y0 = egui::lerp(card.top()..=card.bottom(), k);
        let y1 = egui::lerp(
            card.top()..=card.bottom(),
            ((i + 1) as f32 / bands as f32).min(1.0),
        );
        let band =
            egui::Rect::from_min_max(egui::pos2(card.left(), y0), egui::pos2(card.right(), y1));
        let fill = mix_rgb(colors.surface, colors.surface_strong, k);
        painter.rect_filled(band, 0.0, fill);
    }

    let accent = colors.accent;
    let dim = colors.outline;
    if glow > 0.01 {
        painter.rect_stroke(
            card.expand(KANSO_VISUALS.focus_gap + glow * 1.5),
            egui::Rounding::same(KANSO_VISUALS.corner_radius + 2.0),
            egui::Stroke::new(
                KANSO_VISUALS.focus_width,
                if focused {
                    colors.focus
                } else {
                    colors.focus_glow.linear_multiply(glow)
                },
            ),
        );
    }
    painter.rect_stroke(
        card,
        egui::Rounding::same(KANSO_VISUALS.corner_radius),
        egui::Stroke::new(
            if selected {
                KANSO_VISUALS.focus_width
            } else {
                KANSO_VISUALS.outline_width
            },
            if focused {
                colors.focus
            } else if selected || hovered {
                accent
            } else {
                dim
            },
        ),
    );

    let horizon = card.top() + card.height() * 0.62;
    for i in 0..5 {
        let y = horizon + i as f32 * 7.0;
        painter.line_segment(
            [
                egui::pos2(card.left() + 8.0, y),
                egui::pos2(card.right() - 8.0, y),
            ],
            egui::Stroke::new(0.8, dim.linear_multiply(0.55)),
        );
    }
    let gate_x = card.left() + 24.0;
    let gate_y = horizon - 2.0;
    painter.line_segment(
        [
            egui::pos2(gate_x - 11.0, gate_y),
            egui::pos2(gate_x + 28.0, gate_y - 5.0),
        ],
        egui::Stroke::new(2.0, accent),
    );
    painter.line_segment(
        [
            egui::pos2(gate_x - 6.0, gate_y + 2.0),
            egui::pos2(gate_x + 22.0, gate_y - 2.0),
        ],
        egui::Stroke::new(1.2, accent.linear_multiply(0.8)),
    );
    painter.line_segment(
        [
            egui::pos2(gate_x, gate_y + 1.0),
            egui::pos2(gate_x, card.bottom() - 14.0),
        ],
        egui::Stroke::new(1.4, accent),
    );
    painter.line_segment(
        [
            egui::pos2(gate_x + 18.0, gate_y - 1.0),
            egui::pos2(gate_x + 18.0, card.bottom() - 12.0),
        ],
        egui::Stroke::new(1.4, accent),
    );

    let orb_center = egui::pos2(card.right() - 38.0, card.top() + 34.0);
    painter.circle_filled(
        orb_center,
        20.0 + glow * 2.0,
        colors.selected.linear_multiply(0.75),
    );
    painter.circle_stroke(
        orb_center,
        24.0 + glow * 4.0,
        egui::Stroke::new(1.0, accent),
    );

    // Fixed signal points keep the preview distinctive without ambient motion.
    for i in 0..9 {
        let x_step = ((i * 37 + 11) % 97) as f32 / 96.0;
        let y_step = ((i * 53 + 17) % 89) as f32 / 88.0;
        let x = card.left() + 12.0 + x_step * (card.width() - 24.0);
        let y = card.top() + 10.0 + y_step * (card.height() - 28.0);
        painter.circle_filled(
            egui::pos2(x, y),
            1.25 + (i % 3) as f32 * 0.25,
            preset.color.primary().linear_multiply(0.58),
        );
    }

    let label_pos = egui::pos2(card.left() + 10.0, card.top() + 10.0);
    painter.text(
        label_pos,
        egui::Align2::LEFT_TOP,
        preset.name,
        egui::FontId::monospace(13.0),
        colors.text,
    );
    painter.text(
        label_pos + egui::vec2(0.0, 18.0),
        egui::Align2::LEFT_TOP,
        preset.tagline,
        egui::FontId::monospace(9.0),
        colors.text_muted,
    );

    response.on_hover_text(format!("{} - {}", preset.name, preset.tagline))
}

// ---------------------------------------------------------------------
// One-shot egui style application
// ---------------------------------------------------------------------

/// Amber warning / "danger zone" colour, shared across all variants so
/// the user always sees consistent semantic feedback regardless of
/// the chosen primary phosphor.
pub const AMBER: egui::Color32 = egui::Color32::from_rgb(0xE8, 0xB8, 0x5C);
/// Hard alert colour for irreversible / destructive actions.
pub const ALERT: egui::Color32 = egui::Color32::from_rgb(0xF2, 0x6D, 0x78);
/// Text colour on dark panels (slightly off-white to read as monochrome).
pub const TEXT: egui::Color32 = egui::Color32::from_rgb(0xEA, 0xF2, 0xEE);
/// Cool secondary accent for navigation / links.
pub const CYAN: egui::Color32 = egui::Color32::from_rgb(0x6C, 0xD5, 0xE8);
/// Install the Kanso theme on the given egui context. Idempotent —
/// safe to call once at startup or on every theme-color change.
pub fn apply_hacker_theme(ctx: &egui::Context, settings: ThemeSettings) {
    let primary = settings.color.primary();
    let colors = settings.semantic();
    let reduced_motion = prefers_reduced_motion(ctx);

    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = colors.background;
    visuals.panel_fill = colors.surface;
    visuals.window_stroke = egui::Stroke::new(KANSO_VISUALS.outline_width, colors.outline_strong);
    visuals.window_rounding = egui::Rounding::same(KANSO_VISUALS.corner_radius);
    visuals.menu_rounding = egui::Rounding::same(KANSO_VISUALS.corner_radius);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 20.0,
        spread: 0.0,
        color: egui::Color32::from_black_alpha(168),
    };
    visuals.popup_shadow = visuals.window_shadow;

    visuals.widgets.noninteractive.bg_fill = colors.surface;
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, colors.text);
    visuals.widgets.noninteractive.bg_stroke =
        egui::Stroke::new(KANSO_VISUALS.outline_width, colors.outline);
    visuals.widgets.noninteractive.rounding = egui::Rounding::same(KANSO_VISUALS.corner_radius);

    visuals.widgets.inactive.bg_fill = colors.surface;
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, colors.text);
    visuals.widgets.inactive.bg_stroke =
        egui::Stroke::new(KANSO_VISUALS.outline_width, colors.outline);
    visuals.widgets.inactive.rounding = egui::Rounding::same(KANSO_VISUALS.corner_radius);
    visuals.widgets.inactive.weak_bg_fill = colors.surface;

    visuals.widgets.hovered.bg_fill = colors.surface_strong;
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, colors.text);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(KANSO_VISUALS.focus_width, colors.accent);
    visuals.widgets.hovered.rounding = egui::Rounding::same(KANSO_VISUALS.corner_radius);
    visuals.widgets.hovered.weak_bg_fill = colors.surface_strong;

    visuals.widgets.active.bg_fill = colors.selected;
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, settings.text_on(colors.selected));
    visuals.widgets.active.bg_stroke = egui::Stroke::new(KANSO_VISUALS.focus_width, colors.focus);
    visuals.widgets.active.rounding = egui::Rounding::same(KANSO_VISUALS.corner_radius);
    visuals.widgets.active.weak_bg_fill = colors.selected;

    visuals.widgets.open.bg_fill = colors.surface_strong;
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, colors.text);
    visuals.widgets.open.bg_stroke = egui::Stroke::new(KANSO_VISUALS.focus_width, colors.focus);
    visuals.widgets.open.rounding = egui::Rounding::same(KANSO_VISUALS.corner_radius);

    visuals.selection.bg_fill = colors.selected;
    visuals.selection.stroke = egui::Stroke::new(KANSO_VISUALS.focus_width, colors.focus);
    visuals.hyperlink_color = primary;
    visuals.override_text_color = Some(colors.text);
    visuals.extreme_bg_color = colors.background;
    visuals.faint_bg_color = colors.surface_strong;
    visuals.code_bg_color = colors.surface_strong;
    visuals.warn_fg_color = colors.warning;
    visuals.error_fg_color = colors.danger;
    visuals.text_cursor.stroke = egui::Stroke::new(2.0, colors.focus);
    // A steady caret avoids an otherwise permanent visual timer.
    visuals.text_cursor.blink = false;

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
    style.animation_time = if reduced_motion {
        0.0
    } else {
        MotionRole::Panel.seconds()
    };
    ctx.set_style(style);
}

/// Premium command-deck frame shared by menus, editor and modal panels.
pub fn command_frame(theme: ThemeSettings) -> egui::Frame {
    let colors = theme.semantic();
    egui::Frame::none()
        .fill(colors.surface_strong)
        .stroke(egui::Stroke::new(
            KANSO_VISUALS.outline_width,
            colors.outline_strong,
        ))
        .inner_margin(egui::Margin::symmetric(18.0, 16.0))
        .rounding(egui::Rounding::same(KANSO_VISUALS.corner_radius))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 8.0),
            blur: 20.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(168),
        })
}

/// Static full-screen signal backdrop: gradient, perspective grid and fixed
/// data marks. It never schedules or depends on an ambient repaint loop.
#[allow(dead_code)]
pub fn draw_neural_backdrop(ctx: &egui::Context, theme: ThemeSettings, _time: f32) {
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
    let scroll = 0.0;
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
        let phase = col as f32 * 73.7 + 19.0;
        let y = screen.top() + phase.rem_euclid(screen.height() + 180.0) - 180.0;
        let alpha = 44 + ((col * 29).rem_euclid(58)) as u8;
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
    if matches!(theme.style, ThemeStyle::LiquidGlass) {
        let colors = theme.semantic();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("LIQUID GLASS // {label}"))
                    .color(colors.info)
                    .strong()
                    .monospace(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new("F3 // ESC")
                        .color(colors.text_muted)
                        .small()
                        .monospace(),
                );
            });
        });
        let rect = ui.max_rect();
        let y = ui.cursor().min.y + 2.0;
        ui.painter().line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(KANSO_VISUALS.outline_width, colors.outline),
        );
        ui.add_space(6.0);
        return;
    }

    let primary = theme.color.primary();
    let dim = theme.color.dim();
    // Keep the terminal cursor steady; focus animation belongs to controls.
    let cursor = "█";
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
    let colors = theme.semantic();
    let prefix = if selected { "█ " } else { "> " };
    let label = format!("{prefix}{text}");
    let color = if selected {
        theme.text_on(colors.selected)
    } else {
        colors.text
    };
    let fill = if selected {
        colors.selected
    } else {
        colors.surface
    };
    egui::Button::new(egui::RichText::new(label).color(color).monospace())
        .fill(fill)
        .stroke(egui::Stroke::new(
            if selected {
                KANSO_VISUALS.focus_width
            } else {
                KANSO_VISUALS.outline_width
            },
            if selected {
                colors.accent
            } else {
                colors.outline
            },
        ))
        .rounding(egui::Rounding::same(KANSO_VISUALS.corner_radius))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_presets_have_unique_ids_and_real_preview_copy() {
        let mut ids = std::collections::BTreeSet::new();
        for preset in THEME_PRESETS {
            assert!(!preset.id.is_empty());
            assert!(!preset.name.is_empty());
            assert!(!preset.tagline.is_empty());
            assert!(ids.insert(preset.id), "duplicate theme preset id");
        }
        assert!(THEME_PRESETS.len() >= 4);
    }

    #[test]
    fn default_theme_selects_sakura_zen_preset() {
        let preset = selected_theme_preset(ThemeSettings::default()).expect("default preset");
        assert_eq!(preset.id, "sakura_zen");
        assert_eq!(preset.color, ThemeColor::Sakura);
        assert_eq!(preset.style, ThemeStyle::LiquidGlass);
    }

    #[test]
    fn kanso_palette_keeps_text_and_focus_legible() {
        let styles = [
            ThemeStyle::LiquidGlass,
            ThemeStyle::NeonToolbench,
            ThemeStyle::ClassicCrt,
        ];
        let accents = [
            ThemeColor::Sakura,
            ThemeColor::Green,
            ThemeColor::Amber,
            ThemeColor::Blue,
            ThemeColor::Red,
        ];

        for style in styles {
            for color in accents {
                let theme = ThemeSettings {
                    style,
                    color,
                    ..Default::default()
                };
                let colors = theme.semantic();
                assert!(contrast_ratio(colors.text, colors.background) >= 7.0);
                assert!(contrast_ratio(colors.text_muted, colors.background) >= 4.5);
                assert!(contrast_ratio(colors.focus, colors.background) >= 3.0);
                assert_ne!(colors.outline, colors.accent);
                assert_ne!(colors.outline_strong, colors.focus);
                assert!(colors.focus_glow.a() < colors.focus.a());
                assert!(contrast_ratio(theme.text_on(colors.selected), colors.selected) >= 4.5);
            }
        }
    }

    #[test]
    fn motion_tokens_are_finite_ordered_and_clamped() {
        assert!(MotionRole::Feedback.seconds() < MotionRole::State.seconds());
        assert!(MotionRole::State.seconds() < MotionRole::Panel.seconds());
        assert_eq!(kanso_ease_out(f32::NAN), 0.0);
        assert_eq!(kanso_ease_out(-1.0), 0.0);
        assert_eq!(kanso_ease_out(0.0), 0.0);
        assert_eq!(kanso_ease_out(1.0), 1.0);
        assert_eq!(kanso_ease_out(2.0), 1.0);

        let mut previous = 0.0;
        for step in 0..=20 {
            let value = kanso_ease_out(step as f32 / 20.0);
            assert!(value >= previous);
            assert!((0.0..=1.0).contains(&value));
            previous = value;
        }
    }

    #[test]
    fn reduced_motion_snaps_shared_transitions() {
        let ctx = egui::Context::default();
        set_reduced_motion(&ctx, true);
        assert!(prefers_reduced_motion(&ctx));
        assert_eq!(motion_seconds(&ctx, MotionRole::Panel), 0.0);
        assert_eq!(
            animate_bool_finite(&ctx, egui::Id::new("on"), true, MotionRole::State),
            1.0
        );
        assert_eq!(
            animate_bool_finite(&ctx, egui::Id::new("off"), false, MotionRole::State),
            0.0
        );

        set_reduced_motion(&ctx, false);
        assert!(!prefers_reduced_motion(&ctx));
        assert_eq!(
            motion_seconds(&ctx, MotionRole::Feedback),
            KANSO_MOTION.feedback_seconds
        );
        assert_eq!(ctx.style().animation_time, KANSO_MOTION.feedback_seconds);
    }
}
