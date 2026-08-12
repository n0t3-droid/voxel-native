//! Review-first 3D asset workbench.
//!
//! Blueprints use quarter-voxel coordinates so architecture can carry trim,
//! frames and slabs without changing the world's full-voxel storage format.
//! The viewport is paint-only egui geometry: inspecting an asset never mutates
//! the world and never wakes builders or bots.

use std::sync::OnceLock;

use bevy::prelude::*;
use bevy_egui::egui;

use crate::blocks::{block_label, BlockType};
use crate::creator_contract::{
    authorize_commit, evaluate_plan, issue_preview_receipt, CanonicalPayloadBuilder,
    CreatorAdmissionLimits, CreatorCost, CreatorDiagnostic, CreatorObjectId, CreatorPlanEvaluation,
    CreatorPlanSnapshot, CreatorRevision, DiagnosticSeverity, PreviewReceipt,
};
use crate::icons::Icon;
use crate::theme::{MotionRole, ThemeSettings};
use crate::ui_kit::{
    choice_chip_sized, command_action, icon_action, paint_interactive_surface, ActionTone,
};

const OBJECT_LAB_LIMITS: CreatorAdmissionLimits =
    CreatorAdmissionLimits::new(100_000, 100_000, 200_000);

pub struct ObjectLabPlugin;

impl Plugin for ObjectLabPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ObjectLabState>();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DetailTier {
    Full,
    Half,
    Quarter,
}

impl DetailTier {
    const ALL: [Self; 3] = [Self::Full, Self::Half, Self::Quarter];

    fn label(self) -> &'static str {
        match self {
            Self::Full => "1x",
            Self::Half => "1/2",
            Self::Quarter => "1/4",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Full => "Block silhouette",
            Self::Half => "Frames + slabs",
            Self::Quarter => "Architectural trim",
        }
    }

    fn allows(self, required: Self) -> bool {
        self >= required
    }

    const fn stable_tag(self) -> u16 {
        match self {
            Self::Full => 0,
            Self::Half => 1,
            Self::Quarter => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerrainFitMode {
    LevelPad,
    StepFoundation,
    Stilts,
    CutFill,
}

impl TerrainFitMode {
    const ALL: [Self; 4] = [
        Self::LevelPad,
        Self::StepFoundation,
        Self::Stilts,
        Self::CutFill,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::LevelPad => "LEVEL",
            Self::StepFoundation => "STEP",
            Self::Stilts => "STILTS",
            Self::CutFill => "CUT/FILL",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::LevelPad => "One calm platform; best for gentle ground.",
            Self::StepFoundation => "Terraced foundations follow the hillside.",
            Self::Stilts => "Preserves steep terrain with structural piers.",
            Self::CutFill => "Balances excavation and fill around the footprint.",
        }
    }

    const fn stable_tag(self) -> u16 {
        match self {
            Self::LevelPad => 0,
            Self::StepFoundation => 1,
            Self::Stilts => 2,
            Self::CutFill => 3,
        }
    }

    const fn supported_slope(self) -> f32 {
        match self {
            Self::LevelPad => 0.12,
            Self::StepFoundation => 0.30,
            Self::Stilts | Self::CutFill => 0.45,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectCategory {
    Architecture,
    Infrastructure,
}

impl ObjectCategory {
    fn label(self) -> &'static str {
        match self {
            Self::Architecture => "ARCHITECTURE",
            Self::Infrastructure => "INFRASTRUCTURE",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MaterialSlot {
    label: &'static str,
    default: BlockType,
}

#[derive(Debug, Clone, Copy)]
struct VoxelPrimitive {
    /// Quarter-voxel coordinate.
    min: IVec3,
    /// Quarter-voxel extent.
    size: UVec3,
    material_slot: usize,
    minimum_detail: DetailTier,
}

#[derive(Debug)]
struct ObjectBlueprint {
    id: &'static str,
    label: &'static str,
    version: &'static str,
    description: &'static str,
    dimensions: UVec3,
    category: ObjectCategory,
    default_fit: TerrainFitMode,
    recommended_slope: f32,
    slots: [MaterialSlot; 4],
    primitives: Vec<VoxelPrimitive>,
}

#[derive(Resource)]
pub struct ObjectLabState {
    query: String,
    selected: usize,
    loaded_asset: usize,
    yaw: f32,
    pitch: f32,
    zoom: f32,
    scale: f32,
    detail: DetailTier,
    fit: TerrainFitMode,
    terrain_slope: f32,
    show_grid: bool,
    show_foundation: bool,
    material_overrides: [BlockType; 4],
    revision: CreatorRevision,
    review_receipt: Option<PreviewReceipt>,
    status: String,
}

impl Default for ObjectLabState {
    fn default() -> Self {
        let blueprint = &object_catalog()[0];
        Self {
            query: String::new(),
            selected: 0,
            loaded_asset: 0,
            yaw: -0.72,
            pitch: 0.38,
            zoom: 1.0,
            scale: 1.0,
            detail: DetailTier::Quarter,
            fit: blueprint.default_fit,
            terrain_slope: blueprint.recommended_slope,
            show_grid: true,
            show_foundation: true,
            material_overrides: blueprint.slots.map(|slot| slot.default),
            revision: CreatorRevision::INITIAL,
            review_receipt: None,
            status: "Review only - no world changes".to_owned(),
        }
    }
}

pub fn draw_object_lab(ui: &mut egui::Ui, state: &mut ObjectLabState, theme: ThemeSettings) {
    sync_selected_asset(state);
    let colors = theme.semantic();
    let available = ui.available_size();
    let workbench_height = (available.y - 44.0).max(410.0);

    ui.horizontal(|ui| {
        ui.heading(
            egui::RichText::new("OBJECT LAB")
                .color(colors.text)
                .strong(),
        );
        status_chip(ui, "SAFE PREVIEW", colors.success, colors.surface_strong);
        status_chip(ui, "QTR-VOXEL", colors.info, colors.surface_strong);
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(&state.status)
                .small()
                .color(colors.text_muted),
        );
    });
    ui.add_space(6.0);

    let left_width = 205.0;
    let right_width = 258.0;
    let gap = 10.0;
    let viewport_width = (available.x - left_width - right_width - gap * 2.0).max(360.0);

    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(left_width, workbench_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| draw_asset_browser(ui, state, theme),
        );
        ui.add_space(gap);
        ui.allocate_ui_with_layout(
            egui::vec2(viewport_width, workbench_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| draw_asset_viewport(ui, state, theme),
        );
        ui.add_space(gap);
        ui.allocate_ui_with_layout(
            egui::vec2(right_width, workbench_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| draw_inspector(ui, state, theme),
        );
    });
}

fn draw_asset_browser(ui: &mut egui::Ui, state: &mut ObjectLabState, theme: ThemeSettings) {
    let colors = theme.semantic();
    section_title(ui, "LIBRARY", "versioned blueprints", theme);
    ui.add(
        egui::TextEdit::singleline(&mut state.query)
            .hint_text("Search objects...")
            .desired_width(f32::INFINITY),
    );
    ui.add_space(6.0);

    let query = state.query.trim().to_ascii_lowercase();
    egui::ScrollArea::vertical()
        .id_source("object_lab_library")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, asset) in object_catalog().iter().enumerate() {
                if !query.is_empty()
                    && !asset.label.to_ascii_lowercase().contains(&query)
                    && !asset.category.label().to_ascii_lowercase().contains(&query)
                {
                    continue;
                }

                let selected = index == state.selected;
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width().max(1.0), 78.0),
                    egui::Sense::click(),
                );
                let surface = paint_interactive_surface(
                    ui,
                    rect,
                    &response,
                    selected,
                    MotionRole::State,
                    theme,
                );
                let paint_rect = surface.paint_rect;
                let painter = ui.painter_at(rect.expand(5.0));
                painter.text(
                    egui::pos2(paint_rect.left() + 12.0, paint_rect.top() + 17.0),
                    egui::Align2::LEFT_CENTER,
                    asset.label,
                    egui::FontId::monospace(13.0),
                    surface.text,
                );
                painter.text(
                    egui::pos2(paint_rect.left() + 12.0, paint_rect.top() + 38.0),
                    egui::Align2::LEFT_CENTER,
                    asset.category.label(),
                    egui::FontId::monospace(10.0),
                    colors.info,
                );
                painter.text(
                    egui::pos2(paint_rect.right() - 12.0, paint_rect.top() + 38.0),
                    egui::Align2::RIGHT_CENTER,
                    asset.version,
                    egui::FontId::monospace(10.0),
                    surface.detail,
                );
                painter.text(
                    egui::pos2(paint_rect.left() + 12.0, paint_rect.top() + 59.0),
                    egui::Align2::LEFT_CENTER,
                    asset.description,
                    egui::FontId::monospace(9.0),
                    surface.detail,
                );

                if response.clicked() {
                    select_asset(state, index);
                }
                response.on_hover_text(asset.description);
                ui.add_space(5.0);
            }
        });
}

fn draw_asset_viewport(ui: &mut egui::Ui, state: &mut ObjectLabState, theme: ThemeSettings) {
    let colors = theme.semantic();
    let blueprint = &object_catalog()[state.selected];

    ui.horizontal(|ui| {
        section_title(ui, "3D PREVIEW", blueprint.label, theme);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icon_action(ui, Icon::Rotate, "Reset", false, theme).clicked() {
                state.yaw = -0.72;
                state.pitch = 0.38;
                state.zoom = 1.0;
            }
        });
    });

    let desired = egui::vec2(
        ui.available_width(),
        (ui.available_height() - 72.0).max(300.0),
    );
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click_and_drag());
    let surface =
        paint_interactive_surface(ui, rect, &response, false, MotionRole::Feedback, theme);
    let paint_rect = surface.paint_rect;
    let painter = ui.painter_at(rect.expand(5.0));

    if response.dragged() {
        let delta = ui.input(|input| input.pointer.delta());
        state.yaw += delta.x * 0.009;
        state.pitch = (state.pitch + delta.y * 0.007).clamp(-1.15, 1.15);
    }
    if response.hovered() {
        let scroll = ui.input(|input| input.smooth_scroll_delta.y);
        if scroll.abs() > f32::EPSILON {
            state.zoom = (state.zoom * (1.0 + scroll * 0.0018)).clamp(0.35, 3.2);
        }
    }
    if response.double_clicked() {
        state.yaw = -0.72;
        state.pitch = 0.38;
        state.zoom = 1.0;
    }

    paint_preview(&painter, paint_rect.shrink(8.0), blueprint, state, theme);

    let visible = visible_primitives(blueprint, state.detail).count();
    let cells = estimated_cells(blueprint, state.detail, state.scale);
    let footer = egui::Rect::from_min_max(
        egui::pos2(paint_rect.left() + 10.0, paint_rect.bottom() - 34.0),
        egui::pos2(paint_rect.right() - 10.0, paint_rect.bottom() - 8.0),
    );
    painter.rect_filled(
        footer,
        egui::Rounding::same(3.0),
        egui::Color32::from_black_alpha(190),
    );
    painter.text(
        footer.left_center() + egui::vec2(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "DRAG ORBIT  //  WHEEL ZOOM  //  DOUBLE CLICK RESET",
        egui::FontId::monospace(10.0),
        colors.text_muted,
    );
    painter.text(
        footer.right_center() - egui::vec2(8.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        format!("{visible} PARTS  //  ~{cells} CELLS"),
        egui::FontId::monospace(10.0),
        colors.info,
    );
}

fn draw_inspector(ui: &mut egui::Ui, state: &mut ObjectLabState, theme: ThemeSettings) {
    let colors = theme.semantic();
    let blueprint = &object_catalog()[state.selected];

    section_title(ui, "INSPECTOR", blueprint.id, theme);
    egui::ScrollArea::vertical()
        .id_source("object_lab_inspector")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            property_row(ui, "Version", blueprint.version, theme);
            property_row(
                ui,
                "Size",
                &format!(
                    "{:.1} x {:.1} x {:.1} vox",
                    blueprint.dimensions.x as f32 / 4.0,
                    blueprint.dimensions.y as f32 / 4.0,
                    blueprint.dimensions.z as f32 / 4.0
                ),
                theme,
            );

            ui.add_space(8.0);
            inspector_heading(ui, "DETAIL", theme);
            ui.horizontal(|ui| {
                for tier in DetailTier::ALL {
                    if choice_chip_sized(ui, tier.label(), state.detail == tier, 68.0, theme)
                        .on_hover_text(tier.description())
                        .clicked()
                        && state.detail != tier
                    {
                        state.detail = tier;
                        mark_plan_changed(state, format!("{} detail preview", tier.description()));
                    }
                }
            });
            let scale_response = ui.add(
                egui::Slider::new(&mut state.scale, 0.5..=2.0)
                    .text("Scale")
                    .clamp_to_range(true),
            );
            if scale_response.changed() {
                mark_plan_changed(state, format!("Scale set to {:.2}x", state.scale));
            }

            ui.add_space(8.0);
            inspector_heading(ui, "TERRAIN FIT", theme);
            ui.horizontal_wrapped(|ui| {
                for mode in TerrainFitMode::ALL {
                    if choice_chip_sized(ui, mode.label(), state.fit == mode, 92.0, theme)
                        .on_hover_text(mode.description())
                        .clicked()
                        && state.fit != mode
                    {
                        state.fit = mode;
                        mark_plan_changed(state, format!("{} terrain strategy", mode.label()));
                    }
                }
            });
            let slope_response = ui.add(
                egui::Slider::new(&mut state.terrain_slope, -0.45..=0.45)
                    .text("Slope")
                    .clamp_to_range(true),
            );
            if slope_response.changed() {
                mark_plan_changed(
                    state,
                    format!("Terrain slope set to {:.2}", state.terrain_slope),
                );
            }
            let foundation_response = ui.checkbox(&mut state.show_foundation, "Preview foundation");
            if foundation_response.changed() {
                mark_plan_changed(
                    state,
                    if state.show_foundation {
                        "Foundation included in reviewed plan"
                    } else {
                        "Foundation removed from reviewed plan"
                    },
                );
            }
            ui.checkbox(&mut state.show_grid, "Show quarter-voxel grid");
            ui.label(
                egui::RichText::new(state.fit.description())
                    .small()
                    .color(colors.text_muted),
            );

            ui.add_space(8.0);
            inspector_heading(ui, "MATERIAL SLOTS", theme);
            for (slot_index, slot) in blueprint.slots.iter().enumerate() {
                let previous_material = state.material_overrides[slot_index];
                ui.horizontal(|ui| {
                    material_swatch(
                        ui,
                        state.material_overrides[slot_index],
                        egui::vec2(18.0, 18.0),
                    );
                    ui.label(
                        egui::RichText::new(slot.label)
                            .small()
                            .color(colors.text_muted),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_source(("object_material", slot_index))
                            .selected_text(block_label(state.material_overrides[slot_index]))
                            .width(105.0)
                            .show_ui(ui, |ui| {
                                for material in material_choices() {
                                    ui.selectable_value(
                                        &mut state.material_overrides[slot_index],
                                        *material,
                                        block_label(*material),
                                    );
                                }
                            });
                    });
                });
                if state.material_overrides[slot_index] != previous_material {
                    mark_plan_changed(
                        state,
                        format!(
                            "{} material set to {}",
                            slot.label,
                            block_label(state.material_overrides[slot_index])
                        ),
                    );
                }
            }

            ui.add_space(8.0);
            inspector_heading(ui, "PLACEMENT CHECK", theme);
            let evaluation = object_lab_evaluation(state, blueprint);
            property_row(
                ui,
                "Revision",
                &evaluation.revision.get().to_string(),
                theme,
            );
            property_row(
                ui,
                "Voxel edits",
                &format!(
                    "{} / {}",
                    evaluation.cost.voxel_edits, OBJECT_LAB_LIMITS.max_voxel_edits
                ),
                theme,
            );
            property_row(
                ui,
                "Memory",
                &format!(
                    "{} / {} KiB",
                    evaluation.cost.estimated_bytes.div_ceil(1024),
                    OBJECT_LAB_LIMITS.max_estimated_bytes.div_ceil(1024)
                ),
                theme,
            );
            property_row(
                ui,
                "Validation",
                &format!(
                    "{} errors / {} warnings",
                    evaluation.error_count(),
                    evaluation.warning_count()
                ),
                theme,
            );
            if evaluation.diagnostics.is_empty() {
                diagnostic_row(
                    ui,
                    &CreatorDiagnostic::info(
                        "creator.ready",
                        "Geometry, terrain and budget are admissible",
                    ),
                    theme,
                );
            } else {
                for diagnostic in &evaluation.diagnostics {
                    diagnostic_row(ui, diagnostic, theme);
                }
            }

            ui.add_space(10.0);
            let review_current = state
                .review_receipt
                .as_ref()
                .is_some_and(|receipt| authorize_commit(receipt, &evaluation).is_ok());
            if let Some(receipt) = state
                .review_receipt
                .as_ref()
                .filter(|receipt| authorize_commit(receipt, &evaluation).is_ok())
            {
                property_row(
                    ui,
                    "Receipt",
                    &format!("{} / REV {}", receipt.short_code(), receipt.revision.get()),
                    theme,
                );
            }
            let ready_text = if review_current {
                "REVIEWED - EXACT REVISION"
            } else {
                "MARK REVIEWED"
            };
            if command_action(
                ui,
                ready_text,
                Some("Local review only"),
                ActionTone::Primary,
                38.0,
                theme,
            )
            .clicked()
            {
                match issue_preview_receipt(&evaluation) {
                    Ok(receipt) => {
                        let code = receipt.short_code();
                        state.review_receipt = Some(receipt);
                        state.status = format!(
                            "Receipt {code} issued - exact revision is safe for a future command"
                        );
                    }
                    Err(rejected) => {
                        state.review_receipt = None;
                        state.status = format!(
                            "Review blocked by {} contract error(s)",
                            rejected.error_count()
                        );
                    }
                }
            }
            ui.label(
                egui::RichText::new("This action does not edit the world or start a bot.")
                    .small()
                    .color(colors.text_muted),
            );
        });
}

fn paint_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    blueprint: &ObjectBlueprint,
    state: &ObjectLabState,
    theme: ThemeSettings,
) {
    let colors = theme.semantic();
    let dims = blueprint.dimensions.as_vec3() / 4.0;
    let horizontal = dims.x.max(dims.z).max(1.0);
    let vertical = dims.y.max(1.0);
    let pixels_per_voxel = (rect.width() / (horizontal * 1.65))
        .min(rect.height() / (vertical * 1.55))
        * state.zoom
        * state.scale;
    let projection = PreviewProjection {
        center: egui::pos2(rect.center().x, rect.center().y + rect.height() * 0.12),
        pixels_per_voxel,
        yaw: state.yaw,
        pitch: state.pitch,
        object_center: Vec3::new(dims.x * 0.5, dims.y * 0.5, dims.z * 0.5),
    };

    paint_terrain_grid(painter, rect, projection, state, theme);

    let mut faces = Vec::new();
    if state.show_foundation {
        add_foundation_faces(&mut faces, blueprint, state, projection, colors.outline);
    }
    for primitive in visible_primitives(blueprint, state.detail) {
        add_cuboid_faces(
            &mut faces,
            primitive,
            state.material_overrides[primitive.material_slot],
            projection,
            1.0,
        );
    }
    faces.sort_by(|a, b| {
        a.depth
            .partial_cmp(&b.depth)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for face in faces {
        painter.add(egui::Shape::convex_polygon(
            face.points.to_vec(),
            face.fill,
            face.stroke,
        ));
    }

    if state.show_grid {
        paint_quarter_grid_hint(painter, rect, state.detail, colors.text_muted);
    }
}

#[derive(Clone, Copy)]
struct PreviewProjection {
    center: egui::Pos2,
    pixels_per_voxel: f32,
    yaw: f32,
    pitch: f32,
    object_center: Vec3,
}

impl PreviewProjection {
    fn view(self, point: Vec3) -> Vec3 {
        let mut p = point - self.object_center;
        let (sy, cy) = self.yaw.sin_cos();
        p = Vec3::new(cy * p.x + sy * p.z, p.y, -sy * p.x + cy * p.z);
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(p.x, cp * p.y - sp * p.z, sp * p.y + cp * p.z)
    }

    fn screen(self, point: Vec3) -> (egui::Pos2, f32) {
        let view = self.view(point);
        let perspective = (1.0 + view.z * 0.018).clamp(0.72, 1.35);
        (
            egui::pos2(
                self.center.x + view.x * self.pixels_per_voxel / perspective,
                self.center.y - view.y * self.pixels_per_voxel / perspective,
            ),
            view.z,
        )
    }
}

struct PaintedFace {
    points: [egui::Pos2; 4],
    depth: f32,
    fill: egui::Color32,
    stroke: egui::Stroke,
}

fn add_cuboid_faces(
    output: &mut Vec<PaintedFace>,
    primitive: &VoxelPrimitive,
    material: BlockType,
    projection: PreviewProjection,
    alpha: f32,
) {
    let min = primitive.min.as_vec3() / 4.0;
    let max = min + primitive.size.as_vec3() / 4.0;
    add_box_faces(
        output,
        min,
        max,
        material_color(material),
        projection,
        alpha,
    );
}

fn add_box_faces(
    output: &mut Vec<PaintedFace>,
    min: Vec3,
    max: Vec3,
    base: egui::Color32,
    projection: PreviewProjection,
    alpha: f32,
) {
    let vertices = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(max.x, max.y, max.z),
        Vec3::new(min.x, max.y, max.z),
    ];
    let face_indices = [
        ([0, 1, 2, 3], 0.72),
        ([5, 4, 7, 6], 0.88),
        ([4, 0, 3, 7], 0.64),
        ([1, 5, 6, 2], 0.82),
        ([3, 2, 6, 7], 1.08),
        ([4, 5, 1, 0], 0.56),
    ];
    let transparent = matches!(
        base,
        egui::Color32 {
            ..
        } if base.a() < 210
    );
    for (indices, shade) in face_indices {
        let mut points = [egui::Pos2::ZERO; 4];
        let mut depth = 0.0;
        for (target, source) in points.iter_mut().zip(indices) {
            let (screen, z) = projection.screen(vertices[source]);
            *target = screen;
            depth += z;
        }
        let mut fill = shade_color(base, shade);
        let target_alpha = if transparent {
            92
        } else {
            (alpha * 235.0) as u8
        };
        fill = egui::Color32::from_rgba_unmultiplied(
            fill.r(),
            fill.g(),
            fill.b(),
            target_alpha.min(fill.a()),
        );
        output.push(PaintedFace {
            points,
            depth: depth * 0.25,
            fill,
            stroke: egui::Stroke::new(
                0.8,
                egui::Color32::from_rgba_unmultiplied(
                    base.r().saturating_add(28),
                    base.g().saturating_add(28),
                    base.b().saturating_add(28),
                    160,
                ),
            ),
        });
    }
}

fn add_foundation_faces(
    output: &mut Vec<PaintedFace>,
    blueprint: &ObjectBlueprint,
    state: &ObjectLabState,
    projection: PreviewProjection,
    outline: egui::Color32,
) {
    let dims = blueprint.dimensions.as_vec3() / 4.0;
    let slope = state.terrain_slope;
    let foundation_color = egui::Color32::from_rgba_unmultiplied(
        outline.r().saturating_add(24),
        outline.g().saturating_add(24),
        outline.b().saturating_add(24),
        205,
    );
    match state.fit {
        TerrainFitMode::LevelPad => add_box_faces(
            output,
            Vec3::new(-0.5, -0.45, -0.5),
            Vec3::new(dims.x + 0.5, 0.0, dims.z + 0.5),
            foundation_color,
            projection,
            0.9,
        ),
        TerrainFitMode::StepFoundation | TerrainFitMode::CutFill => {
            let steps = 4;
            for step in 0..steps {
                let x0 = dims.x * step as f32 / steps as f32;
                let x1 = dims.x * (step + 1) as f32 / steps as f32;
                let terrain = slope * (x0 - dims.x * 0.5);
                add_box_faces(
                    output,
                    Vec3::new(x0, terrain.min(0.0) - 0.35, -0.25),
                    Vec3::new(x1, 0.02, dims.z + 0.25),
                    foundation_color,
                    projection,
                    0.88,
                );
            }
        }
        TerrainFitMode::Stilts => {
            for x in [0.5, dims.x - 0.5] {
                for z in [0.5, dims.z - 0.5] {
                    let terrain = slope * (x - dims.x * 0.5);
                    add_box_faces(
                        output,
                        Vec3::new(x - 0.16, terrain.min(0.0) - 1.2, z - 0.16),
                        Vec3::new(x + 0.16, 0.04, z + 0.16),
                        foundation_color,
                        projection,
                        0.92,
                    );
                }
            }
        }
    }
}

fn paint_terrain_grid(
    painter: &egui::Painter,
    rect: egui::Rect,
    projection: PreviewProjection,
    state: &ObjectLabState,
    theme: ThemeSettings,
) {
    let colors = theme.semantic();
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.center().y),
            egui::pos2(rect.right(), rect.bottom()),
        ),
        0.0,
        egui::Color32::from_rgba_unmultiplied(
            colors.surface.r(),
            colors.surface.g(),
            colors.surface.b(),
            82,
        ),
    );
    let span = 26;
    for i in -span..=span {
        let x = projection.object_center.x + i as f32;
        let y0 = terrain_height(
            x,
            -span as f32,
            projection.object_center.x,
            state.terrain_slope,
        );
        let y1 = terrain_height(
            x,
            span as f32,
            projection.object_center.x,
            state.terrain_slope,
        );
        let (a, _) = projection.screen(Vec3::new(x, y0 - 0.05, -span as f32));
        let (b, _) = projection.screen(Vec3::new(x, y1 - 0.05, span as f32));
        painter.line_segment(
            [a, b],
            egui::Stroke::new(
                if i % 4 == 0 { 0.8 } else { 0.35 },
                egui::Color32::from_rgba_unmultiplied(
                    colors.outline.r(),
                    colors.outline.g(),
                    colors.outline.b(),
                    if i % 4 == 0 { 88 } else { 42 },
                ),
            ),
        );
    }
    for i in -span..=span {
        let z = i as f32;
        let x0 = projection.object_center.x - span as f32;
        let x1 = projection.object_center.x + span as f32;
        let y0 = terrain_height(x0, z, projection.object_center.x, state.terrain_slope);
        let y1 = terrain_height(x1, z, projection.object_center.x, state.terrain_slope);
        let (a, _) = projection.screen(Vec3::new(x0, y0 - 0.05, z));
        let (b, _) = projection.screen(Vec3::new(x1, y1 - 0.05, z));
        painter.line_segment(
            [a, b],
            egui::Stroke::new(
                if i % 4 == 0 { 0.8 } else { 0.35 },
                egui::Color32::from_rgba_unmultiplied(
                    colors.outline.r(),
                    colors.outline.g(),
                    colors.outline.b(),
                    if i % 4 == 0 { 76 } else { 36 },
                ),
            ),
        );
    }
}

fn terrain_height(x: f32, z: f32, center_x: f32, slope: f32) -> f32 {
    slope * (x - center_x) + (z * 0.07).sin() * slope.abs() * 0.35
}

fn paint_quarter_grid_hint(
    painter: &egui::Painter,
    rect: egui::Rect,
    detail: DetailTier,
    color: egui::Color32,
) {
    let label = match detail {
        DetailTier::Full => "GRID 1.00 VOX",
        DetailTier::Half => "GRID 0.50 VOX",
        DetailTier::Quarter => "GRID 0.25 VOX",
    };
    painter.text(
        rect.left_top() + egui::vec2(9.0, 9.0),
        egui::Align2::LEFT_TOP,
        label,
        egui::FontId::monospace(10.0),
        color,
    );
}

fn section_title(ui: &mut egui::Ui, title: &str, subtitle: &str, theme: ThemeSettings) {
    let colors = theme.semantic();
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .small()
                .strong()
                .color(colors.accent),
        );
        ui.label(
            egui::RichText::new(subtitle)
                .small()
                .color(colors.text_muted),
        );
    });
}

fn inspector_heading(ui: &mut egui::Ui, title: &str, theme: ThemeSettings) {
    let colors = theme.semantic();
    ui.label(
        egui::RichText::new(title)
            .small()
            .strong()
            .color(colors.accent),
    );
    ui.separator();
}

fn property_row(ui: &mut egui::Ui, label: &str, value: &str, theme: ThemeSettings) {
    let colors = theme.semantic();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).small().color(colors.text_muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(value).small().color(colors.text));
        });
    });
}

fn diagnostic_row(ui: &mut egui::Ui, diagnostic: &CreatorDiagnostic, theme: ThemeSettings) {
    let colors = theme.semantic();
    let (marker, tone) = match diagnostic.severity {
        DiagnosticSeverity::Info => ("i", colors.info),
        DiagnosticSeverity::Warning => ("!", colors.warning),
        DiagnosticSeverity::Error => ("x", colors.danger),
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(marker).strong().color(tone));
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(&diagnostic.code)
                    .small()
                    .strong()
                    .color(colors.text),
            );
            ui.label(
                egui::RichText::new(&diagnostic.message)
                    .small()
                    .color(colors.text_muted),
            );
        });
    });
}

fn status_chip(ui: &mut egui::Ui, label: &str, color: egui::Color32, background: egui::Color32) {
    egui::Frame::none()
        .fill(background)
        .stroke(egui::Stroke::new(1.0, color))
        .rounding(egui::Rounding::same(3.0))
        .inner_margin(egui::Margin::symmetric(6.0, 2.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).small().strong().color(color));
        });
}

fn material_swatch(ui: &mut egui::Ui, block: BlockType, size: egui::Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, egui::Rounding::same(2.0), material_color(block));
    ui.painter().rect_stroke(
        rect,
        egui::Rounding::same(2.0),
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(100)),
    );
}

fn material_color(block: BlockType) -> egui::Color32 {
    let srgb = block.color().to_srgba();
    let alpha = if matches!(
        block,
        BlockType::CockpitGlass | BlockType::NeonGlass | BlockType::ShojiPaper
    ) {
        150
    } else {
        (srgb.alpha * 255.0) as u8
    };
    egui::Color32::from_rgba_unmultiplied(
        (srgb.red * 255.0) as u8,
        (srgb.green * 255.0) as u8,
        (srgb.blue * 255.0) as u8,
        alpha,
    )
}

fn shade_color(color: egui::Color32, factor: f32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (color.r() as f32 * factor).clamp(0.0, 255.0) as u8,
        (color.g() as f32 * factor).clamp(0.0, 255.0) as u8,
        (color.b() as f32 * factor).clamp(0.0, 255.0) as u8,
        color.a(),
    )
}

fn sync_selected_asset(state: &mut ObjectLabState) {
    if state.loaded_asset != state.selected {
        select_asset(state, state.selected);
    }
}

fn select_asset(state: &mut ObjectLabState, index: usize) {
    let index = index.min(object_catalog().len().saturating_sub(1));
    let asset = &object_catalog()[index];
    state.selected = index;
    state.loaded_asset = index;
    state.fit = asset.default_fit;
    state.terrain_slope = asset.recommended_slope;
    state.material_overrides = asset.slots.map(|slot| slot.default);
    mark_plan_changed(
        state,
        format!("Loaded {} {} for local review", asset.label, asset.version),
    );
}

fn mark_plan_changed(state: &mut ObjectLabState, status: impl Into<String>) {
    state.revision.next();
    state.review_receipt = None;
    state.status = status.into();
}

fn object_lab_evaluation(
    state: &ObjectLabState,
    blueprint: &ObjectBlueprint,
) -> CreatorPlanEvaluation {
    evaluate_plan(
        &object_lab_plan_snapshot(state, blueprint),
        OBJECT_LAB_LIMITS,
    )
}

fn object_lab_plan_snapshot(
    state: &ObjectLabState,
    blueprint: &ObjectBlueprint,
) -> CreatorPlanSnapshot {
    let mut payload = CanonicalPayloadBuilder::new("object-lab.blueprint.v1");
    payload
        .push_str(blueprint.id)
        .push_str(blueprint.version)
        .push_u16(state.detail.stable_tag())
        .push_i32(quantize_milli(state.scale))
        .push_u16(state.fit.stable_tag())
        .push_i32(quantize_milli(state.terrain_slope))
        .push_bool(state.show_foundation);
    for material in state.material_overrides {
        payload.push_u16(material as u16);
    }

    let estimated_cells = estimated_cells(blueprint, state.detail, state.scale);
    let mut diagnostics = Vec::new();
    if !blueprint_bounds_valid(blueprint) {
        diagnostics.push(CreatorDiagnostic::error(
            "geometry.bounds",
            "One or more primitives exceed the declared blueprint bounds",
        ));
    }
    if state.terrain_slope.abs() > state.fit.supported_slope() {
        diagnostics.push(CreatorDiagnostic::error(
            "terrain.slope_unsupported",
            format!(
                "{} supports slopes up to {:.2}; current slope is {:.2}",
                state.fit.label(),
                state.fit.supported_slope(),
                state.terrain_slope.abs()
            ),
        ));
    }
    if estimated_cells >= OBJECT_LAB_LIMITS.max_preview_cells * 4 / 5 {
        diagnostics.push(CreatorDiagnostic::warning(
            "budget.preview_headroom",
            "Preview uses at least 80% of the low-end cell budget",
        ));
    }

    CreatorPlanSnapshot::new(
        CreatorObjectId::new(blueprint.id),
        state.revision,
        payload.finish(),
        CreatorCost {
            voxel_edits: estimated_cells,
            preview_cells: estimated_cells,
            estimated_bytes: estimated_cells.saturating_mul(2),
        },
        diagnostics,
    )
}

fn quantize_milli(value: f32) -> i32 {
    (value * 1_000.0).round() as i32
}

fn blueprint_bounds_valid(blueprint: &ObjectBlueprint) -> bool {
    blueprint.primitives.iter().all(|part| {
        part.size.x > 0
            && part.size.y > 0
            && part.size.z > 0
            && part.material_slot < blueprint.slots.len()
            && part.min.x >= 0
            && part.min.y >= 0
            && part.min.z >= 0
            && (part.min.as_uvec3() + part.size)
                .cmple(blueprint.dimensions)
                .all()
    })
}

fn visible_primitives(
    blueprint: &ObjectBlueprint,
    detail: DetailTier,
) -> impl Iterator<Item = &VoxelPrimitive> {
    blueprint
        .primitives
        .iter()
        .filter(move |primitive| detail.allows(primitive.minimum_detail))
}

fn estimated_cells(blueprint: &ObjectBlueprint, detail: DetailTier, scale: f32) -> u64 {
    let quarter_units: u64 = visible_primitives(blueprint, detail)
        .map(|primitive| {
            primitive.size.x as u64 * primitive.size.y as u64 * primitive.size.z as u64
        })
        .sum();
    let divisor = match detail {
        DetailTier::Full => 64,
        DetailTier::Half => 8,
        DetailTier::Quarter => 1,
    };
    ((quarter_units / divisor).max(1) as f32 * scale.powi(3)) as u64
}

fn material_choices() -> &'static [BlockType] {
    &[
        BlockType::ZenStone,
        BlockType::Limestone,
        BlockType::Stone,
        BlockType::Wood,
        BlockType::RoofTile,
        BlockType::ShipHullDark,
        BlockType::ShipHullAlloy,
        BlockType::CockpitGlass,
        BlockType::NeonGlass,
        BlockType::ShojiPaper,
        BlockType::TatamiMat,
        BlockType::Bamboo,
    ]
}

fn primitive(
    min: (i32, i32, i32),
    size: (u32, u32, u32),
    material_slot: usize,
    minimum_detail: DetailTier,
) -> VoxelPrimitive {
    VoxelPrimitive {
        min: IVec3::new(min.0, min.1, min.2),
        size: UVec3::new(size.0, size.1, size.2),
        material_slot,
        minimum_detail,
    }
}

fn object_catalog() -> &'static [ObjectBlueprint] {
    static CATALOG: OnceLock<Vec<ObjectBlueprint>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            vec![
                cantilever_glass_house(),
                hillside_courtyard_villa(),
                skyline_micro_tower(),
            ]
        })
        .as_slice()
}

fn cantilever_glass_house() -> ObjectBlueprint {
    let mut parts = vec![
        primitive((0, 0, 0), (96, 2, 56), 0, DetailTier::Half),
        primitive((0, 2, 0), (3, 28, 56), 0, DetailTier::Full),
        primitive((93, 2, 0), (3, 28, 56), 0, DetailTier::Full),
        primitive((0, 2, 0), (96, 28, 3), 0, DetailTier::Full),
        primitive((0, 2, 53), (96, 28, 3), 0, DetailTier::Full),
        primitive((0, 30, 0), (96, 2, 56), 1, DetailTier::Half),
        primitive((20, 2, 3), (2, 26, 50), 0, DetailTier::Half),
        primitive((48, 2, 3), (2, 26, 50), 0, DetailTier::Half),
        primitive((70, 2, 3), (2, 26, 50), 0, DetailTier::Half),
        primitive((20, 2, 3), (28, 1, 50), 3, DetailTier::Quarter),
        primitive((72, 8, 3), (21, 18, 2), 2, DetailTier::Half),
        primitive((72, 8, 51), (21, 18, 2), 2, DetailTier::Half),
        primitive((72, 8, 5), (2, 18, 46), 2, DetailTier::Half),
        primitive((91, 8, 5), (2, 18, 46), 2, DetailTier::Half),
        primitive((72, 26, 5), (21, 2, 46), 1, DetailTier::Quarter),
        primitive((8, 2, 12), (12, 1, 32), 3, DetailTier::Quarter),
        primitive((5, 10, 2), (13, 12, 2), 2, DetailTier::Quarter),
        primitive((5, 10, 52), (13, 12, 2), 2, DetailTier::Quarter),
    ];
    for x in [24, 32, 40, 52, 60, 68, 76, 84] {
        parts.push(primitive((x, 4, 1), (1, 24, 2), 1, DetailTier::Quarter));
        parts.push(primitive((x, 4, 53), (1, 24, 2), 1, DetailTier::Quarter));
    }
    for z in [8, 16, 24, 32, 40, 48] {
        parts.push(primitive((94, 4, z), (1, 24, 1), 1, DetailTier::Quarter));
    }
    ObjectBlueprint {
        id: "arch.cantilever-glass-house",
        label: "Cantilever Glass House",
        version: "v1.0.0",
        description: "Low horizontal villa with a glazed floating studio.",
        dimensions: UVec3::new(96, 32, 56),
        category: ObjectCategory::Architecture,
        default_fit: TerrainFitMode::Stilts,
        recommended_slope: 0.18,
        slots: [
            MaterialSlot {
                label: "Structure",
                default: BlockType::ZenStone,
            },
            MaterialSlot {
                label: "Frame",
                default: BlockType::ShipHullDark,
            },
            MaterialSlot {
                label: "Glass",
                default: BlockType::CockpitGlass,
            },
            MaterialSlot {
                label: "Interior",
                default: BlockType::TatamiMat,
            },
        ],
        primitives: parts,
    }
}

fn hillside_courtyard_villa() -> ObjectBlueprint {
    let mut parts = vec![
        primitive((0, 0, 0), (112, 2, 72), 0, DetailTier::Half),
        primitive((0, 2, 0), (112, 24, 3), 0, DetailTier::Full),
        primitive((0, 2, 69), (112, 24, 3), 0, DetailTier::Full),
        primitive((0, 2, 0), (3, 24, 72), 0, DetailTier::Full),
        primitive((109, 2, 0), (3, 24, 72), 0, DetailTier::Full),
        primitive((30, 2, 18), (52, 2, 36), 3, DetailTier::Half),
        primitive((28, 2, 16), (3, 24, 40), 0, DetailTier::Half),
        primitive((81, 2, 16), (3, 24, 40), 0, DetailTier::Half),
        primitive((28, 2, 16), (56, 24, 3), 0, DetailTier::Half),
        primitive((28, 2, 53), (56, 24, 3), 0, DetailTier::Half),
        primitive((0, 26, 0), (112, 2, 18), 1, DetailTier::Half),
        primitive((0, 26, 54), (112, 2, 18), 1, DetailTier::Half),
        primitive((0, 26, 18), (30, 2, 36), 1, DetailTier::Half),
        primitive((82, 26, 18), (30, 2, 36), 1, DetailTier::Half),
        primitive((44, 2, 28), (24, 1, 16), 2, DetailTier::Quarter),
        primitive((42, 2, 26), (28, 1, 2), 0, DetailTier::Quarter),
        primitive((42, 2, 44), (28, 1, 2), 0, DetailTier::Quarter),
        primitive((42, 2, 28), (2, 1, 16), 0, DetailTier::Quarter),
        primitive((68, 2, 28), (2, 1, 16), 0, DetailTier::Quarter),
    ];
    for x in (5..108).step_by(10) {
        parts.push(primitive((x, 7, 1), (1, 15, 2), 2, DetailTier::Quarter));
        parts.push(primitive((x, 7, 69), (1, 15, 2), 2, DetailTier::Quarter));
    }
    for z in (8..68).step_by(10) {
        parts.push(primitive((1, 7, z), (2, 15, 1), 2, DetailTier::Quarter));
        parts.push(primitive((109, 7, z), (2, 15, 1), 2, DetailTier::Quarter));
    }
    ObjectBlueprint {
        id: "arch.hillside-courtyard-villa",
        label: "Hillside Courtyard Villa",
        version: "v1.0.0",
        description: "Terraced modern home wrapped around a calm inner court.",
        dimensions: UVec3::new(112, 28, 72),
        category: ObjectCategory::Architecture,
        default_fit: TerrainFitMode::StepFoundation,
        recommended_slope: -0.24,
        slots: [
            MaterialSlot {
                label: "Walls",
                default: BlockType::Limestone,
            },
            MaterialSlot {
                label: "Roof",
                default: BlockType::RoofTile,
            },
            MaterialSlot {
                label: "Windows",
                default: BlockType::NeonGlass,
            },
            MaterialSlot {
                label: "Court",
                default: BlockType::ZenStone,
            },
        ],
        primitives: parts,
    }
}

fn skyline_micro_tower() -> ObjectBlueprint {
    let mut parts = vec![
        primitive((0, 0, 0), (56, 4, 56), 0, DetailTier::Full),
        primitive((6, 4, 6), (44, 72, 44), 0, DetailTier::Full),
        primitive((10, 76, 10), (36, 4, 36), 1, DetailTier::Half),
        primitive((16, 80, 16), (24, 8, 24), 1, DetailTier::Half),
        primitive((25, 88, 25), (6, 12, 6), 1, DetailTier::Quarter),
        primitive((8, 8, 4), (40, 64, 2), 2, DetailTier::Half),
        primitive((8, 8, 50), (40, 64, 2), 2, DetailTier::Half),
        primitive((4, 8, 8), (2, 64, 40), 2, DetailTier::Half),
        primitive((50, 8, 8), (2, 64, 40), 2, DetailTier::Half),
    ];
    for floor in (12..72).step_by(8) {
        parts.push(primitive((6, floor, 4), (44, 1, 2), 1, DetailTier::Quarter));
        parts.push(primitive(
            (6, floor, 50),
            (44, 1, 2),
            1,
            DetailTier::Quarter,
        ));
        parts.push(primitive((4, floor, 6), (2, 1, 44), 1, DetailTier::Quarter));
        parts.push(primitive(
            (50, floor, 6),
            (2, 1, 44),
            1,
            DetailTier::Quarter,
        ));
    }
    for x in [14, 24, 34, 44] {
        parts.push(primitive((x, 8, 3), (1, 64, 2), 1, DetailTier::Quarter));
        parts.push(primitive((x, 8, 51), (1, 64, 2), 1, DetailTier::Quarter));
    }
    ObjectBlueprint {
        id: "infra.skyline-micro-tower",
        label: "Skyline Micro Tower",
        version: "v1.0.0",
        description: "Compact mixed-use tower with readable floor rhythm.",
        dimensions: UVec3::new(56, 100, 56),
        category: ObjectCategory::Infrastructure,
        default_fit: TerrainFitMode::LevelPad,
        recommended_slope: 0.04,
        slots: [
            MaterialSlot {
                label: "Core",
                default: BlockType::ShipHullDark,
            },
            MaterialSlot {
                label: "Bands",
                default: BlockType::ShipHullAlloy,
            },
            MaterialSlot {
                label: "Facade",
                default: BlockType::CockpitGlass,
            },
            MaterialSlot {
                label: "Interior",
                default: BlockType::ShojiLamp,
            },
        ],
        primitives: parts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_ids_are_unique_and_versioned() {
        let mut ids = HashSet::new();
        for asset in object_catalog() {
            assert!(ids.insert(asset.id), "duplicate asset id {}", asset.id);
            assert!(asset.version.starts_with('v'));
            assert!(asset.version.split('.').count() == 3);
        }
    }

    #[test]
    fn every_primitive_is_valid_and_inside_declared_bounds() {
        for asset in object_catalog() {
            for part in &asset.primitives {
                assert!(part.size.x > 0 && part.size.y > 0 && part.size.z > 0);
                assert!(part.material_slot < asset.slots.len());
                assert!(part.min.x >= 0 && part.min.y >= 0 && part.min.z >= 0);
                let max = part.min.as_uvec3() + part.size;
                assert!(
                    max.cmple(asset.dimensions).all(),
                    "{} part {:?} exceeds {:?}",
                    asset.id,
                    max,
                    asset.dimensions
                );
            }
        }
    }

    #[test]
    fn detail_tiers_reveal_monotonically_more_geometry() {
        for asset in object_catalog() {
            let full = visible_primitives(asset, DetailTier::Full).count();
            let half = visible_primitives(asset, DetailTier::Half).count();
            let quarter = visible_primitives(asset, DetailTier::Quarter).count();
            assert!(full <= half);
            assert!(half <= quarter);
            assert!(full > 0);
        }
    }

    #[test]
    fn terrain_fit_height_is_deterministic_and_centered() {
        assert_eq!(terrain_height(12.0, 0.0, 12.0, 0.3), 0.0);
        assert!(terrain_height(13.0, 0.0, 12.0, 0.3) > 0.0);
        assert!(terrain_height(11.0, 0.0, 12.0, 0.3) < 0.0);
    }

    #[test]
    fn logical_plan_mutation_invalidates_review_receipt() {
        let mut state = ObjectLabState::default();
        let blueprint = &object_catalog()[state.selected];
        let reviewed = object_lab_evaluation(&state, blueprint);
        let receipt = issue_preview_receipt(&reviewed).unwrap();

        state.scale = 1.25;
        mark_plan_changed(&mut state, "Scale changed");
        let changed = object_lab_evaluation(&state, blueprint);

        assert!(!receipt.matches(&changed));
        assert!(state.review_receipt.is_none());
        assert_ne!(reviewed.revision, changed.revision);
        assert_ne!(reviewed.content_fingerprint, changed.content_fingerprint);
    }

    #[test]
    fn camera_and_grid_changes_keep_exact_review_valid() {
        let mut state = ObjectLabState::default();
        let blueprint = &object_catalog()[state.selected];
        let reviewed = object_lab_evaluation(&state, blueprint);
        let receipt = issue_preview_receipt(&reviewed).unwrap();

        state.yaw += 0.8;
        state.pitch -= 0.2;
        state.zoom = 2.0;
        state.show_grid = !state.show_grid;

        assert!(receipt.matches(&object_lab_evaluation(&state, blueprint)));
    }

    #[test]
    fn unsupported_slope_and_over_budget_plan_cannot_be_reviewed() {
        let mut state = ObjectLabState::default();
        let blueprint = &object_catalog()[state.selected];
        state.fit = TerrainFitMode::LevelPad;
        state.terrain_slope = 0.45;
        let unsupported = object_lab_evaluation(&state, blueprint);
        assert!(!unsupported.is_admissible());
        assert!(unsupported
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "terrain.slope_unsupported"));
        assert!(issue_preview_receipt(&unsupported).is_err());

        state.fit = TerrainFitMode::Stilts;
        state.scale = 2.0;
        let over_budget = object_lab_evaluation(&state, blueprint);
        assert!(!over_budget.is_admissible());
        assert!(over_budget
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.starts_with("budget.")));
        assert!(issue_preview_receipt(&over_budget).is_err());
    }
}
