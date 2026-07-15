//! Shared Neon Toolbench widgets for egui surfaces.
//!
//! The helpers here are intentionally small: they keep colour, spacing,
//! radius and button states consistent without introducing a new UI
//! framework or asset dependency.

use std::time::Duration;

use bevy_egui::egui;

use crate::icons::{paint_icon, Icon};
use crate::theme::{
    allows_continuous_motion, animate_bool_finite, paint_focus_outline, MotionRole, SemanticColors,
    ThemeSettings, KANSO_LAYOUT, KANSO_VISUALS,
};

const ACTIVITY_REPAINT_INTERVAL: Duration = Duration::from_millis(33);
const STATIC_ACTIVITY_PHASE: f32 = 0.125;
const STATIC_ACTIVITY_PULSE: f32 = 0.68;

fn alpha_u8(alpha: f32) -> u8 {
    let alpha = if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (alpha * 255.0).round() as u8
}

fn with_alpha(color: egui::Color32, alpha: f32) -> egui::Color32 {
    let [red, green, blue, _] = color.to_srgba_unmultiplied();
    egui::Color32::from_rgba_unmultiplied(red, green, blue, alpha_u8(alpha))
}

fn mix_color(a: egui::Color32, b: egui::Color32, amount: f32) -> egui::Color32 {
    let amount = if amount.is_finite() {
        amount.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let a = a.to_srgba_unmultiplied();
    let b = b.to_srgba_unmultiplied();
    let mix =
        |left: u8, right: u8| (left as f32 + (right as f32 - left as f32) * amount).round() as u8;
    egui::Color32::from_rgba_unmultiplied(
        mix(a[0], b[0]),
        mix(a[1], b[1]),
        mix(a[2], b[2]),
        mix(a[3], b[3]),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InteractionState {
    enabled: bool,
    selected: bool,
    hovered: bool,
    focused: bool,
    pressed: bool,
}

impl InteractionState {
    fn from_response(response: &egui::Response, selected: bool) -> Self {
        let enabled = response.enabled();
        Self {
            enabled,
            selected,
            hovered: enabled && response.hovered(),
            focused: enabled && response.has_focus(),
            pressed: enabled && (response.is_pointer_button_down_on() || response.clicked()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ControlVisuals {
    fill: egui::Color32,
    text: egui::Color32,
    icon: egui::Color32,
    outline: egui::Color32,
    outline_width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionTone {
    Standard,
    Primary,
    Danger,
}

fn control_visuals(
    theme: ThemeSettings,
    state: InteractionState,
    motion: ControlMotion,
) -> ControlVisuals {
    let colors = theme.semantic();
    if !state.enabled {
        return ControlVisuals {
            fill: colors.surface_disabled,
            text: colors.text_disabled,
            icon: colors.text_disabled,
            outline: colors.outline_disabled,
            outline_width: KANSO_VISUALS.outline_width,
        };
    }

    let hover_or_focus = motion.hover.max(motion.focus);
    let active = motion.selection.max(motion.press);
    let hover_fill = mix_color(colors.surface, colors.surface_hover, hover_or_focus);
    let selected_fill = mix_color(hover_fill, colors.selected, motion.selection);
    let fill = mix_color(selected_fill, colors.surface_active, motion.press);
    let selected_text = theme.text_on(colors.selected);
    let active_text = theme.text_on(colors.surface_active);
    let text = mix_color(
        mix_color(colors.text, selected_text, motion.selection),
        active_text,
        motion.press,
    );
    let outline = mix_color(
        mix_color(colors.outline, colors.outline_hover, hover_or_focus),
        colors.outline_active,
        active,
    );
    let emphasis = hover_or_focus.max(active);

    ControlVisuals {
        fill,
        text,
        icon: mix_color(colors.accent, colors.focus, motion.focus.max(active)),
        outline,
        outline_width: KANSO_VISUALS.outline_width
            + (KANSO_VISUALS.focus_width - KANSO_VISUALS.outline_width) * emphasis,
    }
}

fn tone_control_visuals(
    theme: ThemeSettings,
    state: InteractionState,
    motion: ControlMotion,
    tone: ActionTone,
) -> ControlVisuals {
    let mut visuals = control_visuals(theme, state, motion);
    if !state.enabled || !matches!(tone, ActionTone::Danger) {
        return visuals;
    }

    let colors = theme.semantic();
    let engagement = motion.hover.max(motion.focus).max(motion.press);
    visuals.fill = mix_color(visuals.fill, colors.danger, 0.03 + motion.press * 0.10);
    visuals.text = mix_color(colors.danger, colors.text, motion.press * 0.18);
    visuals.icon = mix_color(colors.danger, colors.focus, motion.focus * 0.45);
    visuals.outline = mix_color(visuals.outline, colors.danger, 0.52 + engagement * 0.38);
    visuals
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ControlMotion {
    hover: f32,
    focus: f32,
    press: f32,
    selection: f32,
    spatial_motion: bool,
}

impl ControlMotion {
    fn sample(ui: &egui::Ui, response: &egui::Response, selected: bool, role: MotionRole) -> Self {
        let enabled = response.enabled();
        let hover = animate_bool_finite(
            ui.ctx(),
            response.id.with("r93g_hover"),
            enabled && response.hovered(),
            role,
        );
        let focus = animate_bool_finite(
            ui.ctx(),
            response.id.with("r93g_focus"),
            enabled && response.has_focus(),
            MotionRole::Feedback,
        );
        let press = animate_bool_finite(
            ui.ctx(),
            response.id.with("r93g_press"),
            enabled && (response.is_pointer_button_down_on() || response.clicked()),
            MotionRole::Press,
        );
        let selection = animate_bool_finite(
            ui.ctx(),
            response.id.with("r93g_selection"),
            enabled && selected,
            MotionRole::State,
        );
        let spatial_motion = allows_continuous_motion(ui.ctx());
        if enabled {
            Self {
                hover,
                focus,
                press,
                selection,
                spatial_motion,
            }
        } else {
            Self {
                hover: 0.0,
                focus: 0.0,
                press: 0.0,
                selection: 0.0,
                spatial_motion,
            }
        }
    }

    fn paint_rect(self, rect: egui::Rect) -> egui::Rect {
        if !self.spatial_motion {
            return rect;
        }
        let offset = -self.hover * (1.0 - self.press) * KANSO_VISUALS.hover_lift
            + self.press * KANSO_LAYOUT.press_depth;
        rect.translate(egui::vec2(0.0, offset))
    }

    #[cfg(test)]
    fn target(state: InteractionState) -> Self {
        let enabled = if state.enabled { 1.0 } else { 0.0 };
        Self {
            hover: enabled * state.hovered as u8 as f32,
            focus: enabled * state.focused as u8 as f32,
            press: enabled * state.pressed as u8 as f32,
            selection: enabled * state.selected as u8 as f32,
            spatial_motion: false,
        }
    }
}

fn paint_control_outline(
    painter: &egui::Painter,
    rect: egui::Rect,
    colors: SemanticColors,
    visuals: ControlVisuals,
    focus_amount: f32,
) {
    paint_focus_outline(painter, rect, colors, focus_amount);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(KANSO_VISUALS.corner_radius),
        egui::Stroke::new(visuals.outline_width, visuals.outline),
    );
}

fn paint_control_shell(
    painter: &egui::Painter,
    rect: egui::Rect,
    colors: SemanticColors,
    visuals: ControlVisuals,
    focus_amount: f32,
) {
    painter.rect_filled(
        rect,
        egui::Rounding::same(KANSO_VISUALS.corner_radius),
        visuals.fill,
    );
    paint_control_outline(painter, rect, colors, visuals, focus_amount);
}

pub fn toolbench_frame(theme: ThemeSettings) -> egui::Frame {
    let colors = theme.semantic();
    egui::Frame::none()
        .fill(with_alpha(colors.surface_strong, 0.96))
        .stroke(egui::Stroke::new(
            KANSO_VISUALS.outline_width,
            with_alpha(colors.outline_strong, 0.92),
        ))
        .inner_margin(egui::Margin::symmetric(18.0, 16.0))
        .rounding(egui::Rounding::same(KANSO_VISUALS.corner_radius))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 6.0),
            blur: 12.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(144),
        })
}

fn surface_frame(theme: ThemeSettings, emphasis: f32) -> egui::Frame {
    let colors = theme.semantic();
    let emphasis = if emphasis.is_finite() {
        emphasis.clamp(0.0, 1.0)
    } else {
        0.0
    };
    egui::Frame::none()
        .fill(with_alpha(
            mix_color(colors.surface, colors.surface_hover, emphasis * 0.42),
            0.94,
        ))
        .stroke(egui::Stroke::new(
            KANSO_VISUALS.outline_width,
            with_alpha(
                mix_color(colors.outline, colors.outline_hover, emphasis),
                0.92,
            ),
        ))
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
        .rounding(egui::Rounding::same(KANSO_VISUALS.corner_radius))
}

fn show_surface_panel<R>(
    ui: &mut egui::Ui,
    theme: ThemeSettings,
    emphasis: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let response = surface_frame(theme, emphasis).show(ui, add_contents);
    if emphasis > 0.001 {
        let colors = theme.semantic();
        let rect = response.response.rect;
        ui.painter().line_segment(
            [
                egui::pos2(rect.left() + 12.0, rect.top() + 1.0),
                egui::pos2(rect.right() - 12.0, rect.top() + 1.0),
            ],
            egui::Stroke::new(1.0, with_alpha(colors.accent, emphasis * 0.62)),
        );
    }
    response
}

pub fn surface_panel<R>(
    ui: &mut egui::Ui,
    theme: ThemeSettings,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    show_surface_panel(ui, theme, 0.0, add_contents)
}

/// Surface panel with finite paint-only emphasis. Margins and content bounds
/// are identical to [`surface_panel`], so transitions cannot shift layout.
pub fn surface_panel_animated<R>(
    ui: &mut egui::Ui,
    theme: ThemeSettings,
    animation_id: egui::Id,
    emphasized: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let emphasis = animate_bool_finite(
        ui.ctx(),
        animation_id.with("r93g_panel_emphasis"),
        emphasized,
        MotionRole::Panel,
    );
    show_surface_panel(ui, theme, emphasis, add_contents)
}

pub fn hud_panel(
    painter: &egui::Painter,
    rect: egui::Rect,
    theme: ThemeSettings,
    opacity: f32,
    accent: egui::Color32,
) {
    let colors = theme.semantic();
    let opacity = opacity.clamp(0.28, 0.94);
    let rounding = egui::Rounding::same(KANSO_VISUALS.corner_radius);
    let base = with_alpha(colors.surface_strong, opacity * 0.92);
    let deep = with_alpha(colors.background, opacity * 0.72);
    let top_sheen = with_alpha(colors.text, opacity * 0.06);

    painter.rect_filled(rect, rounding, base);
    painter.rect_filled(
        rect.shrink(1.0),
        egui::Rounding::same(KANSO_VISUALS.corner_radius - 1.0),
        deep,
    );

    let top = egui::Rect::from_min_max(
        rect.left_top() + egui::vec2(1.0, 1.0),
        egui::pos2(rect.right() - 1.0, rect.top() + rect.height() * 0.42),
    );
    painter.rect_filled(top, rounding, top_sheen);

    let rim = with_alpha(colors.outline_strong, opacity);
    let signal = with_alpha(accent, opacity * 0.82);
    painter.rect_stroke(
        rect,
        rounding,
        egui::Stroke::new(KANSO_VISUALS.outline_width, rim),
    );

    painter.line_segment(
        [
            egui::pos2(rect.left() + 12.0, rect.top() + 2.0),
            egui::pos2(rect.right() - 12.0, rect.top() + 2.0),
        ],
        egui::Stroke::new(1.0, signal),
    );

    let tick = rect.width().min(rect.height()).min(24.0) * 0.42;
    let stroke = egui::Stroke::new(1.25, signal);
    let l = rect.left() + 5.0;
    let r = rect.right() - 5.0;
    let t = rect.top() + 5.0;
    let b = rect.bottom() - 5.0;
    painter.line_segment([egui::pos2(l, t), egui::pos2(l + tick, t)], stroke);
    painter.line_segment([egui::pos2(l, t), egui::pos2(l, t + tick)], stroke);
    painter.line_segment([egui::pos2(r, b), egui::pos2(r - tick, b)], stroke);
    painter.line_segment([egui::pos2(r, b), egui::pos2(r, b - tick)], stroke);
}

pub fn icon_action(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    selected: bool,
    theme: ThemeSettings,
) -> egui::Response {
    let width = (label.chars().count() as f32 * 8.0 + 42.0).clamp(
        KANSO_LAYOUT.icon_action_min_width,
        KANSO_LAYOUT.icon_action_max_width,
    );
    icon_action_sized(ui, icon, label, selected, width, theme)
}

/// Width-stable variant for dynamic labels or aligned action rows.
pub fn icon_action_sized(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    selected: bool,
    width: f32,
    theme: ThemeSettings,
) -> egui::Response {
    icon_action_sized_tone(
        ui,
        icon,
        label,
        selected,
        width,
        theme,
        ActionTone::Standard,
    )
}

fn icon_action_sized_tone(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    selected: bool,
    width: f32,
    theme: ThemeSettings,
    tone: ActionTone,
) -> egui::Response {
    let colors = theme.semantic();
    let height = theme.density.row_height();
    let width = if width.is_finite() {
        width.max(KANSO_LAYOUT.icon_action_min_width)
    } else {
        KANSO_LAYOUT.icon_action_min_width
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let state = InteractionState::from_response(&response, selected);
    let motion = ControlMotion::sample(ui, &response, selected, MotionRole::Feedback);
    let paint_rect = motion.paint_rect(rect);
    let visuals = tone_control_visuals(theme, state, motion, tone);
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 4.0));
    paint_control_shell(&painter, paint_rect, colors, visuals, motion.focus);
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(paint_rect.left() + 20.0, paint_rect.center().y),
        egui::vec2(20.0, 20.0),
    );
    paint_icon(&painter, icon_rect, icon, visuals.icon);
    painter.text(
        egui::pos2(paint_rect.left() + 38.0, paint_rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(12.0),
        visuals.text,
    );
    response.on_hover_text(format!("{label} - {}", icon.tooltip_de()))
}

fn fitted_monospace_size(text: &str, max_width: f32, preferred: f32, minimum: f32) -> f32 {
    if text.is_empty() || !max_width.is_finite() || max_width <= 0.0 {
        return preferred.max(minimum);
    }
    let estimated_width = text.chars().count() as f32 * preferred * 0.61;
    if estimated_width <= max_width {
        preferred
    } else {
        (preferred * max_width / estimated_width).clamp(minimum, preferred)
    }
}

/// Full-width command control with the same finite state transitions as icon
/// actions. The allocation is stable; hover and press only alter paint.
pub fn command_action(
    ui: &mut egui::Ui,
    label: &str,
    detail: Option<&str>,
    tone: ActionTone,
    height: f32,
    theme: ThemeSettings,
) -> egui::Response {
    let width = ui.available_width().max(1.0);
    let height = if height.is_finite() {
        height.clamp(30.0, 72.0)
    } else {
        theme.density.row_height()
    };
    let selected = matches!(tone, ActionTone::Primary);
    let colors = theme.semantic();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let state = InteractionState::from_response(&response, selected);
    let motion = ControlMotion::sample(ui, &response, selected, MotionRole::Feedback);
    let paint_rect = motion.paint_rect(rect);
    let visuals = tone_control_visuals(theme, state, motion, tone);
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 4.0));
    paint_control_shell(&painter, paint_rect, colors, visuals, motion.focus);

    let text_painter = painter.with_clip_rect(paint_rect.shrink(8.0));
    let max_text_width = (paint_rect.width() - 24.0).max(24.0);
    let has_detail = detail.is_some_and(|value| !value.is_empty()) && height >= 44.0;
    let label_size = fitted_monospace_size(label, max_text_width, 13.5, 10.0);
    let label_y = paint_rect.center().y - if has_detail { 8.0 } else { 0.0 };
    text_painter.text(
        egui::pos2(paint_rect.center().x, label_y),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::monospace(label_size),
        visuals.text,
    );
    if has_detail {
        let detail = detail.unwrap_or_default();
        let detail_size = fitted_monospace_size(detail, max_text_width, 10.0, 8.5);
        let detail_color = if selected {
            with_alpha(visuals.text, 0.76)
        } else {
            colors.text_muted
        };
        text_painter.text(
            egui::pos2(paint_rect.center().x, paint_rect.center().y + 10.0),
            egui::Align2::CENTER_CENTER,
            detail,
            egui::FontId::monospace(detail_size),
            detail_color,
        );
    }

    if let Some(detail) = detail.filter(|value| !value.is_empty()) {
        response.on_hover_text(format!("{label} - {detail}"))
    } else {
        response.on_hover_text(label)
    }
}

/// Text-only segmented choice with fixed geometry and keyboard focus.
pub fn choice_chip_sized(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    width: f32,
    theme: ThemeSettings,
) -> egui::Response {
    let colors = theme.semantic();
    let width = if width.is_finite() {
        width.max(64.0)
    } else {
        92.0
    };
    let size = egui::vec2(width, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let state = InteractionState::from_response(&response, selected);
    let motion = ControlMotion::sample(ui, &response, selected, MotionRole::Feedback);
    let paint_rect = motion.paint_rect(rect);
    let visuals = control_visuals(theme, state, motion);
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 3.0));
    paint_control_shell(&painter, paint_rect, colors, visuals, motion.focus);
    painter.with_clip_rect(paint_rect.shrink(6.0)).text(
        paint_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::monospace(fitted_monospace_size(
            label,
            paint_rect.width() - 14.0,
            11.0,
            8.5,
        )),
        visuals.text,
    );
    response.on_hover_text(label)
}

/// Two-band material swatch. This uses two fills instead of a per-pixel or
/// many-band gradient while preserving enough depth for material recognition.
pub fn paint_material_swatch(
    painter: &egui::Painter,
    rect: egui::Rect,
    color: egui::Color32,
    radius: f32,
) {
    let radius = radius.max(0.0).min(rect.width().min(rect.height()) * 0.5);
    let top = mix_color(color, egui::Color32::WHITE, 0.14);
    let bottom = mix_color(color, egui::Color32::BLACK, 0.24);
    let split = rect.center().y;
    painter.rect_filled(
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, split)),
        egui::Rounding {
            nw: radius,
            ne: radius,
            sw: 0.0,
            se: 0.0,
        },
        top,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(rect.min.x, split), rect.max),
        egui::Rounding {
            nw: 0.0,
            ne: 0.0,
            sw: radius,
            se: radius,
        },
        bottom,
    );
    painter.line_segment(
        [
            egui::pos2(rect.left() + 4.0, rect.top() + 2.0),
            egui::pos2(rect.right() - 4.0, rect.top() + 2.0),
        ],
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(54)),
    );
}

pub fn swatch_card(
    ui: &mut egui::Ui,
    color: egui::Color32,
    label: &str,
    detail: &str,
    selected: bool,
    size: egui::Vec2,
    theme: ThemeSettings,
) -> egui::Response {
    let size = egui::vec2(size.x.max(112.0), size.y.max(58.0));
    let colors = theme.semantic();
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let state = InteractionState::from_response(&response, selected);
    let motion = ControlMotion::sample(ui, &response, selected, MotionRole::State);
    let paint_rect = motion.paint_rect(rect);
    let visuals = control_visuals(theme, state, motion);
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 4.0));
    paint_control_shell(&painter, paint_rect, colors, visuals, motion.focus);

    let swatch_size = (paint_rect.height() - 18.0).clamp(36.0, 58.0);
    let swatch = egui::Rect::from_center_size(
        egui::pos2(
            paint_rect.left() + 9.0 + swatch_size * 0.5,
            paint_rect.center().y,
        ),
        egui::Vec2::splat(swatch_size),
    );
    paint_material_swatch(&painter, swatch, color, 4.0);
    painter.rect_stroke(
        swatch,
        egui::Rounding::same(4.0),
        egui::Stroke::new(KANSO_VISUALS.outline_width, colors.outline_strong),
    );

    let text_left = swatch.right() + 10.0;
    let text_width = (paint_rect.right() - text_left - 8.0).max(24.0);
    let text_painter = painter.with_clip_rect(egui::Rect::from_min_max(
        egui::pos2(text_left, paint_rect.top() + 5.0),
        egui::pos2(paint_rect.right() - 7.0, paint_rect.bottom() - 5.0),
    ));
    text_painter.text(
        egui::pos2(text_left, paint_rect.center().y - 9.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(fitted_monospace_size(label, text_width, 12.0, 8.5)),
        visuals.text,
    );
    text_painter.text(
        egui::pos2(text_left, paint_rect.center().y + 10.0),
        egui::Align2::LEFT_CENTER,
        detail,
        egui::FontId::monospace(fitted_monospace_size(detail, text_width, 9.5, 8.0)),
        mix_color(colors.text_muted, visuals.icon, motion.selection),
    );
    response.on_hover_text(format!("{label} - {detail}"))
}

pub fn swatch_slot(
    ui: &mut egui::Ui,
    index: usize,
    color: egui::Color32,
    selected: bool,
    theme: ThemeSettings,
    tooltip: &str,
) -> egui::Response {
    let colors = theme.semantic();
    let size = egui::Vec2::splat(62.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let state = InteractionState::from_response(&response, selected);
    let motion = ControlMotion::sample(ui, &response, selected, MotionRole::Feedback);
    let paint_rect = motion.paint_rect(rect);
    let visuals = control_visuals(theme, state, motion);
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 4.0));
    paint_control_shell(&painter, paint_rect, colors, visuals, motion.focus);
    let swatch = paint_rect.shrink(6.0);
    paint_material_swatch(&painter, swatch, color, 3.0);
    painter.rect_stroke(
        swatch,
        egui::Rounding::same(3.0),
        egui::Stroke::new(KANSO_VISUALS.outline_width, colors.outline_strong),
    );
    let badge = egui::pos2(paint_rect.left() + 11.0, paint_rect.top() + 11.0);
    painter.circle_filled(badge, 8.0, colors.surface_strong);
    painter.circle_stroke(
        badge,
        8.0,
        egui::Stroke::new(KANSO_VISUALS.outline_width, visuals.outline),
    );
    painter.text(
        badge,
        egui::Align2::CENTER_CENTER,
        (index + 1).to_string(),
        egui::FontId::monospace(9.0),
        visuals.text,
    );
    response.on_hover_text(tooltip)
}

pub fn major_action(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    detail: &str,
    selected: bool,
    theme: ThemeSettings,
) -> egui::Response {
    card_response(
        ui,
        icon,
        label,
        detail,
        selected,
        theme,
        egui::vec2(218.0, 64.0),
    )
}

pub fn setting_card(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    detail: &str,
    active: bool,
    theme: ThemeSettings,
) -> egui::Response {
    card_response(
        ui,
        icon,
        label,
        detail,
        active,
        theme,
        egui::vec2(220.0, 66.0),
    )
}

pub fn mode_card(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    detail: &str,
    active: bool,
    theme: ThemeSettings,
) -> egui::Response {
    card_response(
        ui,
        icon,
        label,
        detail,
        active,
        theme,
        egui::vec2(202.0, 72.0),
    )
}

pub fn status_chip(ui: &mut egui::Ui, icon: Icon, label: &str, value: &str, theme: ThemeSettings) {
    let colors = theme.semantic();
    egui::Frame::none()
        .fill(colors.surface)
        .stroke(egui::Stroke::new(
            KANSO_VISUALS.outline_width,
            colors.outline,
        ))
        .rounding(egui::Rounding::same(KANSO_VISUALS.corner_radius))
        .inner_margin(egui::Margin::symmetric(8.0, 5.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                paint_icon(ui.painter(), rect, icon, colors.accent);
                ui.label(egui::RichText::new(label).small().color(colors.text_muted));
                ui.label(
                    egui::RichText::new(value)
                        .small()
                        .strong()
                        .color(colors.text),
                );
            });
        });
}

/// Rendering state for the fixed-size loading primitive. Progress and terminal
/// states are static; only `Indeterminate` may schedule bounded repaints.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoadingState {
    Idle,
    Indeterminate,
    Progress(f32),
    Complete,
}

impl LoadingState {
    fn progress(self) -> Option<f32> {
        match self {
            Self::Progress(value) => Some(if value.is_finite() {
                value.clamp(0.0, 1.0)
            } else {
                0.0
            }),
            Self::Complete => Some(1.0),
            Self::Idle | Self::Indeterminate => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ActivitySample {
    phase: f32,
    pulse: f32,
    needs_repaint: bool,
}

fn activity_sample(time: f64, active: bool, continuous_motion: bool) -> ActivitySample {
    if !active {
        return ActivitySample {
            phase: 0.0,
            pulse: 0.0,
            needs_repaint: false,
        };
    }
    if !continuous_motion || !time.is_finite() {
        return ActivitySample {
            phase: STATIC_ACTIVITY_PHASE,
            pulse: STATIC_ACTIVITY_PULSE,
            needs_repaint: false,
        };
    }

    let phase = (time as f32 * 0.34).rem_euclid(1.0);
    ActivitySample {
        phase,
        pulse: 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos(),
        needs_repaint: true,
    }
}

fn request_activity_repaint(ui: &egui::Ui, rect: egui::Rect, sample: ActivitySample) {
    if sample.needs_repaint && ui.is_rect_visible(rect) {
        ui.ctx().request_repaint_after(ACTIVITY_REPAINT_INTERVAL);
    }
}

fn point_on_turn(center: egui::Pos2, radius: f32, turn: f32) -> egui::Pos2 {
    let angle = turn * std::f32::consts::TAU;
    center + egui::vec2(angle.cos(), angle.sin()) * radius
}

fn paint_arc(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    start_turn: f32,
    sweep_turn: f32,
    stroke: egui::Stroke,
) -> Option<egui::Pos2> {
    let sweep_turn = if sweep_turn.is_finite() {
        sweep_turn.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if sweep_turn <= f32::EPSILON || !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    let start_turn = if start_turn.is_finite() {
        start_turn.rem_euclid(1.0)
    } else {
        0.0
    };
    let arc_length = radius * std::f32::consts::TAU * sweep_turn;
    let segments = (arc_length / 2.0).ceil().clamp(3.0, 32.0) as usize;
    let mut points = Vec::with_capacity(segments + 1);
    for segment in 0..=segments {
        let amount = segment as f32 / segments as f32;
        points.push(point_on_turn(
            center,
            radius,
            start_turn + sweep_turn * amount,
        ));
    }
    let end = points.last().copied();
    painter.add(egui::Shape::line(points, stroke));
    end
}

/// Paint-only loading primitive. It never requests a repaint; callers that
/// provide a changing phase retain full scheduling control.
pub fn paint_loading_indicator(
    painter: &egui::Painter,
    rect: egui::Rect,
    state: LoadingState,
    phase: f32,
    theme: ThemeSettings,
) {
    let colors = theme.semantic();
    let center = rect.center();
    let diameter = rect.width().min(rect.height());
    if !diameter.is_finite() || diameter <= 6.0 {
        return;
    }
    let radius = diameter * 0.5 - 3.0;
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(KANSO_VISUALS.outline_width, colors.outline_disabled),
    );

    match state {
        LoadingState::Idle => {
            painter.circle_filled(center, 1.75, colors.text_muted);
        }
        LoadingState::Indeterminate => {
            let endpoint = paint_arc(
                painter,
                center,
                radius,
                phase,
                0.30,
                egui::Stroke::new(KANSO_VISUALS.focus_width, colors.accent),
            );
            if let Some(endpoint) = endpoint {
                painter.circle_filled(endpoint, 1.65, colors.focus);
            }
            painter.circle_filled(center, 1.5, with_alpha(colors.info, 0.86));
        }
        LoadingState::Progress(_) => {
            let progress = state.progress().unwrap_or(0.0);
            let endpoint = paint_arc(
                painter,
                center,
                radius,
                -0.25,
                progress,
                egui::Stroke::new(KANSO_VISUALS.focus_width, colors.outline_active),
            );
            if progress > f32::EPSILON && progress < 1.0 - f32::EPSILON {
                if let Some(endpoint) = endpoint {
                    painter.circle_filled(endpoint, 1.5, colors.focus);
                }
            }
            painter.circle_filled(center, 1.75, colors.text_muted);
        }
        LoadingState::Complete => {
            painter.circle_stroke(
                center,
                radius,
                egui::Stroke::new(KANSO_VISUALS.focus_width, colors.success),
            );
            painter.line_segment(
                [
                    center + egui::vec2(-4.0, 0.0),
                    center + egui::vec2(-1.0, 3.0),
                ],
                egui::Stroke::new(1.5, colors.success),
            );
            painter.line_segment(
                [
                    center + egui::vec2(-1.0, 3.0),
                    center + egui::vec2(5.0, -4.0),
                ],
                egui::Stroke::new(1.5, colors.success),
            );
        }
    }
}

/// Fixed-size loading widget. Static states never enqueue a repaint. The
/// indeterminate state runs at a bounded ~30 Hz only under full motion.
pub fn loading_indicator(
    ui: &mut egui::Ui,
    state: LoadingState,
    theme: ThemeSettings,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::Vec2::splat(KANSO_LAYOUT.loading_indicator_size),
        egui::Sense::hover(),
    );
    let sample = activity_sample(
        ui.input(|input| input.time),
        matches!(state, LoadingState::Indeterminate),
        allows_continuous_motion(ui.ctx()),
    );
    request_activity_repaint(ui, rect, sample);
    paint_loading_indicator(ui.painter(), rect, state, sample.phase, theme);
    response
}

/// Stable status row that combines a low-frequency activity indicator with
/// readable state copy. Only `Indeterminate` schedules periodic repaints.
pub fn activity_status(
    ui: &mut egui::Ui,
    state: LoadingState,
    label: &str,
    value: &str,
    theme: ThemeSettings,
) -> egui::Response {
    let colors = theme.semantic();
    let size = egui::vec2(ui.available_width().max(120.0), 40.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let sample = activity_sample(
        ui.input(|input| input.time),
        matches!(state, LoadingState::Indeterminate),
        allows_continuous_motion(ui.ctx()),
    );
    request_activity_repaint(ui, rect, sample);

    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        egui::Rounding::same(KANSO_VISUALS.corner_radius),
        colors.surface,
    );
    painter.rect_stroke(
        rect,
        egui::Rounding::same(KANSO_VISUALS.corner_radius),
        egui::Stroke::new(KANSO_VISUALS.outline_width, colors.outline),
    );
    let indicator = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        egui::Vec2::splat(KANSO_LAYOUT.loading_indicator_size),
    );
    paint_loading_indicator(&painter, indicator, state, sample.phase, theme);

    let status_color = match state {
        LoadingState::Idle => colors.text_muted,
        LoadingState::Indeterminate | LoadingState::Progress(_) => colors.accent,
        LoadingState::Complete => colors.success,
    };
    let text_left = rect.left() + 40.0;
    let text_width = (rect.right() - text_left - 10.0).max(24.0);
    let text_painter = painter.with_clip_rect(egui::Rect::from_min_max(
        egui::pos2(text_left, rect.top() + 4.0),
        egui::pos2(rect.right() - 8.0, rect.bottom() - 4.0),
    ));
    text_painter.text(
        egui::pos2(text_left, rect.center().y - 7.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(fitted_monospace_size(label, text_width, 10.5, 8.5)),
        status_color,
    );
    text_painter.text(
        egui::pos2(text_left, rect.center().y + 8.0),
        egui::Align2::LEFT_CENTER,
        value,
        egui::FontId::monospace(fitted_monospace_size(value, text_width, 10.0, 8.0)),
        colors.text,
    );

    if let Some(progress) = state.progress() {
        let track = egui::Rect::from_min_max(
            egui::pos2(text_left, rect.bottom() - 3.0),
            egui::pos2(rect.right() - 8.0, rect.bottom() - 2.0),
        );
        painter.rect_filled(track, 0.0, colors.outline_disabled);
        if progress > 0.0 {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    track.min,
                    egui::pos2(track.left() + track.width() * progress, track.bottom()),
                ),
                0.0,
                status_color,
            );
        }
    }

    response.on_hover_text(format!("{label}: {value}"))
}

/// Paint-only orbital pulse. Supplying a fixed phase and intensity is fully
/// static and does not interact with egui's repaint scheduler.
#[derive(Debug, Clone, Copy, PartialEq)]
struct OrbitGeometry {
    center: egui::Pos2,
    orbit_radius: f32,
    pulse_radius: f32,
    satellite: egui::Pos2,
    satellite_radius: f32,
    phase: f32,
    intensity: f32,
}

fn orbit_geometry(rect: egui::Rect, phase: f32, intensity: f32) -> Option<OrbitGeometry> {
    let diameter = rect.width().min(rect.height());
    if !diameter.is_finite() || diameter <= 6.0 {
        return None;
    }
    let phase = if phase.is_finite() {
        phase.rem_euclid(1.0)
    } else {
        0.0
    };
    let intensity = if intensity.is_finite() {
        intensity.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let satellite_radius = (diameter * 0.065).clamp(1.5, 2.25);
    let orbit_radius = diameter * 0.5 - satellite_radius - 1.0;
    let center = rect.center();
    Some(OrbitGeometry {
        center,
        orbit_radius,
        pulse_radius: orbit_radius * (0.62 + intensity * 0.18),
        satellite: point_on_turn(center, orbit_radius, phase),
        satellite_radius,
        phase,
        intensity,
    })
}

pub fn paint_orbit_pulse(
    painter: &egui::Painter,
    rect: egui::Rect,
    phase: f32,
    intensity: f32,
    theme: ThemeSettings,
) {
    let colors = theme.semantic();
    let Some(geometry) = orbit_geometry(rect, phase, intensity) else {
        return;
    };
    painter.circle_stroke(
        geometry.center,
        geometry.orbit_radius,
        egui::Stroke::new(KANSO_VISUALS.outline_width, colors.outline_disabled),
    );
    let arc_sweep = 0.14 + geometry.intensity * 0.12;
    let _ = paint_arc(
        painter,
        geometry.center,
        geometry.orbit_radius,
        geometry.phase - arc_sweep * 0.5,
        arc_sweep,
        egui::Stroke::new(
            KANSO_VISUALS.outline_width + geometry.intensity * 0.35,
            with_alpha(colors.accent, 0.34 + geometry.intensity * 0.42),
        ),
    );
    painter.circle_stroke(
        geometry.center,
        geometry.pulse_radius,
        egui::Stroke::new(
            KANSO_VISUALS.focus_width,
            with_alpha(colors.accent, 0.12 + geometry.intensity * 0.34),
        ),
    );
    painter.circle_filled(
        geometry.center,
        3.0,
        with_alpha(colors.info, 0.10 + geometry.intensity * 0.14),
    );
    painter.circle_stroke(
        geometry.center,
        3.0,
        egui::Stroke::new(KANSO_VISUALS.outline_width, colors.outline_hover),
    );
    painter.circle_filled(geometry.center, 1.4, colors.info);
    painter.circle_filled(
        geometry.satellite,
        geometry.satellite_radius + 1.0,
        with_alpha(colors.focus_glow, 0.08 + geometry.intensity * 0.18),
    );
    painter.circle_filled(
        geometry.satellite,
        geometry.satellite_radius,
        mix_color(colors.outline_hover, colors.focus, geometry.intensity),
    );
}

/// Fixed-size orbit widget. Inactive, reduced-motion and low-spec variants
/// are frozen; only an active full-profile pulse schedules bounded repaints.
pub fn orbit_pulse(ui: &mut egui::Ui, active: bool, theme: ThemeSettings) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::Vec2::splat(KANSO_LAYOUT.orbit_pulse_size),
        egui::Sense::hover(),
    );
    let sample = activity_sample(
        ui.input(|input| input.time),
        active,
        allows_continuous_motion(ui.ctx()),
    );
    request_activity_repaint(ui, rect, sample);
    paint_orbit_pulse(ui.painter(), rect, sample.phase, sample.pulse, theme);
    response
}

pub fn tab_chip(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    selected: bool,
    theme: ThemeSettings,
) -> egui::Response {
    let width = (label.chars().count() as f32 * 8.5 + 44.0)
        .clamp(KANSO_LAYOUT.tab_min_width, KANSO_LAYOUT.tab_max_width);
    tab_chip_sized(ui, icon, label, selected, width, theme)
}

/// Width-stable tab variant for switchable labels and aligned tab strips.
pub fn tab_chip_sized(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    selected: bool,
    width: f32,
    theme: ThemeSettings,
) -> egui::Response {
    let colors = theme.semantic();
    let width = if width.is_finite() {
        width.max(KANSO_LAYOUT.tab_min_width)
    } else {
        KANSO_LAYOUT.tab_min_width
    };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, KANSO_LAYOUT.tab_height),
        egui::Sense::click(),
    );
    let state = InteractionState::from_response(&response, selected);
    let motion = ControlMotion::sample(ui, &response, selected, MotionRole::Feedback);
    let paint_rect = motion.paint_rect(rect);
    let visuals = control_visuals(theme, state, motion);
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 4.0));
    paint_control_shell(&painter, paint_rect, colors, visuals, motion.focus);
    paint_icon(
        &painter,
        egui::Rect::from_center_size(
            egui::pos2(paint_rect.left() + 18.0, paint_rect.center().y),
            egui::vec2(18.0, 18.0),
        ),
        icon,
        visuals.icon,
    );
    painter.text(
        egui::pos2(paint_rect.left() + 35.0, paint_rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(11.5),
        visuals.text,
    );
    response.on_hover_text(label)
}

pub fn icon_square(
    ui: &mut egui::Ui,
    icon: Icon,
    selected: bool,
    theme: ThemeSettings,
    tooltip: &str,
) -> egui::Response {
    let colors = theme.semantic();
    let (rect, response) = ui.allocate_exact_size(
        egui::Vec2::splat(KANSO_LAYOUT.icon_square_size),
        egui::Sense::click(),
    );
    let state = InteractionState::from_response(&response, selected);
    let motion = ControlMotion::sample(ui, &response, selected, MotionRole::Feedback);
    let paint_rect = motion.paint_rect(rect);
    let visuals = control_visuals(theme, state, motion);
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 3.0));
    paint_control_shell(&painter, paint_rect, colors, visuals, motion.focus);
    paint_icon(&painter, paint_rect.shrink(8.0), icon, visuals.icon);
    response.on_hover_text(tooltip)
}

pub fn search_box(
    ui: &mut egui::Ui,
    query: &mut String,
    hint: &str,
    theme: ThemeSettings,
) -> egui::Response {
    let colors = theme.semantic();
    let response = ui.add(
        egui::TextEdit::singleline(query)
            .hint_text(hint)
            .desired_width(f32::INFINITY),
    );
    let state = InteractionState::from_response(&response, false);
    let motion = ControlMotion::sample(ui, &response, false, MotionRole::Feedback);
    let visuals = control_visuals(theme, state, motion);
    let rect = response.rect.expand(1.0);
    paint_control_outline(ui.painter(), rect, colors, visuals, motion.focus);
    response
}

pub fn danger_action(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    theme: ThemeSettings,
) -> egui::Response {
    let width = (label.chars().count() as f32 * 8.0 + 42.0).clamp(
        KANSO_LAYOUT.icon_action_min_width,
        KANSO_LAYOUT.icon_action_max_width,
    );
    icon_action_sized_tone(ui, icon, label, false, width, theme, ActionTone::Danger)
}

pub fn compact_separator(ui: &mut egui::Ui, theme: ThemeSettings) {
    let colors = theme.semantic();
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, colors.outline);
}

pub fn advanced_section(
    ui: &mut egui::Ui,
    theme: ThemeSettings,
    title: &str,
    open: &mut bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let label = if *open {
        format!("Hide {title}")
    } else {
        format!("Advanced {title}")
    };
    if icon_action(ui, Icon::Drawer, &label, *open, theme).clicked() {
        *open = !*open;
    }
    let panel_amount = animate_bool_finite(
        ui.ctx(),
        ui.make_persistent_id(("r93g_advanced_panel", title)),
        *open,
        MotionRole::Panel,
    );
    if *open || panel_amount > 0.001 {
        ui.add_space(6.0);
        show_surface_panel(ui, theme, panel_amount, add_contents);
    }
}

fn card_response(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    detail: &str,
    selected: bool,
    theme: ThemeSettings,
    size: egui::Vec2,
) -> egui::Response {
    let colors = theme.semantic();
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let state = InteractionState::from_response(&response, selected);
    let motion = ControlMotion::sample(ui, &response, selected, MotionRole::State);
    let paint_rect = motion.paint_rect(rect);
    let visuals = control_visuals(theme, state, motion);
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 5.0));
    paint_control_shell(&painter, paint_rect, colors, visuals, motion.focus);
    let signal_amount = motion
        .hover
        .max(motion.focus)
        .max(motion.press)
        .max(motion.selection);
    if state.enabled && signal_amount > 0.001 {
        painter.line_segment(
            [
                egui::pos2(paint_rect.left() + 14.0, paint_rect.top() + 2.0),
                egui::pos2(paint_rect.right() - 14.0, paint_rect.top() + 2.0),
            ],
            egui::Stroke::new(
                1.0,
                with_alpha(
                    mix_color(colors.outline_hover, colors.focus, motion.focus),
                    signal_amount,
                ),
            ),
        );
    }
    let icon_rect = egui::Rect::from_min_size(
        paint_rect.min + egui::vec2(12.0, 14.0),
        egui::vec2(26.0, 26.0),
    );
    paint_icon(&painter, icon_rect, icon, visuals.icon);
    painter.text(
        egui::pos2(paint_rect.left() + 48.0, paint_rect.top() + 18.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(13.0),
        visuals.text,
    );
    painter.text(
        egui::pos2(paint_rect.left() + 48.0, paint_rect.top() + 42.0),
        egui::Align2::LEFT_CENTER,
        detail,
        egui::FontId::monospace(10.0),
        if state.enabled {
            colors.text_muted
        } else {
            colors.text_disabled
        },
    );
    response.on_hover_text(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_conversion_clamps_and_rounds() {
        assert_eq!(alpha_u8(-1.0), 0);
        assert_eq!(alpha_u8(f32::NAN), 0);
        assert_eq!(alpha_u8(0.0), 0);
        assert_eq!(alpha_u8(0.5), 128);
        assert_eq!(alpha_u8(1.0), 255);
        assert_eq!(alpha_u8(2.0), 255);
    }

    #[test]
    fn interaction_tokens_have_stable_precedence() {
        let theme = ThemeSettings::default();
        let colors = theme.semantic();
        let target_visuals = |state| control_visuals(theme, state, ControlMotion::target(state));
        let resting = target_visuals(InteractionState {
            enabled: true,
            selected: false,
            hovered: false,
            focused: false,
            pressed: false,
        });
        assert_eq!(resting.fill, colors.surface);
        assert_eq!(resting.outline, colors.outline);
        assert_eq!(resting.outline_width, KANSO_VISUALS.outline_width);

        let hovered = target_visuals(InteractionState {
            enabled: true,
            selected: false,
            hovered: true,
            focused: false,
            pressed: false,
        });
        assert_eq!(hovered.fill, colors.surface_hover);
        assert_eq!(hovered.outline, colors.outline_hover);

        let pressed = target_visuals(InteractionState {
            enabled: true,
            selected: false,
            hovered: true,
            focused: false,
            pressed: true,
        });
        assert_eq!(pressed.fill, colors.surface_active);
        assert_eq!(pressed.outline, colors.outline_active);

        let selected = target_visuals(InteractionState {
            enabled: true,
            selected: true,
            hovered: false,
            focused: false,
            pressed: false,
        });
        assert_eq!(selected.fill, colors.selected);
        assert_eq!(selected.text, theme.text_on(colors.selected));

        let focused_selected = target_visuals(InteractionState {
            enabled: true,
            selected: true,
            hovered: true,
            focused: true,
            pressed: false,
        });
        assert_eq!(focused_selected.fill, colors.selected);
        assert_eq!(focused_selected.outline, colors.outline_active);
        assert_eq!(focused_selected.icon, colors.focus);
        assert_eq!(focused_selected.outline_width, KANSO_VISUALS.focus_width);

        let disabled = target_visuals(InteractionState {
            enabled: false,
            selected: true,
            hovered: true,
            focused: true,
            pressed: true,
        });
        assert_eq!(disabled.fill, colors.surface_disabled);
        assert_eq!(disabled.text, colors.text_disabled);
        assert_eq!(disabled.icon, colors.text_disabled);
        assert_eq!(disabled.outline, colors.outline_disabled);
        assert_eq!(disabled.outline_width, KANSO_VISUALS.outline_width);
    }

    #[test]
    fn control_motion_is_paint_only_and_keeps_allocated_size() {
        let rect = egui::Rect::from_min_size(egui::pos2(20.0, 30.0), egui::vec2(140.0, 36.0));
        let moved = ControlMotion {
            hover: 1.0,
            focus: 0.0,
            press: 0.5,
            selection: 0.0,
            spatial_motion: true,
        }
        .paint_rect(rect);
        let stationary = ControlMotion {
            hover: 1.0,
            focus: 1.0,
            press: 1.0,
            selection: 1.0,
            spatial_motion: false,
        }
        .paint_rect(rect);

        assert_eq!(moved.size(), rect.size());
        assert_eq!(moved.left(), rect.left());
        assert!(moved.top() < rect.top());
        assert_eq!(stationary, rect);
        assert_eq!(KANSO_LAYOUT.tab_height, 34.0);
        assert_eq!(KANSO_LAYOUT.icon_square_size, 36.0);
    }

    #[test]
    fn interaction_colors_interpolate_without_hard_state_jumps() {
        let theme = ThemeSettings::default();
        let colors = theme.semantic();
        let state = InteractionState {
            enabled: true,
            selected: false,
            hovered: true,
            focused: false,
            pressed: false,
        };
        let halfway = control_visuals(
            theme,
            state,
            ControlMotion {
                hover: 0.5,
                focus: 0.0,
                press: 0.0,
                selection: 0.0,
                spatial_motion: true,
            },
        );

        assert_ne!(halfway.fill, colors.surface);
        assert_ne!(halfway.fill, colors.surface_hover);
        assert_ne!(halfway.outline, colors.outline);
        assert_ne!(halfway.outline, colors.outline_hover);
        assert!(halfway.outline_width > KANSO_VISUALS.outline_width);
        assert!(halfway.outline_width < KANSO_VISUALS.focus_width);
    }

    #[test]
    fn fitted_labels_shrink_only_when_the_container_requires_it() {
        assert_eq!(fitted_monospace_size("SHORT", 120.0, 12.0, 8.0), 12.0);
        let fitted = fitted_monospace_size("A VERY LONG COMMAND LABEL", 90.0, 12.0, 8.0);
        assert!((8.0..12.0).contains(&fitted));
        assert_eq!(fitted_monospace_size("", 0.0, 12.0, 8.0), 12.0);
    }

    #[test]
    fn activity_repaints_only_for_active_full_motion() {
        let idle = activity_sample(12.0, false, true);
        let frozen = activity_sample(12.0, true, false);
        let frozen_later = activity_sample(9_999.0, true, false);
        let active = activity_sample(12.0, true, true);
        let invalid_time = activity_sample(f64::NAN, true, true);

        assert!(!idle.needs_repaint);
        assert!(!frozen.needs_repaint);
        assert_eq!(frozen, frozen_later);
        assert_eq!(frozen.phase, STATIC_ACTIVITY_PHASE);
        assert_eq!(frozen.pulse, STATIC_ACTIVITY_PULSE);
        assert!(active.needs_repaint);
        assert!((0.0..1.0).contains(&active.phase));
        assert!((0.0..=1.0).contains(&active.pulse));
        assert_eq!(invalid_time, frozen);
        assert!(ACTIVITY_REPAINT_INTERVAL >= Duration::from_millis(30));
        assert!(ACTIVITY_REPAINT_INTERVAL <= Duration::from_millis(50));
    }

    #[test]
    fn loading_progress_is_finite_and_clamped() {
        assert_eq!(LoadingState::Progress(-1.0).progress(), Some(0.0));
        assert_eq!(LoadingState::Progress(f32::NAN).progress(), Some(0.0));
        assert_eq!(LoadingState::Progress(0.4).progress(), Some(0.4));
        assert_eq!(LoadingState::Progress(2.0).progress(), Some(1.0));
        assert_eq!(LoadingState::Complete.progress(), Some(1.0));
        assert_eq!(LoadingState::Indeterminate.progress(), None);
    }

    #[test]
    fn orbit_geometry_is_clamped_static_and_inside_its_allocation() {
        let rect = egui::Rect::from_center_size(
            egui::pos2(40.0, 50.0),
            egui::Vec2::splat(KANSO_LAYOUT.orbit_pulse_size),
        );
        let frozen = orbit_geometry(rect, STATIC_ACTIVITY_PHASE, STATIC_ACTIVITY_PULSE)
            .expect("fixed-size orbit geometry");
        let clamped = orbit_geometry(rect, f32::NAN, 2.0).expect("clamped orbit geometry");

        assert!(rect.contains(frozen.center));
        assert!(rect.contains(frozen.satellite));
        assert!(frozen.pulse_radius < frozen.orbit_radius);
        assert!((0.0..1.0).contains(&frozen.phase));
        assert!((0.0..=1.0).contains(&frozen.intensity));
        assert_eq!(clamped.phase, 0.0);
        assert_eq!(clamped.intensity, 1.0);
        let painted_satellite_radius = frozen.satellite_radius + 1.0;
        assert!(frozen.satellite.x - painted_satellite_radius >= rect.left());
        assert!(frozen.satellite.x + painted_satellite_radius <= rect.right());
        assert!(frozen.satellite.y - painted_satellite_radius >= rect.top());
        assert!(frozen.satellite.y + painted_satellite_radius <= rect.bottom());
        assert!(orbit_geometry(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(4.0)),
            0.0,
            0.0,
        )
        .is_none());
    }

    #[test]
    fn activity_widgets_and_animated_panel_keep_fixed_allocations() {
        egui::__run_test_ui(|ui| {
            let theme = ThemeSettings::default();
            let loading = loading_indicator(ui, LoadingState::Idle, theme);
            assert_eq!(
                loading.rect.size(),
                egui::Vec2::splat(KANSO_LAYOUT.loading_indicator_size)
            );

            let orbit = orbit_pulse(ui, false, theme);
            assert_eq!(
                orbit.rect.size(),
                egui::Vec2::splat(KANSO_LAYOUT.orbit_pulse_size)
            );

            let status = activity_status(ui, LoadingState::Idle, "WORLD LINK", "Ready", theme);
            assert_eq!(status.rect.height(), 40.0);
            assert!(status.rect.width() >= 120.0);

            let command = command_action(
                ui,
                "CONTINUE",
                Some("garden"),
                ActionTone::Primary,
                54.0,
                theme,
            );
            assert_eq!(command.rect.height(), 54.0);

            let panel = surface_panel_animated(
                ui,
                theme,
                egui::Id::new("allocation_test_panel"),
                true,
                |ui| ui.allocate_space(egui::vec2(80.0, 20.0)),
            );
            assert!(panel.response.rect.width() >= 80.0);
            assert!(panel.response.rect.height() >= 20.0);
        });
    }
}
