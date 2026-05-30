//! Shared Neon Toolbench widgets for egui surfaces.
//!
//! The helpers here are intentionally small: they keep colour, spacing,
//! radius and button states consistent without introducing a new UI
//! framework or asset dependency.

use bevy_egui::egui;

use crate::icons::{paint_icon, Icon};
use crate::theme::ThemeSettings;

fn alpha_u8(alpha: f32) -> u8 {
    (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn with_alpha(color: egui::Color32, alpha: f32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha_u8(alpha))
}

pub fn toolbench_frame(theme: ThemeSettings) -> egui::Frame {
    let colors = theme.semantic();
    egui::Frame::none()
        .fill(with_alpha(colors.surface_strong, 0.88))
        .stroke(egui::Stroke::new(1.0, with_alpha(colors.stroke, 0.72)))
        .inner_margin(egui::Margin::symmetric(18.0, 16.0))
        .rounding(egui::Rounding::same(8.0))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 12.0),
            blur: 28.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(185),
        })
}

pub fn surface_panel<R>(
    ui: &mut egui::Ui,
    theme: ThemeSettings,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let colors = theme.semantic();
    egui::Frame::none()
        .fill(with_alpha(colors.surface, 0.78))
        .stroke(egui::Stroke::new(1.0, with_alpha(colors.stroke, 0.58)))
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
        .rounding(egui::Rounding::same(8.0))
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
    let rounding = egui::Rounding::same(9.0);
    let base = egui::Color32::from_rgba_unmultiplied(5, 14, 20, alpha_u8(opacity * 0.78));
    let deep = egui::Color32::from_rgba_unmultiplied(1, 4, 8, alpha_u8(opacity * 0.46));
    let top_sheen = egui::Color32::from_rgba_unmultiplied(210, 246, 255, alpha_u8(opacity * 0.15));
    let inner_sheen = egui::Color32::from_white_alpha(alpha_u8(opacity * 0.18));

    painter.rect_filled(rect, rounding, base);
    painter.rect_filled(rect.shrink(1.0), egui::Rounding::same(7.5), deep);

    let top = egui::Rect::from_min_max(
        rect.left_top() + egui::vec2(1.0, 1.0),
        egui::pos2(rect.right() - 1.0, rect.top() + rect.height() * 0.42),
    );
    painter.rect_filled(top, rounding, top_sheen);

    let rim = with_alpha(accent, opacity * 0.78);
    let cool_rim = with_alpha(colors.info, opacity * 0.44);
    painter.rect_stroke(rect, rounding, egui::Stroke::new(1.0, rim));
    painter.rect_stroke(
        rect.shrink(1.5),
        egui::Rounding::same(7.0),
        egui::Stroke::new(1.0, inner_sheen),
    );

    painter.line_segment(
        [
            egui::pos2(rect.left() + 12.0, rect.top() + 2.0),
            egui::pos2(rect.right() - 12.0, rect.top() + 2.0),
        ],
        egui::Stroke::new(1.0, inner_sheen),
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
    let stroke = egui::Stroke::new(1.35, cool_rim);
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
    let fill = if selected {
        colors.accent
    } else if hovered {
        colors.surface_strong
    } else {
        colors.surface
    };
    let text = if selected {
        theme.text_on(fill)
    } else {
        colors.text
    };
    let stroke = if selected || hovered {
        colors.accent
    } else {
        colors.stroke.linear_multiply(0.62)
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::Rounding::same(7.0), fill);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(7.0),
        egui::Stroke::new(1.0, stroke),
    );
    let icon_rect =
        egui::Rect::from_min_size(rect.min + egui::vec2(10.0, 8.0), egui::vec2(20.0, 20.0));
    paint_icon(&painter, icon_rect, icon, text);
    painter.text(
        egui::pos2(rect.left() + 38.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(12.0),
        text,
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
        .stroke(egui::Stroke::new(1.0, colors.stroke.linear_multiply(0.68)))
        .rounding(egui::Rounding::same(6.0))
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
    let fill = if selected {
        colors.accent
    } else if response.hovered() {
        colors.surface_strong
    } else {
        colors.surface
    };
    let text = if selected {
        theme.text_on(fill)
    } else {
        colors.text
    };
    let stroke = if selected || response.hovered() {
        colors.accent
    } else {
        colors.stroke.linear_multiply(0.55)
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::Rounding::same(7.0), fill);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(7.0),
        egui::Stroke::new(1.0, stroke),
    );
    paint_icon(
        &painter,
        egui::Rect::from_min_size(rect.min + egui::vec2(9.0, 8.0), egui::vec2(18.0, 18.0)),
        icon,
        text,
    );
    painter.text(
        egui::pos2(rect.left() + 35.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(11.5),
        text,
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
    let fill = if selected {
        colors.accent
    } else if response.hovered() {
        colors.surface_strong
    } else {
        colors.surface
    };
    let icon_color = if selected {
        theme.text_on(fill)
    } else {
        colors.accent
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::Rounding::same(7.0), fill);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(7.0),
        egui::Stroke::new(
            1.0,
            if selected || response.hovered() {
                colors.accent
            } else {
                colors.stroke.linear_multiply(0.55)
            },
        ),
    );
    paint_icon(&painter, rect.shrink(8.0), icon, icon_color);
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
    let rect = response.rect.expand(2.0);
    ui.painter().rect_stroke(
        rect,
        egui::Rounding::same(6.0),
        egui::Stroke::new(1.0, colors.stroke.linear_multiply(0.55)),
    );
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
        resp.rect,
        egui::Rounding::same(7.0),
        egui::Stroke::new(1.2, colors.danger),
    );
    resp
}

pub fn compact_separator(ui: &mut egui::Ui, theme: ThemeSettings) {
    let colors = theme.semantic();
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 0.0, colors.stroke.linear_multiply(0.55));
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
    let fill = if selected {
        colors.selected
    } else if hovered {
        colors.surface_strong
    } else {
        colors.surface
    };
    let stroke = if selected || hovered {
        colors.accent
    } else {
        colors.stroke.linear_multiply(0.62)
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::Rounding::same(8.0), fill);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(8.0),
        egui::Stroke::new(1.0, stroke),
    );
    let icon_rect =
        egui::Rect::from_min_size(rect.min + egui::vec2(12.0, 14.0), egui::vec2(26.0, 26.0));
    paint_icon(&painter, icon_rect, icon, colors.accent);
    painter.text(
        egui::pos2(rect.left() + 48.0, rect.top() + 18.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(13.0),
        colors.text,
    );
    painter.text(
        egui::pos2(rect.left() + 48.0, rect.top() + 42.0),
        egui::Align2::LEFT_CENTER,
        detail,
        egui::FontId::monospace(10.0),
        colors.text_muted,
    );
    response.on_hover_text(detail)
}
