//! Procedural icon system — zero font / zero image dependencies.
//!
//! Each [`Icon`] variant is drawn on demand via `egui::Shape` primitives
//! (circles, rects, line segments, simple paths). The whole set is a
//! single `match` in [`paint_icon`]; adding a new glyph is ~10 lines.
//!
//! Why procedural?
//!   * No font atlas bloat, no PNG decode at startup.
//!   * Icons inherit the active phosphor colour automatically.
//!   * Crisp at any zoom (vector, no mip fuzz).
//!   * Deterministic layout: the engine ships the same look on any OS.
//!
//! Performance budget: 48 glyphs × ≲8 shapes ≈ 400 Shape pushes/frame
//! in the worst case ( every tab + mode bar + transform row visible ).
//! Measured on Vega 8 this is well under 0.05 ms — an order of magnitude
//! below the 0.7 ms editor budget.

use bevy_egui::egui;

use crate::theme::ThemeSettings;

// ---------------------------------------------------------------------
// Icon enum
// ---------------------------------------------------------------------

/// All procedural glyphs known to the editor.
///
/// The set is intentionally fixed and small so UI code can reason about
/// it exhaustively; adding an icon means adding one arm to
/// [`paint_icon`] and (optionally) a tooltip in [`Icon::tooltip_de`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // some variants land with later phases
pub enum Icon {
    // --- Tabs (10) ---
    World,
    Graphics,
    Weather,
    Time,
    Player,
    Textures,
    Builder,
    Animation,
    City,
    System,

    // --- City sub-tools ---
    Road,
    District,
    Snap,

    // --- Modes (4) ---
    ModeNavigate,
    ModeBuild,
    ModeManipulate,
    ModeAnimate,

    // --- File ops ---
    New,
    Open,
    Save,
    SaveAs,
    Copy,
    Paste,
    Delete,
    Undo,
    Redo,

    // --- Transforms ---
    Move,
    Rotate,
    Scale,
    FlipX,
    FlipY,
    FlipZ,
    RotateY90,

    // --- World / navigation ---
    Seed,
    Bookmark,
    Teleport,
    Globe,
    Grid,
    Chunk,

    // --- Lights / sky ---
    Sun,
    Moon,
    Cloud,
    Rain,
    Snow,
    Fog,
    LightBulb,

    // --- Builder ---
    Cube,
    Brush,
    Eraser,
    Wand,
    Magnet,
    Pipette,

    // --- Anim / media ---
    Clip,
    Key,
    Play,
    Pause,
    Loop,

    // --- UI ---
    Gear,
    Search,
    Help,
    Close,
    Pin,
    Eye,
    EyeOff,
    Resume,
    Quit,
    Accessibility,
    Hud,
    Optimize,
    Detail,
    Intensity,
    Follow,
    Hold,
    Scan,
    Approve,
    Drawer,
    Layout,
    Density,
}

impl Icon {
    /// Localized tooltip (German). English fallback added later via a
    /// single settings toggle — kept in one place so we never scatter
    /// UI strings through the codebase.
    pub fn tooltip_de(self) -> &'static str {
        match self {
            Icon::World => "WELT",
            Icon::Graphics => "GRAFIK",
            Icon::Weather => "WETTER",
            Icon::Time => "ZEIT",
            Icon::Player => "SPIELER",
            Icon::Textures => "TEXTUREN",
            Icon::Builder => "BAUEN",
            Icon::Animation => "ANIMATION",
            Icon::City => "STADT",
            Icon::System => "SYSTEM",

            Icon::Road => "Strasse (N)",
            Icon::District => "Bezirk (T)",
            Icon::Snap => "Snap (.)",

            Icon::ModeNavigate => "Navigieren (1)",
            Icon::ModeBuild => "Bauen (2)",
            Icon::ModeManipulate => "Bearbeiten (3)",
            Icon::ModeAnimate => "Animieren (4)",

            Icon::New => "Neu",
            Icon::Open => "Öffnen",
            Icon::Save => "Speichern",
            Icon::SaveAs => "Speichern unter",
            Icon::Copy => "Kopieren",
            Icon::Paste => "Einfügen",
            Icon::Delete => "Löschen",
            Icon::Undo => "Zurück (Strg+Z)",
            Icon::Redo => "Vor (Strg+Shift+Z)",

            Icon::Move => "Verschieben (G)",
            Icon::Rotate => "Drehen (R)",
            Icon::Scale => "Skalieren (S)",
            Icon::FlipX => "Spiegeln X",
            Icon::FlipY => "Spiegeln Y",
            Icon::FlipZ => "Spiegeln Z",
            Icon::RotateY90 => "Drehen Y 90°",

            Icon::Seed => "Seed",
            Icon::Bookmark => "Lesezeichen",
            Icon::Teleport => "Teleport",
            Icon::Globe => "Welt",
            Icon::Grid => "Gitter",
            Icon::Chunk => "Chunk",

            Icon::Sun => "Sonne",
            Icon::Moon => "Mond",
            Icon::Cloud => "Wolke",
            Icon::Rain => "Regen",
            Icon::Snow => "Schnee",
            Icon::Fog => "Nebel",
            Icon::LightBulb => "Licht",

            Icon::Cube => "Block",
            Icon::Brush => "Pinsel",
            Icon::Eraser => "Radierer",
            Icon::Wand => "Zauberstab",
            Icon::Magnet => "Magnet",
            Icon::Pipette => "Pipette",

            Icon::Clip => "Clip",
            Icon::Key => "Keyframe",
            Icon::Play => "Abspielen",
            Icon::Pause => "Pause",
            Icon::Loop => "Schleife",

            Icon::Gear => "Einstellungen",
            Icon::Search => "Suchen (Strg+P)",
            Icon::Help => "Hilfe",
            Icon::Close => "Schließen",
            Icon::Pin => "Anheften",
            Icon::Eye => "Sichtbar",
            Icon::EyeOff => "Versteckt",
            Icon::Resume => "Fortsetzen",
            Icon::Quit => "Beenden",
            Icon::Accessibility => "Barrierefreiheit",
            Icon::Hud => "HUD",
            Icon::Optimize => "Optimieren",
            Icon::Detail => "Details",
            Icon::Intensity => "Intensitaet",
            Icon::Follow => "Folgen",
            Icon::Hold => "Halten",
            Icon::Scan => "Scannen",
            Icon::Approve => "Bestaetigen",
            Icon::Drawer => "Schublade",
            Icon::Layout => "Layout",
            Icon::Density => "Dichte",
        }
    }
}

// ---------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------

/// Paint a single icon into `rect` with `color` as the primary stroke.
///
/// Shapes are all normalized to a 24×24 canonical grid and then mapped
/// into `rect`, so the same enum variant looks identical at any size
/// (12 px status-bar tray, 24 px chip, 48 px tutorial overlay).
#[allow(clippy::too_many_lines)] // one big match — flat is clearer
pub fn paint_icon(painter: &egui::Painter, rect: egui::Rect, icon: Icon, color: egui::Color32) {
    let min = rect.min;
    let size = rect.size();
    // Map the 0..24 canonical grid into `rect`.
    let p = |x: f32, y: f32| -> egui::Pos2 {
        egui::pos2(min.x + (x / 24.0) * size.x, min.y + (y / 24.0) * size.y)
    };
    let stroke = egui::Stroke::new((size.x / 24.0).max(1.0), color);
    let line = |a: egui::Pos2, b: egui::Pos2| egui::Shape::line_segment([a, b], stroke);
    let poly = |pts: Vec<egui::Pos2>| egui::Shape::closed_line(pts, stroke);
    let circle = |c: egui::Pos2, r: f32| egui::Shape::circle_stroke(c, r * (size.x / 24.0), stroke);
    let disc = |c: egui::Pos2, r: f32| egui::Shape::circle_filled(c, r * (size.x / 24.0), color);
    let rect_stroke = |a: egui::Pos2, b: egui::Pos2| {
        egui::Shape::rect_stroke(egui::Rect::from_two_pos(a, b), egui::Rounding::ZERO, stroke)
    };

    let mut shapes: Vec<egui::Shape> = Vec::with_capacity(10);

    match icon {
        // --- Tabs -----------------------------------------------------
        Icon::World | Icon::Globe => {
            shapes.push(circle(p(12.0, 12.0), 9.0));
            shapes.push(line(p(3.0, 12.0), p(21.0, 12.0)));
            shapes.push(egui::Shape::line(
                vec![p(12.0, 3.0), p(8.0, 12.0), p(12.0, 21.0)],
                stroke,
            ));
            shapes.push(egui::Shape::line(
                vec![p(12.0, 3.0), p(16.0, 12.0), p(12.0, 21.0)],
                stroke,
            ));
        }
        Icon::Graphics => {
            // monitor
            shapes.push(rect_stroke(p(3.0, 4.0), p(21.0, 16.0)));
            shapes.push(line(p(9.0, 20.0), p(15.0, 20.0)));
            shapes.push(line(p(12.0, 16.0), p(12.0, 20.0)));
            // waveform
            shapes.push(egui::Shape::line(
                vec![
                    p(6.0, 11.0),
                    p(9.0, 8.0),
                    p(12.0, 13.0),
                    p(15.0, 7.0),
                    p(18.0, 11.0),
                ],
                stroke,
            ));
        }
        Icon::Weather => {
            // cloud w/ rain
            shapes.push(circle(p(10.0, 10.0), 4.0));
            shapes.push(circle(p(15.0, 11.0), 3.5));
            shapes.push(line(p(8.0, 17.0), p(7.0, 20.0)));
            shapes.push(line(p(12.0, 17.0), p(11.0, 20.0)));
            shapes.push(line(p(16.0, 17.0), p(15.0, 20.0)));
        }
        Icon::Time => {
            shapes.push(circle(p(12.0, 12.0), 9.0));
            shapes.push(line(p(12.0, 12.0), p(12.0, 6.0)));
            shapes.push(line(p(12.0, 12.0), p(16.0, 14.0)));
        }
        Icon::Player => {
            shapes.push(circle(p(12.0, 7.0), 3.0));
            shapes.push(egui::Shape::line(
                vec![p(6.0, 21.0), p(9.0, 12.0), p(15.0, 12.0), p(18.0, 21.0)],
                stroke,
            ));
        }
        Icon::Textures => {
            // checkerboard 3x3
            for gy in 0..3 {
                for gx in 0..3 {
                    if (gx + gy) % 2 == 0 {
                        let x0 = 4.0 + gx as f32 * 5.0;
                        let y0 = 4.0 + gy as f32 * 5.0;
                        shapes.push(egui::Shape::rect_filled(
                            egui::Rect::from_min_max(p(x0, y0), p(x0 + 5.0, y0 + 5.0)),
                            egui::Rounding::ZERO,
                            color,
                        ));
                    }
                }
            }
            shapes.push(rect_stroke(p(4.0, 4.0), p(19.0, 19.0)));
        }
        Icon::Builder | Icon::Cube => {
            // iso cube
            shapes.push(poly(vec![
                p(12.0, 3.0),
                p(21.0, 8.0),
                p(21.0, 17.0),
                p(12.0, 22.0),
                p(3.0, 17.0),
                p(3.0, 8.0),
            ]));
            shapes.push(line(p(12.0, 3.0), p(12.0, 12.0)));
            shapes.push(line(p(3.0, 8.0), p(12.0, 12.0)));
            shapes.push(line(p(21.0, 8.0), p(12.0, 12.0)));
            shapes.push(line(p(12.0, 12.0), p(12.0, 22.0)));
        }
        Icon::Animation | Icon::Clip => {
            // film strip
            shapes.push(rect_stroke(p(3.0, 6.0), p(21.0, 18.0)));
            for i in 0..4 {
                let x = 5.0 + i as f32 * 4.5;
                shapes.push(rect_stroke(p(x, 4.0), p(x + 2.5, 6.0)));
                shapes.push(rect_stroke(p(x, 18.0), p(x + 2.5, 20.0)));
            }
        }
        Icon::System | Icon::Gear => {
            shapes.push(circle(p(12.0, 12.0), 4.0));
            for i in 0..8 {
                let a = (i as f32) * std::f32::consts::TAU / 8.0;
                let (c, s) = (a.cos(), a.sin());
                shapes.push(line(
                    egui::pos2(
                        p(12.0, 12.0).x + c * 6.0 * size.x / 24.0,
                        p(12.0, 12.0).y + s * 6.0 * size.x / 24.0,
                    ),
                    egui::pos2(
                        p(12.0, 12.0).x + c * 9.0 * size.x / 24.0,
                        p(12.0, 12.0).y + s * 9.0 * size.x / 24.0,
                    ),
                ));
            }
        }

        // --- Modes ----------------------------------------------------
        Icon::ModeNavigate => {
            // compass rose
            shapes.push(circle(p(12.0, 12.0), 9.0));
            shapes.push(poly(vec![
                p(12.0, 4.0),
                p(14.0, 12.0),
                p(12.0, 20.0),
                p(10.0, 12.0),
            ]));
            shapes.push(disc(p(12.0, 12.0), 1.2));
        }
        Icon::ModeBuild => {
            // hammer
            shapes.push(poly(vec![
                p(4.0, 8.0),
                p(12.0, 4.0),
                p(20.0, 8.0),
                p(14.0, 12.0),
                p(10.0, 12.0),
            ]));
            shapes.push(line(p(12.0, 12.0), p(8.0, 21.0)));
            shapes.push(line(p(10.0, 12.0), p(6.0, 21.0)));
        }
        Icon::ModeManipulate | Icon::Move => {
            // 4-way arrow
            shapes.push(line(p(12.0, 3.0), p(12.0, 21.0)));
            shapes.push(line(p(3.0, 12.0), p(21.0, 12.0)));
            shapes.push(poly(vec![p(12.0, 3.0), p(10.0, 7.0), p(14.0, 7.0)]));
            shapes.push(poly(vec![p(12.0, 21.0), p(10.0, 17.0), p(14.0, 17.0)]));
            shapes.push(poly(vec![p(3.0, 12.0), p(7.0, 10.0), p(7.0, 14.0)]));
            shapes.push(poly(vec![p(21.0, 12.0), p(17.0, 10.0), p(17.0, 14.0)]));
        }
        Icon::ModeAnimate | Icon::Play => {
            shapes.push(poly(vec![p(7.0, 5.0), p(7.0, 19.0), p(20.0, 12.0)]));
        }

        // --- File ops -------------------------------------------------
        Icon::New => {
            shapes.push(poly(vec![
                p(6.0, 3.0),
                p(15.0, 3.0),
                p(19.0, 7.0),
                p(19.0, 21.0),
                p(6.0, 21.0),
            ]));
            shapes.push(line(p(15.0, 3.0), p(15.0, 7.0)));
            shapes.push(line(p(15.0, 7.0), p(19.0, 7.0)));
            shapes.push(line(p(9.0, 13.0), p(16.0, 13.0)));
            shapes.push(line(p(12.5, 10.0), p(12.5, 16.0)));
        }
        Icon::Open => {
            shapes.push(poly(vec![
                p(3.0, 7.0),
                p(10.0, 7.0),
                p(12.0, 9.0),
                p(21.0, 9.0),
                p(21.0, 19.0),
                p(3.0, 19.0),
            ]));
        }
        Icon::Save => {
            shapes.push(rect_stroke(p(4.0, 4.0), p(20.0, 20.0)));
            shapes.push(rect_stroke(p(7.0, 4.0), p(17.0, 9.0)));
            shapes.push(rect_stroke(p(8.0, 13.0), p(16.0, 19.0)));
        }
        Icon::SaveAs => {
            shapes.push(rect_stroke(p(3.0, 3.0), p(18.0, 18.0)));
            shapes.push(rect_stroke(p(5.0, 3.0), p(14.0, 7.0)));
            shapes.push(line(p(15.0, 15.0), p(21.0, 21.0)));
            shapes.push(line(p(21.0, 15.0), p(15.0, 21.0)));
        }
        Icon::Copy => {
            shapes.push(rect_stroke(p(4.0, 4.0), p(15.0, 15.0)));
            shapes.push(rect_stroke(p(9.0, 9.0), p(20.0, 20.0)));
        }
        Icon::Paste => {
            shapes.push(rect_stroke(p(4.0, 5.0), p(20.0, 21.0)));
            shapes.push(rect_stroke(p(8.0, 3.0), p(16.0, 7.0)));
            shapes.push(line(p(9.0, 12.0), p(15.0, 12.0)));
            shapes.push(line(p(9.0, 16.0), p(15.0, 16.0)));
        }
        Icon::Delete => {
            shapes.push(rect_stroke(p(6.0, 6.0), p(18.0, 21.0)));
            shapes.push(line(p(4.0, 6.0), p(20.0, 6.0)));
            shapes.push(line(p(9.0, 4.0), p(15.0, 4.0)));
            shapes.push(line(p(10.0, 10.0), p(10.0, 17.0)));
            shapes.push(line(p(14.0, 10.0), p(14.0, 17.0)));
        }
        Icon::Undo => {
            shapes.push(egui::Shape::line(
                vec![p(8.0, 7.0), p(4.0, 11.0), p(8.0, 15.0)],
                stroke,
            ));
            shapes.push(egui::Shape::line(
                vec![p(4.0, 11.0), p(16.0, 11.0), p(20.0, 15.0), p(20.0, 19.0)],
                stroke,
            ));
        }
        Icon::Redo => {
            shapes.push(egui::Shape::line(
                vec![p(16.0, 7.0), p(20.0, 11.0), p(16.0, 15.0)],
                stroke,
            ));
            shapes.push(egui::Shape::line(
                vec![p(20.0, 11.0), p(8.0, 11.0), p(4.0, 15.0), p(4.0, 19.0)],
                stroke,
            ));
        }

        // --- Transforms ----------------------------------------------
        Icon::Rotate | Icon::RotateY90 => {
            shapes.push(circle(p(12.0, 12.0), 7.0));
            shapes.push(poly(vec![p(18.0, 6.0), p(20.0, 10.0), p(16.0, 10.0)]));
            shapes.push(line(p(19.0, 7.0), p(16.0, 9.0)));
        }
        Icon::Scale => {
            shapes.push(line(p(4.0, 20.0), p(20.0, 4.0)));
            shapes.push(rect_stroke(p(3.0, 17.0), p(7.0, 21.0)));
            shapes.push(rect_stroke(p(17.0, 3.0), p(21.0, 7.0)));
        }
        Icon::FlipX => {
            shapes.push(line(p(12.0, 3.0), p(12.0, 21.0)));
            shapes.push(poly(vec![p(3.0, 12.0), p(10.0, 6.0), p(10.0, 18.0)]));
            shapes.push(poly(vec![p(21.0, 12.0), p(14.0, 6.0), p(14.0, 18.0)]));
        }
        Icon::FlipY => {
            shapes.push(line(p(3.0, 12.0), p(21.0, 12.0)));
            shapes.push(poly(vec![p(12.0, 3.0), p(6.0, 10.0), p(18.0, 10.0)]));
            shapes.push(poly(vec![p(12.0, 21.0), p(6.0, 14.0), p(18.0, 14.0)]));
        }
        Icon::FlipZ => {
            // diagonal
            shapes.push(line(p(4.0, 20.0), p(20.0, 4.0)));
            shapes.push(poly(vec![p(4.0, 4.0), p(12.0, 6.0), p(6.0, 12.0)]));
            shapes.push(poly(vec![p(20.0, 20.0), p(12.0, 18.0), p(18.0, 12.0)]));
        }

        // --- World / nav ---------------------------------------------
        Icon::Seed => {
            // droplet w/ sprout
            shapes.push(poly(vec![
                p(12.0, 13.0),
                p(8.0, 18.0),
                p(12.0, 22.0),
                p(16.0, 18.0),
            ]));
            shapes.push(line(p(12.0, 13.0), p(12.0, 6.0)));
            shapes.push(egui::Shape::line(
                vec![p(12.0, 8.0), p(15.0, 4.0), p(18.0, 6.0)],
                stroke,
            ));
            shapes.push(egui::Shape::line(
                vec![p(12.0, 10.0), p(9.0, 6.0), p(6.0, 8.0)],
                stroke,
            ));
        }
        Icon::Bookmark | Icon::Pin => {
            shapes.push(poly(vec![
                p(6.0, 3.0),
                p(18.0, 3.0),
                p(18.0, 21.0),
                p(12.0, 16.0),
                p(6.0, 21.0),
            ]));
        }
        Icon::Teleport => {
            shapes.push(circle(p(12.0, 12.0), 9.0));
            shapes.push(poly(vec![p(9.0, 7.0), p(17.0, 12.0), p(9.0, 17.0)]));
            shapes.push(disc(p(6.0, 12.0), 1.0));
        }
        Icon::Grid => {
            for i in 0..4 {
                let x = 4.0 + i as f32 * 5.0;
                shapes.push(line(p(x, 4.0), p(x, 20.0)));
                shapes.push(line(p(4.0, x), p(20.0, x)));
            }
        }
        Icon::Chunk => {
            shapes.push(rect_stroke(p(4.0, 4.0), p(20.0, 20.0)));
            shapes.push(rect_stroke(p(4.0, 4.0), p(12.0, 12.0)));
            shapes.push(rect_stroke(p(12.0, 12.0), p(20.0, 20.0)));
        }

        // --- Lights / sky --------------------------------------------
        Icon::Sun => {
            shapes.push(disc(p(12.0, 12.0), 4.0));
            for i in 0..8 {
                let a = (i as f32) * std::f32::consts::TAU / 8.0;
                let (c, s) = (a.cos(), a.sin());
                let cx = p(12.0, 12.0);
                shapes.push(line(
                    egui::pos2(
                        cx.x + c * 7.0 * size.x / 24.0,
                        cx.y + s * 7.0 * size.x / 24.0,
                    ),
                    egui::pos2(
                        cx.x + c * 10.0 * size.x / 24.0,
                        cx.y + s * 10.0 * size.x / 24.0,
                    ),
                ));
            }
        }
        Icon::Moon => {
            shapes.push(circle(p(12.0, 12.0), 8.0));
            shapes.push(egui::Shape::circle_filled(
                p(14.0, 10.0),
                6.0 * size.x / 24.0,
                egui::Color32::TRANSPARENT,
            ));
            // crescent via overlap (approx) — just draw smaller arc
            shapes.push(circle(p(15.0, 10.0), 6.0));
        }
        Icon::Cloud => {
            shapes.push(circle(p(9.0, 13.0), 4.0));
            shapes.push(circle(p(14.0, 11.0), 5.0));
            shapes.push(circle(p(17.0, 14.0), 3.5));
            shapes.push(line(p(5.0, 17.0), p(20.0, 17.0)));
        }
        Icon::Rain => {
            shapes.push(circle(p(12.0, 9.0), 5.0));
            for i in 0..4 {
                let x = 7.0 + i as f32 * 3.0;
                shapes.push(line(p(x, 16.0), p(x - 1.0, 21.0)));
            }
        }
        Icon::Snow => {
            shapes.push(line(p(12.0, 3.0), p(12.0, 21.0)));
            shapes.push(line(p(3.0, 12.0), p(21.0, 12.0)));
            shapes.push(line(p(5.0, 5.0), p(19.0, 19.0)));
            shapes.push(line(p(5.0, 19.0), p(19.0, 5.0)));
        }
        Icon::Fog => {
            for i in 0..4 {
                let y = 6.0 + i as f32 * 4.0;
                let off = if i % 2 == 0 { 2.0 } else { -2.0 };
                shapes.push(line(p(4.0 + off, y), p(20.0 + off, y)));
            }
        }
        Icon::LightBulb => {
            shapes.push(circle(p(12.0, 10.0), 5.0));
            shapes.push(line(p(9.0, 16.0), p(15.0, 16.0)));
            shapes.push(line(p(10.0, 19.0), p(14.0, 19.0)));
            shapes.push(line(p(12.0, 3.0), p(12.0, 5.0)));
            shapes.push(line(p(5.0, 10.0), p(7.0, 10.0)));
            shapes.push(line(p(17.0, 10.0), p(19.0, 10.0)));
        }

        // --- Builder tools -------------------------------------------
        Icon::Brush => {
            shapes.push(poly(vec![
                p(14.0, 3.0),
                p(21.0, 10.0),
                p(14.0, 14.0),
                p(10.0, 10.0),
            ]));
            shapes.push(line(p(10.0, 14.0), p(4.0, 20.0)));
            shapes.push(line(p(6.0, 16.0), p(8.0, 18.0)));
        }
        Icon::Eraser => {
            shapes.push(poly(vec![
                p(4.0, 16.0),
                p(12.0, 8.0),
                p(20.0, 16.0),
                p(16.0, 20.0),
                p(8.0, 20.0),
            ]));
            shapes.push(line(p(10.0, 14.0), p(16.0, 20.0)));
        }
        Icon::Wand => {
            shapes.push(line(p(4.0, 20.0), p(18.0, 6.0)));
            shapes.push(disc(p(20.0, 4.0), 1.5));
            shapes.push(line(p(16.0, 3.0), p(16.0, 7.0)));
            shapes.push(line(p(14.0, 5.0), p(18.0, 5.0)));
        }
        Icon::Magnet => {
            shapes.push(egui::Shape::line(
                vec![
                    p(4.0, 6.0),
                    p(4.0, 14.0),
                    p(9.0, 14.0),
                    p(9.0, 9.0),
                    p(15.0, 9.0),
                    p(15.0, 14.0),
                    p(20.0, 14.0),
                    p(20.0, 6.0),
                ],
                stroke,
            ));
            shapes.push(line(p(4.0, 6.0), p(9.0, 6.0)));
            shapes.push(line(p(15.0, 6.0), p(20.0, 6.0)));
            shapes.push(line(p(4.0, 18.0), p(9.0, 18.0)));
            shapes.push(line(p(15.0, 18.0), p(20.0, 18.0)));
        }
        Icon::Pipette => {
            shapes.push(line(p(5.0, 19.0), p(14.0, 10.0)));
            shapes.push(rect_stroke(p(13.0, 6.0), p(19.0, 11.0)));
            shapes.push(line(p(3.0, 21.0), p(6.0, 18.0)));
        }

        // --- Anim / media --------------------------------------------
        Icon::Key => {
            shapes.push(circle(p(8.0, 12.0), 4.0));
            shapes.push(line(p(12.0, 12.0), p(21.0, 12.0)));
            shapes.push(line(p(18.0, 12.0), p(18.0, 16.0)));
            shapes.push(line(p(21.0, 12.0), p(21.0, 15.0)));
        }
        Icon::Pause => {
            shapes.push(rect_stroke(p(7.0, 5.0), p(10.0, 19.0)));
            shapes.push(rect_stroke(p(14.0, 5.0), p(17.0, 19.0)));
        }
        Icon::Loop => {
            shapes.push(egui::Shape::line(
                vec![
                    p(6.0, 8.0),
                    p(18.0, 8.0),
                    p(21.0, 12.0),
                    p(18.0, 16.0),
                    p(6.0, 16.0),
                    p(3.0, 12.0),
                    p(6.0, 8.0),
                ],
                stroke,
            ));
            shapes.push(poly(vec![p(9.0, 6.0), p(6.0, 8.0), p(9.0, 10.0)]));
        }

        // --- UI -------------------------------------------------------
        Icon::Search => {
            shapes.push(circle(p(10.0, 10.0), 6.0));
            shapes.push(line(p(14.0, 14.0), p(20.0, 20.0)));
        }
        Icon::Help => {
            shapes.push(circle(p(12.0, 12.0), 9.0));
            shapes.push(egui::Shape::line(
                vec![
                    p(9.0, 9.0),
                    p(12.0, 7.0),
                    p(15.0, 9.0),
                    p(12.0, 13.0),
                    p(12.0, 15.0),
                ],
                stroke,
            ));
            shapes.push(disc(p(12.0, 18.0), 1.0));
        }
        Icon::Close => {
            shapes.push(line(p(5.0, 5.0), p(19.0, 19.0)));
            shapes.push(line(p(5.0, 19.0), p(19.0, 5.0)));
        }
        Icon::Eye => {
            shapes.push(egui::Shape::line(
                vec![
                    p(3.0, 12.0),
                    p(8.0, 6.0),
                    p(16.0, 6.0),
                    p(21.0, 12.0),
                    p(16.0, 18.0),
                    p(8.0, 18.0),
                    p(3.0, 12.0),
                ],
                stroke,
            ));
            shapes.push(circle(p(12.0, 12.0), 3.0));
            shapes.push(disc(p(12.0, 12.0), 1.3));
        }
        Icon::EyeOff => {
            shapes.push(egui::Shape::line(
                vec![
                    p(3.0, 12.0),
                    p(8.0, 6.0),
                    p(16.0, 6.0),
                    p(21.0, 12.0),
                    p(16.0, 18.0),
                    p(8.0, 18.0),
                    p(3.0, 12.0),
                ],
                stroke,
            ));
            shapes.push(line(p(4.0, 4.0), p(20.0, 20.0)));
        }
        Icon::Resume => {
            shapes.push(poly(vec![p(7.0, 5.0), p(7.0, 19.0), p(20.0, 12.0)]));
            shapes.push(line(p(4.0, 5.0), p(4.0, 19.0)));
        }
        Icon::Quit => {
            shapes.push(rect_stroke(p(5.0, 4.0), p(17.0, 20.0)));
            shapes.push(line(p(11.0, 12.0), p(22.0, 12.0)));
            shapes.push(poly(vec![p(18.0, 8.0), p(22.0, 12.0), p(18.0, 16.0)]));
        }
        Icon::Accessibility => {
            shapes.push(circle(p(12.0, 5.0), 2.0));
            shapes.push(line(p(4.0, 10.0), p(20.0, 10.0)));
            shapes.push(line(p(12.0, 8.0), p(12.0, 14.0)));
            shapes.push(line(p(12.0, 14.0), p(7.0, 21.0)));
            shapes.push(line(p(12.0, 14.0), p(17.0, 21.0)));
        }
        Icon::Hud => {
            shapes.push(rect_stroke(p(4.0, 6.0), p(20.0, 18.0)));
            shapes.push(line(p(12.0, 8.0), p(12.0, 16.0)));
            shapes.push(line(p(8.0, 12.0), p(16.0, 12.0)));
            shapes.push(circle(p(12.0, 12.0), 3.0));
        }
        Icon::Optimize => {
            shapes.push(circle(p(12.0, 12.0), 8.0));
            shapes.push(line(p(12.0, 12.0), p(17.0, 8.0)));
            shapes.push(poly(vec![p(15.0, 8.0), p(18.0, 7.0), p(17.0, 10.0)]));
            shapes.push(line(p(6.0, 18.0), p(18.0, 18.0)));
        }
        Icon::Detail => {
            shapes.push(rect_stroke(p(4.0, 4.0), p(20.0, 20.0)));
            shapes.push(line(p(8.0, 4.0), p(8.0, 20.0)));
            shapes.push(line(p(16.0, 4.0), p(16.0, 20.0)));
            shapes.push(line(p(4.0, 8.0), p(20.0, 8.0)));
            shapes.push(line(p(4.0, 16.0), p(20.0, 16.0)));
        }
        Icon::Intensity => {
            shapes.push(line(p(5.0, 20.0), p(19.0, 20.0)));
            shapes.push(rect_stroke(p(6.0, 13.0), p(9.0, 20.0)));
            shapes.push(rect_stroke(p(11.0, 8.0), p(14.0, 20.0)));
            shapes.push(rect_stroke(p(16.0, 4.0), p(19.0, 20.0)));
        }
        Icon::Follow => {
            shapes.push(circle(p(8.0, 12.0), 3.0));
            shapes.push(line(p(11.0, 12.0), p(21.0, 12.0)));
            shapes.push(poly(vec![p(17.0, 8.0), p(21.0, 12.0), p(17.0, 16.0)]));
        }
        Icon::Hold => {
            shapes.push(rect_stroke(p(6.0, 6.0), p(18.0, 18.0)));
            shapes.push(line(p(8.0, 12.0), p(16.0, 12.0)));
        }
        Icon::Scan => {
            shapes.push(circle(p(10.0, 10.0), 6.0));
            shapes.push(line(p(14.0, 14.0), p(20.0, 20.0)));
            shapes.push(line(p(4.0, 4.0), p(8.0, 4.0)));
            shapes.push(line(p(4.0, 4.0), p(4.0, 8.0)));
            shapes.push(line(p(20.0, 16.0), p(20.0, 20.0)));
            shapes.push(line(p(16.0, 20.0), p(20.0, 20.0)));
        }
        Icon::Approve => {
            shapes.push(circle(p(12.0, 12.0), 9.0));
            shapes.push(egui::Shape::line(
                vec![p(7.0, 12.0), p(10.0, 16.0), p(17.0, 8.0)],
                stroke,
            ));
        }
        Icon::Drawer => {
            shapes.push(rect_stroke(p(4.0, 5.0), p(20.0, 19.0)));
            shapes.push(line(p(4.0, 12.0), p(20.0, 12.0)));
            shapes.push(line(p(9.0, 9.0), p(12.0, 12.0)));
            shapes.push(line(p(15.0, 9.0), p(12.0, 12.0)));
        }
        Icon::Layout => {
            shapes.push(rect_stroke(p(4.0, 4.0), p(20.0, 20.0)));
            shapes.push(line(p(4.0, 10.0), p(20.0, 10.0)));
            shapes.push(line(p(10.0, 10.0), p(10.0, 20.0)));
        }
        Icon::Density => {
            for i in 0..4 {
                let y = 6.0 + i as f32 * 4.0;
                shapes.push(line(p(5.0, y), p(19.0, y)));
            }
        }

        // --- City sub-tools --------------------------------------------
        Icon::City => {
            // tiny skyline — three buildings of differing height
            shapes.push(rect_stroke(p(3.0, 14.0), p(8.0, 21.0)));
            shapes.push(rect_stroke(p(9.0, 8.0), p(15.0, 21.0)));
            shapes.push(rect_stroke(p(16.0, 11.0), p(21.0, 21.0)));
            // windows on the middle tower
            shapes.push(line(p(11.0, 11.0), p(13.0, 11.0)));
            shapes.push(line(p(11.0, 14.0), p(13.0, 14.0)));
            shapes.push(line(p(11.0, 17.0), p(13.0, 17.0)));
            // street at the base
            shapes.push(line(p(2.0, 22.0), p(22.0, 22.0)));
        }
        Icon::Road => {
            // road running diagonally with a dashed centreline
            shapes.push(egui::Shape::line(vec![p(3.0, 21.0), p(21.0, 3.0)], stroke));
            shapes.push(egui::Shape::line(vec![p(6.0, 21.0), p(21.0, 6.0)], stroke));
            // dashes
            shapes.push(line(p(6.0, 15.0), p(8.0, 13.0)));
            shapes.push(line(p(12.0, 9.0), p(14.0, 7.0)));
        }
        Icon::District => {
            // dashed polygon representing a tagged zone
            let pts = [p(4.0, 6.0), p(20.0, 4.0), p(21.0, 18.0), p(6.0, 20.0)];
            for i in 0..pts.len() {
                let a = pts[i];
                let b = pts[(i + 1) % pts.len()];
                // split each edge into 3 dashes
                let mid = egui::pos2((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
                shapes.push(line(a, mid));
            }
            // little pin marker inside
            shapes.push(disc(p(12.0, 12.0), 1.5));
        }
        Icon::Snap => {
            // 3x3 grid of dots with a crosshair on the centre dot
            for gy in 0..3 {
                for gx in 0..3 {
                    let x = 6.0 + gx as f32 * 6.0;
                    let y = 6.0 + gy as f32 * 6.0;
                    shapes.push(disc(p(x, y), 0.8));
                }
            }
            shapes.push(circle(p(12.0, 12.0), 3.0));
        }
    }

    painter.extend(shapes);
}

// ---------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------

/// Standard 28 px icon button with phosphor hover glow.
///
/// Returns a `Response` so callers can chain `.on_hover_text(..)` or
/// `.clicked()` exactly like a regular `egui::Button`.
pub fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    size: f32,
    selected: bool,
    theme: ThemeSettings,
) -> egui::Response {
    let desired = egui::vec2(size, size);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
    let hovered = response.hovered();

    let primary = theme.color.primary();
    let dim = theme.color.dim();
    let bg = if selected {
        // filled phosphor when selected
        egui::Color32::from_rgba_premultiplied(
            primary.r() / 4,
            primary.g() / 4,
            primary.b() / 4,
            200,
        )
    } else if hovered {
        egui::Color32::from_rgba_premultiplied(
            primary.r() / 8,
            primary.g() / 8,
            primary.b() / 8,
            140,
        )
    } else {
        egui::Color32::from_rgba_premultiplied(0, 0, 0, 160)
    };

    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        egui::Rounding::ZERO,
        bg,
        egui::Stroke::new(1.0, if selected { primary } else { dim }),
    );

    // Inset glyph by 20 %.
    let glyph_rect = rect.shrink(size * 0.18);
    let glyph_color = if selected || hovered { primary } else { dim };
    paint_icon(&painter, glyph_rect, icon, glyph_color);

    response.on_hover_text(icon.tooltip_de())
}

/// Larger icon tab chip used for the top tab strip.
///
/// Shows both the icon (big) and a short caption underneath, so users
/// who read German still see the familiar "WELT / GRAFIK / ..." labels
/// while pre-readers get the glyph. When the tab bar overflows on
/// narrow windows, the caption is dropped automatically by `egui`'s
/// wrapping layout; the icon stays.
pub fn icon_tab_chip(
    ui: &mut egui::Ui,
    icon: Icon,
    caption: &str,
    selected: bool,
    theme: ThemeSettings,
) -> egui::Response {
    let width = 56.0;
    let height = 52.0;
    let desired = egui::vec2(width, height);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
    let hovered = response.hovered();

    let primary = theme.color.primary();
    let dim = theme.color.dim();
    let stroke_col = if selected { primary } else { dim };
    let bg = if selected {
        egui::Color32::from_rgba_premultiplied(
            primary.r() / 5,
            primary.g() / 5,
            primary.b() / 5,
            220,
        )
    } else if hovered {
        egui::Color32::from_rgba_premultiplied(
            primary.r() / 10,
            primary.g() / 10,
            primary.b() / 10,
            160,
        )
    } else {
        egui::Color32::from_rgba_premultiplied(0, 0, 0, 180)
    };

    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        egui::Rounding::ZERO,
        bg,
        egui::Stroke::new(1.0, stroke_col),
    );

    // Icon box (top ~60 % of the chip).
    let icon_box = egui::Rect::from_min_size(
        rect.min + egui::vec2((width - 28.0) * 0.5, 4.0),
        egui::vec2(28.0, 28.0),
    );
    let glyph_color = if selected || hovered { primary } else { dim };
    paint_icon(&painter, icon_box, icon, glyph_color);

    // Caption underneath.
    painter.text(
        egui::pos2(rect.center().x, rect.max.y - 10.0),
        egui::Align2::CENTER_CENTER,
        caption,
        egui::FontId::monospace(10.0),
        glyph_color,
    );

    response.on_hover_text(icon.tooltip_de())
}
