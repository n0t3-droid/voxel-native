//! Shared Neon Toolbench widgets for egui surfaces.
//!
//! The helpers here are intentionally small: they keep colour, spacing,
//! radius and button states consistent without introducing a new UI
//! framework or asset dependency.

use bevy_egui::egui;

use crate::icons::{paint_icon, Icon};
use crate::theme::{animate_bool_finite, MotionRole, SemanticColors, ThemeSettings, KANSO_VISUALS};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InteractionState {
    selected: bool,
    hovered: bool,
    focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ControlVisuals {
    fill: egui::Color32,
    text: egui::Color32,
    icon: egui::Color32,
    outline: egui::Color32,
    outline_width: f32,
}

fn control_visuals(theme: ThemeSettings, state: InteractionState) -> ControlVisuals {
    let colors = theme.semantic();
    let fill = if state.selected {
        colors.selected
    } else if state.hovered || state.focused {
        colors.surface_strong
    } else {
        colors.surface
    };
    let text = if state.selected {
        theme.text_on(fill)
    } else {
        colors.text
    };
    let (outline, outline_width) = if state.focused {
        (colors.focus, KANSO_VISUALS.focus_width)
    } else if state.selected || state.hovered {
        (colors.accent, KANSO_VISUALS.focus_width)
    } else {
        (colors.outline, KANSO_VISUALS.outline_width)
    };

    ControlVisuals {
        fill,
        text,
        icon: if state.selected || state.focused {
            colors.focus
        } else {
            colors.accent
        },
        outline,
        outline_width,
    }
}

fn paint_control_outline(
    painter: &egui::Painter,
    rect: egui::Rect,
    colors: SemanticColors,
    visuals: ControlVisuals,
    focus_amount: f32,
    focused: bool,
) {
    if focus_amount > 0.001 {
        let glow_rect = rect.expand(KANSO_VISUALS.focus_gap + focus_amount * 1.5);
        painter.rect_stroke(
            glow_rect,
            egui::Rounding::same(KANSO_VISUALS.corner_radius + 2.0),
            egui::Stroke::new(
                KANSO_VISUALS.focus_width,
                if focused {
                    colors.focus_glow
                } else {
                    colors.focus_glow.linear_multiply(focus_amount * 0.72)
                },
            ),
        );
    }
    if focused {
        painter.rect_stroke(
            rect.expand(KANSO_VISUALS.focus_gap),
            egui::Rounding::same(KANSO_VISUALS.corner_radius + 1.0),
            egui::Stroke::new(KANSO_VISUALS.focus_width, colors.focus),
        );
    }
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
    focused: bool,
) {
    painter.rect_filled(
        rect,
        egui::Rounding::same(KANSO_VISUALS.corner_radius),
        visuals.fill,
    );
    paint_control_outline(painter, rect, colors, visuals, focus_amount, focused);
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
            offset: egui::vec2(0.0, 8.0),
            blur: 20.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(168),
        })
}

pub fn surface_panel<R>(
    ui: &mut egui::Ui,
    theme: ThemeSettings,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let colors = theme.semantic();
    egui::Frame::none()
        .fill(with_alpha(colors.surface, 0.94))
        .stroke(egui::Stroke::new(
            KANSO_VISUALS.outline_width,
            with_alpha(colors.outline, 0.92),
        ))
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
        .rounding(egui::Rounding::same(KANSO_VISUALS.corner_radius))
        .show(ui, add_contents)
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
    let inner_outline = with_alpha(colors.outline_strong, opacity * 0.78);

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
    painter.rect_stroke(
        rect.shrink(1.5),
        egui::Rounding::same(KANSO_VISUALS.corner_radius - 1.0),
        egui::Stroke::new(KANSO_VISUALS.outline_width, inner_outline),
    );

    painter.line_segment(
        [
            egui::pos2(rect.left() + 12.0, rect.top() + 2.0),
            egui::pos2(rect.right() - 12.0, rect.top() + 2.0),
        ],
        egui::Stroke::new(1.0, signal),
    );
    painter.line_segment(
        [
            egui::pos2(rect.left() + 14.0, rect.bottom() - 2.0),
            egui::pos2(rect.right() - 14.0, rect.bottom() - 2.0),
        ],
        egui::Stroke::new(
            1.0,
            egui::Color32::from_black_alpha(alpha_u8(opacity * 0.32)),
        ),
    );

    let tick = rect.width().min(rect.height()).min(24.0) * 0.42;
    let stroke = egui::Stroke::new(1.25, signal);
    let l = rect.left() + 5.0;
    let r = rect.right() - 5.0;
    let t = rect.top() + 5.0;
    let b = rect.bottom() - 5.0;
    painter.line_segment([egui::pos2(l, t), egui::pos2(l + tick, t)], stroke);
    painter.line_segment([egui::pos2(l, t), egui::pos2(l, t + tick)], stroke);
    painter.line_segment([egui::pos2(r, t), egui::pos2(r - tick, t)], stroke);
    painter.line_segment([egui::pos2(r, t), egui::pos2(r, t + tick)], stroke);
    painter.line_segment([egui::pos2(l, b), egui::pos2(l + tick, b)], stroke);
    painter.line_segment([egui::pos2(l, b), egui::pos2(l, b - tick)], stroke);
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
    let colors = theme.semantic();
    let height = theme.density.row_height();
    let width = (label.chars().count() as f32 * 8.0 + 42.0).clamp(82.0, 170.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let hovered = response.hovered();
    let focused = response.has_focus();
    let focus_amount = animate_bool_finite(
        ui.ctx(),
        response.id.with("kanso_action_focus"),
        hovered || focused,
        MotionRole::Feedback,
    );
    let paint_rect = rect.translate(egui::vec2(0.0, -focus_amount * KANSO_VISUALS.hover_lift));
    let state = InteractionState {
        selected,
        hovered,
        focused,
    };
    let visuals = control_visuals(theme, state);
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 4.0));
    paint_control_shell(&painter, paint_rect, colors, visuals, focus_amount, focused);
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
        egui::vec2(218.0, 70.0),
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

pub fn tab_chip(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    selected: bool,
    theme: ThemeSettings,
) -> egui::Response {
    let colors = theme.semantic();
    let width = (label.chars().count() as f32 * 8.5 + 44.0).clamp(96.0, 150.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::click());
    let hovered = response.hovered();
    let focused = response.has_focus();
    let focus_amount = animate_bool_finite(
        ui.ctx(),
        response.id.with("kanso_tab_focus"),
        hovered || focused,
        MotionRole::Feedback,
    );
    let paint_rect = rect.translate(egui::vec2(0.0, -focus_amount * KANSO_VISUALS.hover_lift));
    let visuals = control_visuals(
        theme,
        InteractionState {
            selected,
            hovered,
            focused,
        },
    );
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 4.0));
    paint_control_shell(&painter, paint_rect, colors, visuals, focus_amount, focused);
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
    let (rect, response) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::click());
    let hovered = response.hovered();
    let focused = response.has_focus();
    let focus_amount = animate_bool_finite(
        ui.ctx(),
        response.id.with("kanso_square_focus"),
        hovered || focused,
        MotionRole::Feedback,
    );
    let visuals = control_visuals(
        theme,
        InteractionState {
            selected,
            hovered,
            focused,
        },
    );
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 3.0));
    paint_control_shell(&painter, rect, colors, visuals, focus_amount, focused);
    paint_icon(&painter, rect.shrink(8.0), icon, visuals.icon);
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
    let hovered = response.hovered();
    let focused = response.has_focus();
    let focus_amount = animate_bool_finite(
        ui.ctx(),
        response.id.with("kanso_search_focus"),
        hovered || focused,
        MotionRole::Feedback,
    );
    let visuals = control_visuals(
        theme,
        InteractionState {
            selected: false,
            hovered,
            focused,
        },
    );
    let rect = response.rect.expand(1.0);
    paint_control_outline(ui.painter(), rect, colors, visuals, focus_amount, focused);
    response
}

pub fn danger_action(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    theme: ThemeSettings,
) -> egui::Response {
    let colors = theme.semantic();
    let resp = icon_action(ui, icon, label, false, theme);
    ui.painter().rect_stroke(
        resp.rect.shrink(1.5),
        egui::Rounding::same(KANSO_VISUALS.corner_radius - 1.0),
        egui::Stroke::new(1.2, colors.danger),
    );
    resp
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
    if *open {
        ui.add_space(6.0);
        surface_panel(ui, theme, add_contents);
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
    let hovered = response.hovered();
    let focused = response.has_focus();
    let focus_amount = animate_bool_finite(
        ui.ctx(),
        response.id.with("kanso_card_focus"),
        hovered || focused,
        MotionRole::State,
    );
    let paint_rect = rect.translate(egui::vec2(0.0, -focus_amount * KANSO_VISUALS.hover_lift));
    let visuals = control_visuals(
        theme,
        InteractionState {
            selected,
            hovered,
            focused,
        },
    );
    let painter = ui.painter_at(rect.expand(KANSO_VISUALS.focus_gap + 5.0));
    paint_control_shell(&painter, paint_rect, colors, visuals, focus_amount, focused);
    if selected || hovered || focused {
        painter.line_segment(
            [
                egui::pos2(paint_rect.left() + 14.0, paint_rect.top() + 2.0),
                egui::pos2(paint_rect.right() - 14.0, paint_rect.top() + 2.0),
            ],
            egui::Stroke::new(1.0, if focused { colors.focus } else { colors.accent }),
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
        colors.text_muted,
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
        let resting = control_visuals(
            theme,
            InteractionState {
                selected: false,
                hovered: false,
                focused: false,
            },
        );
        assert_eq!(resting.fill, colors.surface);
        assert_eq!(resting.outline, colors.outline);
        assert_eq!(resting.outline_width, KANSO_VISUALS.outline_width);

        let hovered = control_visuals(
            theme,
            InteractionState {
                selected: false,
                hovered: true,
                focused: false,
            },
        );
        assert_eq!(hovered.fill, colors.surface_strong);
        assert_eq!(hovered.outline, colors.accent);

        let selected = control_visuals(
            theme,
            InteractionState {
                selected: true,
                hovered: false,
                focused: false,
            },
        );
        assert_eq!(selected.fill, colors.selected);
        assert_eq!(selected.text, theme.text_on(colors.selected));

        let focused_selected = control_visuals(
            theme,
            InteractionState {
                selected: true,
                hovered: true,
                focused: true,
            },
        );
        assert_eq!(focused_selected.fill, colors.selected);
        assert_eq!(focused_selected.outline, colors.focus);
        assert_eq!(focused_selected.icon, colors.focus);
        assert_eq!(focused_selected.outline_width, KANSO_VISUALS.focus_width);
    }
}
