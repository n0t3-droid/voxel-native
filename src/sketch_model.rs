//! SketchUp-style semantic editor model.
//!
//! This module is intentionally separate from the voxel storage. The engine
//! stays voxel-native for terrain, runtime edits, booleans, and meshing, while
//! editor tools can now share a semantic document spine: contexts, entities,
//! component definitions/instances, selection, picking hits, inference hints,
//! and transactions.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use bevy::prelude::{App, IVec3, Mat4, Plugin, Quat, Resource, Vec2, Vec3, Vec4};
use serde::{Deserialize, Serialize};

const PLANAR_GRAPH_SCALE: f32 = 1000.0;
const PLANAR_GRAPH_MAX_LOOP_VERTICES: usize = 16;
const PLANAR_GRAPH_MIN_AREA: f32 = 1.0e-4;
const SKETCH_DOCUMENT_SAVE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SketchId(u64);

impl SketchId {
    #[cfg(test)]
    pub const fn new_for_test(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SketchTransform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct PlanarPointKey {
    x: i64,
    y: i64,
    z: i64,
}

impl PlanarPointKey {
    fn from_vec3(point: Vec3) -> Self {
        Self {
            x: (point.x * PLANAR_GRAPH_SCALE).round() as i64,
            y: (point.y * PLANAR_GRAPH_SCALE).round() as i64,
            z: (point.z * PLANAR_GRAPH_SCALE).round() as i64,
        }
    }
}

impl SketchTransform {
    pub fn identity() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }

    pub fn from_translation(translation: Vec3) -> Self {
        Self {
            translation,
            ..Self::identity()
        }
    }
}

impl Default for SketchTransform {
    fn default() -> Self {
        Self::identity()
    }
}

pub type AttributeStore = BTreeMap<String, BTreeMap<String, String>>;

#[derive(Debug, Clone, PartialEq)]
pub enum SketchEntityKind {
    Vertex {
        point: Vec3,
    },
    Edge {
        a: Vec3,
        b: Vec3,
    },
    Face {
        vertices: Vec<Vec3>,
        normal: Vec3,
    },
    CircleFace {
        center: Vec3,
        normal: Vec3,
        radius: f32,
        segments: usize,
        vertices: Vec<Vec3>,
    },
    PolygonFace {
        center: Vec3,
        normal: Vec3,
        radius: f32,
        sides: usize,
        vertices: Vec<Vec3>,
    },
    ArcCurve {
        center: Vec3,
        normal: Vec3,
        radius: f32,
        start_direction: Vec3,
        sweep_radians: f32,
        points: Vec<Vec3>,
    },
    FreehandCurve {
        points: Vec<Vec3>,
    },
    PushPullExtrusion {
        source_face: SketchId,
        base_vertices: Vec<Vec3>,
        top_vertices: Vec<Vec3>,
        normal: Vec3,
        depth: f32,
        bounds: SketchBounds,
    },
    Opening {
        host: SketchId,
        center: Vec3,
        size: Vec3,
        normal: Vec3,
        through_depth: f32,
        bounds: SketchBounds,
    },
    Room {
        shell: SketchId,
        shell_bounds: SketchBounds,
        interior_bounds: SketchBounds,
        wall_thickness: f32,
    },
    Group {
        context: SketchId,
    },
    ComponentInstance {
        definition: SketchId,
        transform: SketchTransform,
    },
    GuidePoint {
        point: Vec3,
    },
    GuideLine {
        origin: Vec3,
        direction: Vec3,
    },
    SectionPlane {
        origin: Vec3,
        normal: Vec3,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SketchEntity {
    pub id: SketchId,
    pub kind: SketchEntityKind,
    pub visible: bool,
    pub locked: bool,
    pub material: Option<SketchId>,
    pub tag: Option<SketchId>,
    pub attributes: AttributeStore,
}

impl SketchEntity {
    fn new(
        id: SketchId,
        kind: SketchEntityKind,
        material: Option<SketchId>,
        tag: Option<SketchId>,
    ) -> Self {
        Self {
            id,
            kind,
            visible: true,
            locked: false,
            material,
            tag,
            attributes: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SketchContext {
    pub id: SketchId,
    pub parent: Option<SketchId>,
    pub local_to_parent: SketchTransform,
    pub entities: Vec<SketchId>,
}

impl SketchContext {
    fn root(id: SketchId) -> Self {
        Self {
            id,
            parent: None,
            local_to_parent: SketchTransform::default(),
            entities: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComponentDefinition {
    pub id: SketchId,
    pub name: String,
    pub context: SketchId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl SketchColor {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchMaterial {
    pub id: SketchId,
    pub name: String,
    pub color: SketchColor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchTag {
    pub id: SketchId,
    pub name: String,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchStyle {
    pub id: SketchId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SketchCamera {
    pub eye: Vec3,
    pub target: Vec3,
    pub up: Vec3,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SketchBounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl SketchBounds {
    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Option<Self> {
        let mut iter = points.into_iter();
        let first = iter.next()?;
        let mut bounds = Self {
            min: first,
            max: first,
        };
        for point in iter {
            bounds.include(point);
        }
        Some(bounds)
    }

    pub fn from_center_size(center: Vec3, size: Vec3) -> Self {
        let half = size.abs() * 0.5;
        Self {
            min: center - half,
            max: center + half,
        }
    }

    pub fn size(self) -> Vec3 {
        self.max - self.min
    }

    fn include(&mut self, point: Vec3) {
        self.min = self.min.min(point);
        self.max = self.max.max(point);
    }

    fn extruded(self, normal: Vec3, depth: f32) -> Self {
        let offset = normal.try_normalize().unwrap_or(Vec3::Z) * depth;
        let mut bounds = self;
        for corner in [
            self.min,
            self.max,
            Vec3::new(self.min.x, self.min.y, self.max.z),
            Vec3::new(self.min.x, self.max.y, self.min.z),
            Vec3::new(self.max.x, self.min.y, self.min.z),
            Vec3::new(self.min.x, self.max.y, self.max.z),
            Vec3::new(self.max.x, self.min.y, self.max.z),
            Vec3::new(self.max.x, self.max.y, self.min.z),
        ] {
            bounds.include(corner + offset);
        }
        bounds
    }

    fn inset(self, amount: f32) -> Self {
        let amount = amount.max(0.0);
        let mut min = self.min;
        let mut max = self.max;
        for axis in 0..3 {
            let size = component_by_index_vec3(max, axis) - component_by_index_vec3(min, axis);
            if size > amount * 2.0 {
                let min_component = component_by_index_vec3(min, axis);
                let max_component = component_by_index_vec3(max, axis);
                set_component_by_index_vec3(&mut min, axis, min_component + amount);
                set_component_by_index_vec3(&mut max, axis, max_component - amount);
            }
        }
        Self { min, max }
    }

    fn translated(self, delta: Vec3) -> Self {
        Self {
            min: self.min + delta,
            max: self.max + delta,
        }
    }

    fn transformed(self, mut transform: impl FnMut(Vec3) -> Vec3) -> Self {
        SketchBounds::from_points(self.corners().into_iter().map(&mut transform)).unwrap_or(self)
    }

    fn corners(self) -> [Vec3; 8] {
        [
            self.min,
            self.max,
            Vec3::new(self.min.x, self.min.y, self.max.z),
            Vec3::new(self.min.x, self.max.y, self.min.z),
            Vec3::new(self.max.x, self.min.y, self.min.z),
            Vec3::new(self.min.x, self.max.y, self.max.z),
            Vec3::new(self.max.x, self.min.y, self.max.z),
            Vec3::new(self.max.x, self.max.y, self.min.z),
        ]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SketchBRepVertex {
    pub id: SketchId,
    pub position: Vec3,
    pub connected_edges: BTreeSet<SketchId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SketchBRepEdge {
    pub id: SketchId,
    pub a: SketchId,
    pub b: SketchId,
    pub faces: BTreeSet<SketchId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SketchBRepLoopEdge {
    pub edge: SketchId,
    pub reversed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SketchBRepFace {
    pub id: SketchId,
    pub outer_loop: Vec<SketchBRepLoopEdge>,
    pub normal: Vec3,
    pub plane_d: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchBRepExtrusionResult {
    pub source_face: SketchId,
    pub top_face: SketchId,
    pub side_faces: Vec<SketchId>,
}

/// Lightweight editable B-Rep graph that sits above voxel rasterization.
///
/// This is intentionally small: it stores stable vertices/edges/faces and the
/// operations the PDF calls out first (coplanar face splitting and Push/Pull
/// extrusion). Runtime tools can voxelize this graph without losing the
/// higher-level edit handles needed for SketchUp-style workflows.
#[derive(Debug, Clone)]
pub struct SketchBRepKernel {
    next_id: u64,
    vertices: BTreeMap<SketchId, SketchBRepVertex>,
    edges: BTreeMap<SketchId, SketchBRepEdge>,
    faces: BTreeMap<SketchId, SketchBRepFace>,
    vertex_lookup: BTreeMap<PlanarPointKey, SketchId>,
    edge_lookup: BTreeMap<(u64, u64), SketchId>,
}

impl Default for SketchBRepKernel {
    fn default() -> Self {
        Self::new()
    }
}

impl SketchBRepKernel {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            vertices: BTreeMap::new(),
            edges: BTreeMap::new(),
            faces: BTreeMap::new(),
            vertex_lookup: BTreeMap::new(),
            edge_lookup: BTreeMap::new(),
        }
    }

    pub fn vertices(&self) -> &BTreeMap<SketchId, SketchBRepVertex> {
        &self.vertices
    }

    pub fn edges(&self) -> &BTreeMap<SketchId, SketchBRepEdge> {
        &self.edges
    }

    pub fn faces(&self) -> &BTreeMap<SketchId, SketchBRepFace> {
        &self.faces
    }

    pub fn face(&self, id: SketchId) -> Option<&SketchBRepFace> {
        self.faces.get(&id)
    }

    pub fn add_face_from_points(
        &mut self,
        points: impl IntoIterator<Item = Vec3>,
    ) -> Result<SketchId, SketchModelError> {
        let mut points: Vec<Vec3> = points.into_iter().collect();
        if points.len() >= 2 && points_close(points[0], *points.last().unwrap()) {
            points.pop();
        }
        if points.len() < 3 {
            return Err(SketchModelError::InvalidGeometry(
                "B-Rep faces require at least three points",
            ));
        }
        let normal = cad_polygon_normal(&points).ok_or(SketchModelError::InvalidGeometry(
            "B-Rep face points must not be collinear",
        ))?;
        let plane_d = -normal.dot(points[0]);
        let face_id = self.allocate_id();
        let mut outer_loop = Vec::with_capacity(points.len());

        for index in 0..points.len() {
            let a = self.vertex_for_point(points[index]);
            let b = self.vertex_for_point(points[(index + 1) % points.len()]);
            if a == b {
                return Err(SketchModelError::InvalidGeometry(
                    "B-Rep face has a collapsed edge",
                ));
            }
            let (edge, reversed) = self.edge_between(a, b);
            outer_loop.push(SketchBRepLoopEdge { edge, reversed });
        }

        self.faces.insert(
            face_id,
            SketchBRepFace {
                id: face_id,
                outer_loop: outer_loop.clone(),
                normal,
                plane_d,
            },
        );
        for loop_edge in outer_loop {
            if let Some(edge) = self.edges.get_mut(&loop_edge.edge) {
                edge.faces.insert(face_id);
            }
        }
        Ok(face_id)
    }

    pub fn face_vertices(&self, face: SketchId) -> Result<Vec<Vec3>, SketchModelError> {
        let face = self
            .faces
            .get(&face)
            .ok_or(SketchModelError::UnknownEntity(face))?;
        face.outer_loop
            .iter()
            .map(|loop_edge| {
                let edge = self
                    .edges
                    .get(&loop_edge.edge)
                    .ok_or(SketchModelError::UnknownEntity(loop_edge.edge))?;
                let vertex_id = if loop_edge.reversed { edge.b } else { edge.a };
                self.vertices
                    .get(&vertex_id)
                    .map(|vertex| vertex.position)
                    .ok_or(SketchModelError::UnknownEntity(vertex_id))
            })
            .collect()
    }

    pub fn split_face_with_edge(
        &mut self,
        face: SketchId,
        start: Vec3,
        end: Vec3,
    ) -> Result<(SketchId, SketchId), SketchModelError> {
        let original = self
            .faces
            .get(&face)
            .cloned()
            .ok_or(SketchModelError::UnknownEntity(face))?;
        if points_close(start, end) {
            return Err(SketchModelError::InvalidGeometry(
                "B-Rep split edge endpoints must differ",
            ));
        }
        if !point_on_plane(start, original.normal, original.plane_d)
            || !point_on_plane(end, original.normal, original.plane_d)
        {
            return Err(SketchModelError::InvalidGeometry(
                "B-Rep split edge must be coplanar with the face",
            ));
        }

        let mut boundary = self.face_vertices(face)?;
        insert_boundary_point(&mut boundary, start).ok_or(SketchModelError::InvalidGeometry(
            "B-Rep split start point must lie on the face boundary",
        ))?;
        insert_boundary_point(&mut boundary, end).ok_or(SketchModelError::InvalidGeometry(
            "B-Rep split end point must lie on the face boundary",
        ))?;
        let start_index = find_point_index(&boundary, start).ok_or(
            SketchModelError::InvalidGeometry("B-Rep split start point could not be resolved"),
        )?;
        let end_index = find_point_index(&boundary, end).ok_or(
            SketchModelError::InvalidGeometry("B-Rep split end point could not be resolved"),
        )?;
        if start_index == end_index {
            return Err(SketchModelError::InvalidGeometry(
                "B-Rep split edge endpoints collapse to the same boundary point",
            ));
        }

        let first_loop = polygon_path_between(&boundary, start_index, end_index);
        let second_loop = polygon_path_between(&boundary, end_index, start_index);
        if first_loop.len() < 3 || second_loop.len() < 3 {
            return Err(SketchModelError::InvalidGeometry(
                "B-Rep split edge must divide the face into two loops",
            ));
        }

        self.remove_face(face)?;
        let first = self.add_face_from_points(first_loop)?;
        let second = self.add_face_from_points(second_loop)?;
        Ok((first, second))
    }

    pub fn push_pull_face(
        &mut self,
        face: SketchId,
        distance: f32,
    ) -> Result<SketchBRepExtrusionResult, SketchModelError> {
        let source = self
            .faces
            .get(&face)
            .cloned()
            .ok_or(SketchModelError::UnknownEntity(face))?;
        if distance.abs() <= PLANAR_GRAPH_MIN_AREA {
            return Err(SketchModelError::InvalidGeometry(
                "B-Rep Push/Pull distance must be non-zero",
            ));
        }
        let base = self.face_vertices(face)?;
        let offset = source.normal * distance;
        let top: Vec<Vec3> = base.iter().map(|point| *point + offset).collect();
        let top_face = self.add_face_from_points(top.clone())?;
        let mut side_faces = Vec::with_capacity(base.len());
        for index in 0..base.len() {
            let next = (index + 1) % base.len();
            side_faces.push(self.add_face_from_points([
                base[index],
                base[next],
                top[next],
                top[index],
            ])?);
        }
        Ok(SketchBRepExtrusionResult {
            source_face: face,
            top_face,
            side_faces,
        })
    }

    fn allocate_id(&mut self) -> SketchId {
        let id = SketchId(self.next_id);
        self.next_id += 1;
        id
    }

    fn vertex_for_point(&mut self, point: Vec3) -> SketchId {
        let key = PlanarPointKey::from_vec3(point);
        if let Some(id) = self.vertex_lookup.get(&key) {
            return *id;
        }
        let id = self.allocate_id();
        self.vertices.insert(
            id,
            SketchBRepVertex {
                id,
                position: point,
                connected_edges: BTreeSet::new(),
            },
        );
        self.vertex_lookup.insert(key, id);
        id
    }

    fn edge_between(&mut self, a: SketchId, b: SketchId) -> (SketchId, bool) {
        let key = sorted_edge_key(a, b);
        if let Some(edge_id) = self.edge_lookup.get(&key).copied() {
            let edge = self.edges.get(&edge_id).expect("B-Rep edge lookup drifted");
            return (edge_id, edge.a != a);
        }

        let id = self.allocate_id();
        self.edges.insert(
            id,
            SketchBRepEdge {
                id,
                a,
                b,
                faces: BTreeSet::new(),
            },
        );
        self.edge_lookup.insert(key, id);
        if let Some(vertex) = self.vertices.get_mut(&a) {
            vertex.connected_edges.insert(id);
        }
        if let Some(vertex) = self.vertices.get_mut(&b) {
            vertex.connected_edges.insert(id);
        }
        (id, false)
    }

    fn remove_face(&mut self, face: SketchId) -> Result<(), SketchModelError> {
        let old = self
            .faces
            .remove(&face)
            .ok_or(SketchModelError::UnknownEntity(face))?;
        for loop_edge in old.outer_loop {
            if let Some(edge) = self.edges.get_mut(&loop_edge.edge) {
                edge.faces.remove(&face);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SketchScene {
    pub id: SketchId,
    pub name: String,
    pub camera: Option<SketchCamera>,
    pub style: Option<SketchId>,
    pub visible_tags: BTreeMap<SketchId, bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SketchCadTool {
    #[serde(alias = "LINE")]
    Pencil,
    #[serde(alias = "RECT")]
    Rectangle,
    Circle,
    Polygon,
    Arc,
    Freehand,
    #[serde(alias = "PUSH", alias = "PUSH_PULL")]
    PushPull,
    Opening,
    Room,
    Road,
    #[serde(alias = "CITY_AREA")]
    BotArea,
}

impl SketchCadTool {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pencil => "PENCIL",
            Self::Rectangle => "RECTANGLE",
            Self::Circle => "CIRCLE",
            Self::Polygon => "POLYGON",
            Self::Arc => "ARC",
            Self::Freehand => "FREEHAND",
            Self::PushPull => "PUSH_PULL",
            Self::Opening => "OPENING",
            Self::Room => "ROOM",
            Self::Road => "ROAD",
            Self::BotArea => "BOT_AREA",
        }
    }

    pub const fn default_label(self) -> &'static str {
        match self {
            Self::Pencil => "CAD pencil",
            Self::Rectangle => "CAD rectangle",
            Self::Circle => "CAD circle",
            Self::Polygon => "CAD polygon",
            Self::Arc => "CAD arc",
            Self::Freehand => "CAD freehand",
            Self::PushPull => "CAD push/pull",
            Self::Opening => "CAD opening",
            Self::Room => "CAD room",
            Self::Road => "CAD road",
            Self::BotArea => "CAD bot area",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SketchCadPoint {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl From<Vec3> for SketchCadPoint {
    fn from(value: Vec3) -> Self {
        Self {
            x: value.x,
            y: value.y,
            z: value.z,
        }
    }
}

impl From<SketchCadPoint> for Vec3 {
    fn from(value: SketchCadPoint) -> Self {
        Self::new(value.x, value.y, value.z)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SketchCadCommand {
    pub tool: SketchCadTool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sides: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sweep_radians: Option<f32>,
    #[serde(default)]
    pub points: Vec<SketchCadPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl SketchCadCommand {
    pub fn new(tool: SketchCadTool) -> Self {
        Self {
            tool,
            material: None,
            width: None,
            height: None,
            depth: None,
            target: None,
            segments: None,
            sides: None,
            sweep_radians: None,
            points: Vec::new(),
            label: None,
        }
    }

    pub fn with_material(mut self, material: impl Into<String>) -> Self {
        self.material = Some(material.into());
        self
    }

    pub fn with_width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn with_height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    pub fn with_depth(mut self, depth: f32) -> Self {
        self.depth = Some(depth);
        self
    }

    pub fn with_target(mut self, target: SketchId) -> Self {
        self.target = Some(target.raw());
        self
    }

    pub fn with_segments(mut self, segments: usize) -> Self {
        self.segments = Some(segments);
        self
    }

    pub fn with_sides(mut self, sides: usize) -> Self {
        self.sides = Some(sides);
        self
    }

    pub fn with_sweep_radians(mut self, sweep_radians: f32) -> Self {
        self.sweep_radians = Some(sweep_radians);
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_points(mut self, points: impl IntoIterator<Item = Vec3>) -> Self {
        self.points = points.into_iter().map(SketchCadPoint::from).collect();
        self
    }

    fn points_vec3(&self) -> Vec<Vec3> {
        self.points.iter().copied().map(Vec3::from).collect()
    }

    fn label_or_default(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| self.tool.default_label().to_string())
    }

    fn target_id(&self) -> Option<SketchId> {
        self.target.map(SketchId)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchCadCommandResult {
    pub label: String,
    pub entities: Vec<SketchId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SketchVoxelCellKey {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl SketchVoxelCellKey {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    pub fn from_ivec3(cell: IVec3) -> Self {
        Self::new(cell.x, cell.y, cell.z)
    }

    pub fn as_ivec3(self) -> IVec3 {
        IVec3::new(self.x, self.y, self.z)
    }
}

impl From<IVec3> for SketchVoxelCellKey {
    fn from(value: IVec3) -> Self {
        Self::from_ivec3(value)
    }
}

impl From<SketchVoxelCellKey> for IVec3 {
    fn from(value: SketchVoxelCellKey) -> Self {
        value.as_ivec3()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SketchVoxelFaceKey {
    pub cell: SketchVoxelCellKey,
    pub normal_x: i8,
    pub normal_y: i8,
    pub normal_z: i8,
}

impl SketchVoxelFaceKey {
    pub fn new(cell: IVec3, normal: IVec3) -> Option<Self> {
        if normal_axis_index(normal).is_none() {
            return None;
        }
        Some(Self {
            cell: SketchVoxelCellKey::from_ivec3(cell),
            normal_x: normal.x as i8,
            normal_y: normal.y as i8,
            normal_z: normal.z as i8,
        })
    }

    pub fn normal(self) -> IVec3 {
        IVec3::new(
            self.normal_x as i32,
            self.normal_y as i32,
            self.normal_z as i32,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SketchVoxelLinkRole {
    Face,
    Stroke,
    Shape,
    Extrusion,
    Opening,
    Room,
    Road,
    BotArea,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SketchVoxelLink {
    pub role: SketchVoxelLinkRole,
    pub entity: SketchId,
    pub context: SketchId,
}

impl SketchVoxelLink {
    pub const fn new(entity: SketchId, context: SketchId, role: SketchVoxelLinkRole) -> Self {
        Self {
            role,
            entity,
            context,
        }
    }
}

#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct SketchVoxelLinkIndex {
    cell_links: BTreeMap<SketchVoxelCellKey, BTreeSet<SketchVoxelLink>>,
    face_links: BTreeMap<SketchVoxelFaceKey, BTreeSet<SketchVoxelLink>>,
}

impl SketchVoxelLinkIndex {
    pub fn clear(&mut self) {
        self.cell_links.clear();
        self.face_links.clear();
    }

    pub fn link_cell(&mut self, cell: IVec3, link: SketchVoxelLink) {
        self.cell_links
            .entry(SketchVoxelCellKey::from_ivec3(cell))
            .or_default()
            .insert(link);
    }

    pub fn link_cells(&mut self, cells: impl IntoIterator<Item = IVec3>, link: SketchVoxelLink) {
        for cell in cells {
            self.link_cell(cell, link);
        }
    }

    pub fn link_face_cell(&mut self, cell: IVec3, normal: IVec3, link: SketchVoxelLink) -> bool {
        let Some(face_key) = SketchVoxelFaceKey::new(cell, normal) else {
            return false;
        };
        self.link_cell(cell, link);
        self.face_links.entry(face_key).or_default().insert(link);
        true
    }

    pub fn link_face_cells(
        &mut self,
        cells: impl IntoIterator<Item = IVec3>,
        normal: IVec3,
        link: SketchVoxelLink,
    ) -> usize {
        cells
            .into_iter()
            .filter(|cell| self.link_face_cell(*cell, normal, link))
            .count()
    }

    pub fn links_for_cell(&self, cell: IVec3) -> Vec<SketchVoxelLink> {
        self.cell_links
            .get(&SketchVoxelCellKey::from_ivec3(cell))
            .map(|links| links.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn links_for_face(&self, cell: IVec3, normal: IVec3) -> Vec<SketchVoxelLink> {
        let Some(face_key) = SketchVoxelFaceKey::new(cell, normal) else {
            return Vec::new();
        };
        self.face_links
            .get(&face_key)
            .map(|links| links.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn primary_face_link(&self, cell: IVec3, normal: IVec3) -> Option<SketchVoxelLink> {
        self.links_for_face(cell, normal).into_iter().next()
    }

    pub fn hit_for_face(
        &self,
        cell: IVec3,
        normal: IVec3,
        world_point: Vec3,
        distance: f32,
    ) -> Option<HitRecord> {
        let link = self.primary_face_link(cell, normal)?;
        Some(
            HitRecord::new(
                link.entity,
                std::iter::empty::<SketchId>(),
                HitKind::Face,
                world_point,
                distance,
            )
            .with_normal(ivec3_to_vec3(normal)),
        )
    }

    pub fn remove_entity(&mut self, entity: SketchId) {
        for links in self.cell_links.values_mut() {
            links.retain(|link| link.entity != entity);
        }
        self.cell_links.retain(|_, links| !links.is_empty());
        for links in self.face_links.values_mut() {
            links.retain(|link| link.entity != entity);
        }
        self.face_links.retain(|_, links| !links.is_empty());
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SketchDocumentSnapshot {
    pub version: u32,
    pub id: u64,
    pub root_context: u64,
    pub active_context: u64,
    pub default_tag: u64,
    pub default_material: u64,
    pub default_style: u64,
    pub active_style: u64,
    pub active_scene: Option<u64>,
    pub next_id: u64,
    pub contexts: Vec<SketchContextSnapshot>,
    pub entities: Vec<SketchEntitySnapshot>,
    pub definitions: Vec<ComponentDefinitionSnapshot>,
    pub materials: Vec<SketchMaterialSnapshot>,
    pub tags: Vec<SketchTagSnapshot>,
    pub styles: Vec<SketchStyleSnapshot>,
    pub scenes: Vec<SketchSceneSnapshot>,
    pub attributes: AttributeStore,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SketchContextSnapshot {
    pub id: u64,
    pub parent: Option<u64>,
    pub local_to_parent: SketchTransformSnapshot,
    pub entities: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentDefinitionSnapshot {
    pub id: u64,
    pub name: String,
    pub context: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SketchMaterialSnapshot {
    pub id: u64,
    pub name: String,
    pub color: SketchColorSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SketchColorSnapshot {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SketchTagSnapshot {
    pub id: u64,
    pub name: String,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SketchStyleSnapshot {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SketchSceneSnapshot {
    pub id: u64,
    pub name: String,
    pub camera: Option<SketchCameraSnapshot>,
    pub style: Option<u64>,
    pub visible_tags: BTreeMap<u64, bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SketchCameraSnapshot {
    pub eye: [f32; 3],
    pub target: [f32; 3],
    pub up: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SketchEntitySnapshot {
    pub id: u64,
    pub kind: SketchEntityKindSnapshot,
    pub visible: bool,
    pub locked: bool,
    pub material: Option<u64>,
    pub tag: Option<u64>,
    pub attributes: AttributeStore,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SketchEntityKindSnapshot {
    Vertex {
        point: [f32; 3],
    },
    Edge {
        a: [f32; 3],
        b: [f32; 3],
    },
    Face {
        vertices: Vec<[f32; 3]>,
        normal: [f32; 3],
    },
    CircleFace {
        center: [f32; 3],
        normal: [f32; 3],
        radius: f32,
        segments: usize,
        vertices: Vec<[f32; 3]>,
    },
    PolygonFace {
        center: [f32; 3],
        normal: [f32; 3],
        radius: f32,
        sides: usize,
        vertices: Vec<[f32; 3]>,
    },
    ArcCurve {
        center: [f32; 3],
        normal: [f32; 3],
        radius: f32,
        start_direction: [f32; 3],
        sweep_radians: f32,
        points: Vec<[f32; 3]>,
    },
    FreehandCurve {
        points: Vec<[f32; 3]>,
    },
    PushPullExtrusion {
        source_face: u64,
        base_vertices: Vec<[f32; 3]>,
        top_vertices: Vec<[f32; 3]>,
        normal: [f32; 3],
        depth: f32,
        bounds: SketchBoundsSnapshot,
    },
    Opening {
        host: u64,
        center: [f32; 3],
        size: [f32; 3],
        normal: [f32; 3],
        through_depth: f32,
        bounds: SketchBoundsSnapshot,
    },
    Room {
        shell: u64,
        shell_bounds: SketchBoundsSnapshot,
        interior_bounds: SketchBoundsSnapshot,
        wall_thickness: f32,
    },
    Group {
        context: u64,
    },
    ComponentInstance {
        definition: u64,
        transform: SketchTransformSnapshot,
    },
    GuidePoint {
        point: [f32; 3],
    },
    GuideLine {
        origin: [f32; 3],
        direction: [f32; 3],
    },
    SectionPlane {
        origin: [f32; 3],
        normal: [f32; 3],
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SketchTransformSnapshot {
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SketchBoundsSnapshot {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SketchSnapshotError {
    Serialize(ron::Error),
    Deserialize(ron::error::SpannedError),
    UnsupportedVersion(u32),
}

impl fmt::Display for SketchSnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialize(error) => write!(f, "failed to serialize sketch document: {error}"),
            Self::Deserialize(error) => write!(f, "failed to deserialize sketch document: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported sketch document save version {version}")
            }
        }
    }
}

impl Error for SketchSnapshotError {}

#[derive(Debug, Clone, PartialEq)]
struct SketchEntityRecord {
    context: SketchId,
    entity: SketchEntity,
}

#[derive(Debug, Clone, PartialEq)]
enum SketchEditChange {
    Created(SketchEntityRecord),
    Modified {
        context: SketchId,
        before: SketchEntity,
        after: SketchEntity,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct SketchEditBatch {
    label: String,
    changes: Vec<SketchEditChange>,
}

impl SketchEditBatch {
    fn new(label: impl Into<String>, changes: Vec<SketchEditChange>) -> Self {
        Self {
            label: label.into(),
            changes,
        }
    }

    fn summary(&self) -> SketchEditSummary {
        SketchEditSummary {
            label: self.label.clone(),
            entity_count: self.changes.len(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchEditSummary {
    pub label: String,
    pub entity_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SketchModelError {
    UnknownContext(SketchId),
    UnknownDefinition(SketchId),
    UnknownEntity(SketchId),
    UnknownMaterial(SketchId),
    UnknownTag(SketchId),
    UnknownStyle(SketchId),
    UnknownScene(SketchId),
    NotComponentInstance(SketchId),
    InvalidGeometry(&'static str),
    InvalidCadCommand(&'static str),
}

impl fmt::Display for SketchModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownContext(id) => write!(f, "unknown sketch context {}", id.raw()),
            Self::UnknownDefinition(id) => {
                write!(f, "unknown component definition {}", id.raw())
            }
            Self::UnknownEntity(id) => write!(f, "unknown sketch entity {}", id.raw()),
            Self::UnknownMaterial(id) => write!(f, "unknown material {}", id.raw()),
            Self::UnknownTag(id) => write!(f, "unknown tag {}", id.raw()),
            Self::UnknownStyle(id) => write!(f, "unknown style {}", id.raw()),
            Self::UnknownScene(id) => write!(f, "unknown scene {}", id.raw()),
            Self::NotComponentInstance(id) => {
                write!(f, "entity {} is not a component instance", id.raw())
            }
            Self::InvalidGeometry(reason) => write!(f, "invalid geometry: {reason}"),
            Self::InvalidCadCommand(reason) => write!(f, "invalid CAD command: {reason}"),
        }
    }
}

impl Error for SketchModelError {}

#[derive(Resource, Debug, Clone)]
pub struct SketchDocument {
    id: SketchId,
    root_context: SketchId,
    active_context: SketchId,
    default_tag: SketchId,
    default_material: SketchId,
    default_style: SketchId,
    active_style: SketchId,
    active_scene: Option<SketchId>,
    next_id: u64,
    contexts: BTreeMap<SketchId, SketchContext>,
    entities: BTreeMap<SketchId, SketchEntity>,
    definitions: BTreeMap<SketchId, ComponentDefinition>,
    materials: BTreeMap<SketchId, SketchMaterial>,
    tags: BTreeMap<SketchId, SketchTag>,
    styles: BTreeMap<SketchId, SketchStyle>,
    scenes: BTreeMap<SketchId, SketchScene>,
    attributes: AttributeStore,
    undo_stack: Vec<SketchEditBatch>,
    redo_stack: Vec<SketchEditBatch>,
    pub undo_generation: u64,
}

impl Default for SketchDocument {
    fn default() -> Self {
        Self::new()
    }
}

impl SketchDocument {
    pub fn new() -> Self {
        let id = SketchId(1);
        let root_context = SketchId(2);
        let default_tag = SketchId(3);
        let default_material = SketchId(4);
        let default_style = SketchId(5);

        let mut contexts = BTreeMap::new();
        contexts.insert(root_context, SketchContext::root(root_context));

        let mut tags = BTreeMap::new();
        tags.insert(
            default_tag,
            SketchTag {
                id: default_tag,
                name: "Untagged".into(),
                visible: true,
            },
        );

        let mut materials = BTreeMap::new();
        materials.insert(
            default_material,
            SketchMaterial {
                id: default_material,
                name: "Default".into(),
                color: SketchColor::rgb(255, 255, 255),
            },
        );

        let mut styles = BTreeMap::new();
        styles.insert(
            default_style,
            SketchStyle {
                id: default_style,
                name: "Modeling".into(),
            },
        );

        Self {
            id,
            root_context,
            active_context: root_context,
            default_tag,
            default_material,
            default_style,
            active_style: default_style,
            active_scene: None,
            next_id: 6,
            contexts,
            entities: BTreeMap::new(),
            definitions: BTreeMap::new(),
            materials,
            tags,
            styles,
            scenes: BTreeMap::new(),
            attributes: BTreeMap::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_generation: 0,
        }
    }

    pub fn id(&self) -> SketchId {
        self.id
    }

    pub fn root_context(&self) -> SketchId {
        self.root_context
    }

    pub fn active_context(&self) -> SketchId {
        self.active_context
    }

    pub fn context(&self, id: SketchId) -> Option<&SketchContext> {
        self.contexts.get(&id)
    }

    pub fn entity(&self, id: SketchId) -> Option<&SketchEntity> {
        self.entities.get(&id)
    }

    pub fn material(&self, id: SketchId) -> Option<&SketchMaterial> {
        self.materials.get(&id)
    }

    pub fn tag(&self, id: SketchId) -> Option<&SketchTag> {
        self.tags.get(&id)
    }

    pub fn style(&self, id: SketchId) -> Option<&SketchStyle> {
        self.styles.get(&id)
    }

    pub fn scene(&self, id: SketchId) -> Option<&SketchScene> {
        self.scenes.get(&id)
    }

    pub fn active_style(&self) -> SketchId {
        self.active_style
    }

    pub fn active_scene(&self) -> Option<SketchId> {
        self.active_scene
    }

    pub fn to_stable_snapshot(&self) -> SketchDocumentSnapshot {
        SketchDocumentSnapshot {
            version: SKETCH_DOCUMENT_SAVE_VERSION,
            id: self.id.raw(),
            root_context: self.root_context.raw(),
            active_context: self.active_context.raw(),
            default_tag: self.default_tag.raw(),
            default_material: self.default_material.raw(),
            default_style: self.default_style.raw(),
            active_style: self.active_style.raw(),
            active_scene: self.active_scene.map(SketchId::raw),
            next_id: self.next_id,
            contexts: self.contexts.values().map(snapshot_context).collect(),
            entities: self.entities.values().map(snapshot_entity).collect(),
            definitions: self.definitions.values().map(snapshot_definition).collect(),
            materials: self.materials.values().map(snapshot_material).collect(),
            tags: self.tags.values().map(snapshot_tag).collect(),
            styles: self.styles.values().map(snapshot_style).collect(),
            scenes: self.scenes.values().map(snapshot_scene).collect(),
            attributes: self.attributes.clone(),
        }
    }

    pub fn to_stable_ron(&self) -> Result<String, SketchSnapshotError> {
        ron::ser::to_string_pretty(
            &self.to_stable_snapshot(),
            ron::ser::PrettyConfig::default(),
        )
        .map_err(SketchSnapshotError::Serialize)
    }

    pub fn from_stable_ron(text: &str) -> Result<Self, SketchSnapshotError> {
        let snapshot: SketchDocumentSnapshot =
            ron::from_str(text).map_err(SketchSnapshotError::Deserialize)?;
        Self::from_stable_snapshot(snapshot)
    }

    pub fn from_stable_snapshot(
        snapshot: SketchDocumentSnapshot,
    ) -> Result<Self, SketchSnapshotError> {
        if snapshot.version != SKETCH_DOCUMENT_SAVE_VERSION {
            return Err(SketchSnapshotError::UnsupportedVersion(snapshot.version));
        }

        let contexts = snapshot
            .contexts
            .into_iter()
            .map(|context| (SketchId(context.id), restore_context(context)))
            .collect();
        let entities = snapshot
            .entities
            .into_iter()
            .map(|entity| (SketchId(entity.id), restore_entity(entity)))
            .collect();
        let definitions = snapshot
            .definitions
            .into_iter()
            .map(|definition| (SketchId(definition.id), restore_definition(definition)))
            .collect();
        let materials = snapshot
            .materials
            .into_iter()
            .map(|material| (SketchId(material.id), restore_material(material)))
            .collect();
        let tags = snapshot
            .tags
            .into_iter()
            .map(|tag| (SketchId(tag.id), restore_tag(tag)))
            .collect();
        let styles = snapshot
            .styles
            .into_iter()
            .map(|style| (SketchId(style.id), restore_style(style)))
            .collect();
        let scenes = snapshot
            .scenes
            .into_iter()
            .map(|scene| (SketchId(scene.id), restore_scene(scene)))
            .collect();

        Ok(Self {
            id: SketchId(snapshot.id),
            root_context: SketchId(snapshot.root_context),
            active_context: SketchId(snapshot.active_context),
            default_tag: SketchId(snapshot.default_tag),
            default_material: SketchId(snapshot.default_material),
            default_style: SketchId(snapshot.default_style),
            active_style: SketchId(snapshot.active_style),
            active_scene: snapshot.active_scene.map(SketchId),
            next_id: snapshot.next_id,
            contexts,
            entities,
            definitions,
            materials,
            tags,
            styles,
            scenes,
            attributes: snapshot.attributes,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_generation: 0,
        })
    }

    pub fn default_tag_name(&self) -> Option<&str> {
        self.tags
            .get(&self.default_tag)
            .map(|tag| tag.name.as_str())
    }

    pub fn default_material_name(&self) -> Option<&str> {
        self.materials
            .get(&self.default_material)
            .map(|material| material.name.as_str())
    }

    pub fn default_material(&self) -> SketchId {
        self.default_material
    }

    pub fn default_style_name(&self) -> Option<&str> {
        self.styles
            .get(&self.default_style)
            .map(|style| style.name.as_str())
    }

    pub fn create_material(
        &mut self,
        name: impl Into<String>,
        color: SketchColor,
    ) -> Result<SketchId, SketchModelError> {
        let id = self.allocate_id();
        self.materials.insert(
            id,
            SketchMaterial {
                id,
                name: name.into(),
                color,
            },
        );
        self.undo_generation += 1;
        Ok(id)
    }

    pub fn create_tag(&mut self, name: impl Into<String>) -> Result<SketchId, SketchModelError> {
        let id = self.allocate_id();
        self.tags.insert(
            id,
            SketchTag {
                id,
                name: name.into(),
                visible: true,
            },
        );
        self.undo_generation += 1;
        Ok(id)
    }

    pub fn create_style(&mut self, name: impl Into<String>) -> Result<SketchId, SketchModelError> {
        let id = self.allocate_id();
        self.styles.insert(
            id,
            SketchStyle {
                id,
                name: name.into(),
            },
        );
        self.undo_generation += 1;
        Ok(id)
    }

    pub fn assign_entity_material(
        &mut self,
        entity: SketchId,
        material: SketchId,
    ) -> Result<(), SketchModelError> {
        if !self.materials.contains_key(&material) {
            return Err(SketchModelError::UnknownMaterial(material));
        }
        let entity = self
            .entities
            .get_mut(&entity)
            .ok_or(SketchModelError::UnknownEntity(entity))?;
        entity.material = Some(material);
        self.undo_generation += 1;
        Ok(())
    }

    pub fn assign_entity_tag(
        &mut self,
        entity: SketchId,
        tag: SketchId,
    ) -> Result<(), SketchModelError> {
        if !self.tags.contains_key(&tag) {
            return Err(SketchModelError::UnknownTag(tag));
        }
        let entity = self
            .entities
            .get_mut(&entity)
            .ok_or(SketchModelError::UnknownEntity(entity))?;
        entity.tag = Some(tag);
        self.undo_generation += 1;
        Ok(())
    }

    pub fn set_tag_visibility(
        &mut self,
        tag: SketchId,
        visible: bool,
    ) -> Result<(), SketchModelError> {
        let tag = self
            .tags
            .get_mut(&tag)
            .ok_or(SketchModelError::UnknownTag(tag))?;
        tag.visible = visible;
        self.undo_generation += 1;
        Ok(())
    }

    pub fn tag_visible(&self, tag: SketchId) -> Result<bool, SketchModelError> {
        self.tags
            .get(&tag)
            .map(|tag| tag.visible)
            .ok_or(SketchModelError::UnknownTag(tag))
    }

    pub fn entity_effective_visible(&self, entity: SketchId) -> Result<bool, SketchModelError> {
        let entity = self
            .entities
            .get(&entity)
            .ok_or(SketchModelError::UnknownEntity(entity))?;
        let tag_visible = entity
            .tag
            .map(|tag| self.tag_visible(tag))
            .transpose()?
            .unwrap_or(true);
        Ok(entity.visible && tag_visible)
    }

    pub fn set_active_style(&mut self, style: SketchId) -> Result<(), SketchModelError> {
        if !self.styles.contains_key(&style) {
            return Err(SketchModelError::UnknownStyle(style));
        }
        self.active_style = style;
        self.undo_generation += 1;
        Ok(())
    }

    pub fn capture_scene(
        &mut self,
        name: impl Into<String>,
        camera: Option<SketchCamera>,
    ) -> Result<SketchId, SketchModelError> {
        let id = self.allocate_id();
        let visible_tags = self
            .tags
            .iter()
            .map(|(id, tag)| (*id, tag.visible))
            .collect();
        self.scenes.insert(
            id,
            SketchScene {
                id,
                name: name.into(),
                camera,
                style: Some(self.active_style),
                visible_tags,
            },
        );
        self.undo_generation += 1;
        Ok(id)
    }

    pub fn apply_scene(&mut self, scene: SketchId) -> Result<(), SketchModelError> {
        let scene_record = self
            .scenes
            .get(&scene)
            .cloned()
            .ok_or(SketchModelError::UnknownScene(scene))?;
        for (tag, visible) in scene_record.visible_tags {
            if let Some(tag) = self.tags.get_mut(&tag) {
                tag.visible = visible;
            }
        }
        if let Some(style) = scene_record.style {
            if !self.styles.contains_key(&style) {
                return Err(SketchModelError::UnknownStyle(style));
            }
            self.active_style = style;
        }
        self.active_scene = Some(scene);
        self.undo_generation += 1;
        Ok(())
    }

    pub fn set_model_attribute(
        &mut self,
        namespace: impl Into<String>,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), SketchModelError> {
        self.attributes
            .entry(namespace.into())
            .or_default()
            .insert(key.into(), value.into());
        self.undo_generation += 1;
        Ok(())
    }

    pub fn model_attribute(&self, namespace: &str, key: &str) -> Option<&str> {
        self.attributes
            .get(namespace)
            .and_then(|attrs| attrs.get(key))
            .map(String::as_str)
    }

    pub fn set_entity_attribute(
        &mut self,
        entity: SketchId,
        namespace: impl Into<String>,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), SketchModelError> {
        let entity = self
            .entities
            .get_mut(&entity)
            .ok_or(SketchModelError::UnknownEntity(entity))?;
        entity
            .attributes
            .entry(namespace.into())
            .or_default()
            .insert(key.into(), value.into());
        self.undo_generation += 1;
        Ok(())
    }

    pub fn entity_attribute(
        &self,
        entity: SketchId,
        namespace: &str,
        key: &str,
    ) -> Result<Option<&str>, SketchModelError> {
        let entity = self
            .entities
            .get(&entity)
            .ok_or(SketchModelError::UnknownEntity(entity))?;
        Ok(entity
            .attributes
            .get(namespace)
            .and_then(|attrs| attrs.get(key))
            .map(String::as_str))
    }

    pub fn material_by_name(&self, name: &str) -> Option<SketchId> {
        self.materials
            .iter()
            .find(|(_, material)| material.name.eq_ignore_ascii_case(name))
            .map(|(id, _)| *id)
    }

    pub fn execute_cad_commands(
        &mut self,
        context: SketchId,
        commands: &[SketchCadCommand],
    ) -> Result<Vec<SketchCadCommandResult>, SketchModelError> {
        commands
            .iter()
            .map(|command| self.execute_cad_command(context, command))
            .collect()
    }

    pub fn execute_cad_command(
        &mut self,
        context: SketchId,
        command: &SketchCadCommand,
    ) -> Result<SketchCadCommandResult, SketchModelError> {
        if !self.contexts.contains_key(&context) {
            return Err(SketchModelError::UnknownContext(context));
        }

        let label = command.label_or_default();
        let points = command.points_vec3();
        let mut created = Vec::new();
        let mut result_entities = Vec::new();

        match command.tool {
            SketchCadTool::Pencil => {
                if points.len() < 2 {
                    return Err(SketchModelError::InvalidCadCommand(
                        "PENCIL requires at least two points",
                    ));
                }
                for segment in points.windows(2) {
                    self.add_cad_entity(
                        context,
                        SketchEntityKind::Edge {
                            a: segment[0],
                            b: segment[1],
                        },
                        command,
                        &mut created,
                        &mut result_entities,
                    )?;
                }
            }
            SketchCadTool::Rectangle => {
                let (vertices, normal) = cad_rectangle_face(&points)?;
                self.add_cad_entity(
                    context,
                    SketchEntityKind::Face { vertices, normal },
                    command,
                    &mut created,
                    &mut result_entities,
                )?;
            }
            SketchCadTool::Circle => {
                if points.len() < 2 {
                    return Err(SketchModelError::InvalidCadCommand(
                        "CIRCLE requires center and radius point",
                    ));
                }
                let center = points[0];
                let radius_vector = points[1] - center;
                let radius = radius_vector.length().max(PLANAR_GRAPH_MIN_AREA);
                let normal = cad_polygon_normal(&points).unwrap_or(Vec3::Y);
                let segments = command.segments.unwrap_or(32).max(8);
                let vertices = radial_points(center, normal, radius, segments, Some(radius_vector));
                self.add_cad_entity(
                    context,
                    SketchEntityKind::CircleFace {
                        center,
                        normal: safe_normal(normal),
                        radius,
                        segments,
                        vertices,
                    },
                    command,
                    &mut created,
                    &mut result_entities,
                )?;
            }
            SketchCadTool::Polygon => {
                if points.len() < 2 {
                    return Err(SketchModelError::InvalidCadCommand(
                        "POLYGON requires center and radius point",
                    ));
                }
                let center = points[0];
                let radius_vector = points[1] - center;
                let radius = radius_vector.length().max(PLANAR_GRAPH_MIN_AREA);
                let normal = cad_polygon_normal(&points).unwrap_or(Vec3::Y);
                let sides = command.sides.unwrap_or(6).max(3);
                let vertices = radial_points(center, normal, radius, sides, Some(radius_vector));
                self.add_cad_entity(
                    context,
                    SketchEntityKind::PolygonFace {
                        center,
                        normal: safe_normal(normal),
                        radius,
                        sides,
                        vertices,
                    },
                    command,
                    &mut created,
                    &mut result_entities,
                )?;
            }
            SketchCadTool::Arc => {
                if points.len() < 2 {
                    return Err(SketchModelError::InvalidCadCommand(
                        "ARC requires center and start point",
                    ));
                }
                let center = points[0];
                let start_direction = points[1] - center;
                let radius = start_direction.length().max(PLANAR_GRAPH_MIN_AREA);
                let normal = cad_polygon_normal(&points).unwrap_or(Vec3::Y);
                let sweep_radians = command.sweep_radians.unwrap_or(std::f32::consts::FRAC_PI_2);
                let segments = command.segments.unwrap_or(12).max(1);
                let (axis_u, axis_v, normal) = plane_basis(normal, Some(start_direction));
                let arc_points = (0..=segments)
                    .map(|index| {
                        let t = index as f32 / segments as f32;
                        let angle = sweep_radians * t;
                        center + (axis_u * angle.cos() + axis_v * angle.sin()) * radius
                    })
                    .collect();
                self.add_cad_entity(
                    context,
                    SketchEntityKind::ArcCurve {
                        center,
                        normal,
                        radius,
                        start_direction: axis_u,
                        sweep_radians,
                        points: arc_points,
                    },
                    command,
                    &mut created,
                    &mut result_entities,
                )?;
            }
            SketchCadTool::Freehand | SketchCadTool::Road => {
                if points.len() < 2 {
                    return Err(SketchModelError::InvalidCadCommand(
                        "curve commands require at least two points",
                    ));
                }
                self.add_cad_entity(
                    context,
                    SketchEntityKind::FreehandCurve { points },
                    command,
                    &mut created,
                    &mut result_entities,
                )?;
            }
            SketchCadTool::PushPull => {
                let target = command
                    .target_id()
                    .ok_or(SketchModelError::InvalidCadCommand(
                        "PUSH_PULL requires target face",
                    ))?;
                let (base_vertices, normal) = self.face_geometry(target)?;
                let depth = command.depth.or(command.height).unwrap_or(1.0);
                let offset = normal * depth;
                let top_vertices: Vec<_> = base_vertices
                    .iter()
                    .map(|vertex| *vertex + offset)
                    .collect();
                let bounds = SketchBounds::from_points(
                    base_vertices
                        .iter()
                        .copied()
                        .chain(top_vertices.iter().copied()),
                )
                .unwrap_or(SketchBounds {
                    min: Vec3::ZERO,
                    max: Vec3::ZERO,
                });
                self.add_cad_entity(
                    context,
                    SketchEntityKind::PushPullExtrusion {
                        source_face: target,
                        base_vertices,
                        top_vertices,
                        normal,
                        depth,
                        bounds,
                    },
                    command,
                    &mut created,
                    &mut result_entities,
                )?;
            }
            SketchCadTool::Opening => {
                let target = command
                    .target_id()
                    .ok_or(SketchModelError::InvalidCadCommand(
                        "OPENING requires target face",
                    ))?;
                let center = *points.first().ok_or(SketchModelError::InvalidCadCommand(
                    "OPENING requires a center point",
                ))?;
                let (_, normal) = self.face_geometry(target)?;
                let width = command
                    .width
                    .unwrap_or(2.0)
                    .abs()
                    .max(PLANAR_GRAPH_MIN_AREA);
                let height = command
                    .height
                    .unwrap_or(width)
                    .abs()
                    .max(PLANAR_GRAPH_MIN_AREA);
                let through_depth = command
                    .depth
                    .unwrap_or(1.0)
                    .abs()
                    .max(PLANAR_GRAPH_MIN_AREA);
                let size = cad_opening_size(normal, width, height);
                let bounds =
                    SketchBounds::from_center_size(center, size).extruded(normal, through_depth);
                self.add_cad_entity(
                    context,
                    SketchEntityKind::Opening {
                        host: target,
                        center,
                        size,
                        normal,
                        through_depth,
                        bounds,
                    },
                    command,
                    &mut created,
                    &mut result_entities,
                )?;
            }
            SketchCadTool::Room => {
                if points.len() < 3 {
                    return Err(SketchModelError::InvalidCadCommand(
                        "ROOM requires a closed polygon footprint",
                    ));
                }
                let normal = cad_polygon_normal(&points).unwrap_or(Vec3::Y);
                let shell = self.add_cad_entity(
                    context,
                    SketchEntityKind::Face {
                        vertices: points.clone(),
                        normal,
                    },
                    command,
                    &mut created,
                    &mut result_entities,
                )?;
                let room_depth = command
                    .height
                    .or(command.depth)
                    .unwrap_or(3.0)
                    .abs()
                    .max(PLANAR_GRAPH_MIN_AREA);
                let wall_thickness = command
                    .width
                    .unwrap_or(0.35)
                    .abs()
                    .max(PLANAR_GRAPH_MIN_AREA);
                let face_bounds = SketchBounds::from_points(points).unwrap_or(SketchBounds {
                    min: Vec3::ZERO,
                    max: Vec3::ZERO,
                });
                let shell_bounds = face_bounds.extruded(normal, room_depth);
                let interior_bounds = shell_bounds.inset(wall_thickness);
                self.add_cad_entity(
                    context,
                    SketchEntityKind::Room {
                        shell,
                        shell_bounds,
                        interior_bounds,
                        wall_thickness,
                    },
                    command,
                    &mut created,
                    &mut result_entities,
                )?;
            }
            SketchCadTool::BotArea => {
                if points.len() < 3 {
                    return Err(SketchModelError::InvalidCadCommand(
                        "BOT_AREA requires at least three points",
                    ));
                }
                let normal = cad_polygon_normal(&points).unwrap_or(Vec3::Y);
                self.add_cad_entity(
                    context,
                    SketchEntityKind::Face {
                        vertices: points,
                        normal,
                    },
                    command,
                    &mut created,
                    &mut result_entities,
                )?;
            }
        }

        if created.is_empty() {
            return Err(SketchModelError::InvalidCadCommand(
                "command did not create semantic entities",
            ));
        }

        self.record_created_entities(label.clone(), created.iter().copied())?;
        Ok(SketchCadCommandResult {
            label,
            entities: result_entities,
        })
    }

    pub fn add_entity_to_active(
        &mut self,
        kind: SketchEntityKind,
    ) -> Result<SketchId, SketchModelError> {
        self.add_entity(self.active_context, kind)
    }

    pub fn add_entity(
        &mut self,
        context: SketchId,
        kind: SketchEntityKind,
    ) -> Result<SketchId, SketchModelError> {
        if !self.contexts.contains_key(&context) {
            return Err(SketchModelError::UnknownContext(context));
        }

        let id = self.allocate_id();
        let entity = SketchEntity::new(
            id,
            kind,
            Some(self.default_material),
            Some(self.default_tag),
        );
        self.entities.insert(id, entity);
        self.contexts
            .get_mut(&context)
            .expect("context was checked above")
            .entities
            .push(id);
        self.undo_generation += 1;
        Ok(id)
    }

    fn add_cad_entity(
        &mut self,
        context: SketchId,
        kind: SketchEntityKind,
        command: &SketchCadCommand,
        created: &mut Vec<(SketchId, SketchId)>,
        result_entities: &mut Vec<SketchId>,
    ) -> Result<SketchId, SketchModelError> {
        let id = self.add_entity(context, kind)?;
        self.apply_cad_entity_metadata(id, command)?;
        created.push((context, id));
        result_entities.push(id);
        Ok(id)
    }

    fn apply_cad_entity_metadata(
        &mut self,
        entity: SketchId,
        command: &SketchCadCommand,
    ) -> Result<(), SketchModelError> {
        let material = command
            .material
            .as_deref()
            .map(|name| self.ensure_material_named(name))
            .transpose()?;
        let entity = self
            .entities
            .get_mut(&entity)
            .ok_or(SketchModelError::UnknownEntity(entity))?;
        if let Some(material) = material {
            entity.material = Some(material);
        }

        let attributes = entity.attributes.entry("cad".to_string()).or_default();
        attributes.insert("tool".to_string(), command.tool.as_str().to_string());
        attributes.insert("label".to_string(), command.label_or_default());
        if let Some(material) = command.material.as_deref() {
            attributes.insert("material".to_string(), material.to_string());
        }
        if let Some(width) = command.width {
            attributes.insert("width".to_string(), format_cad_number(width));
        }
        if let Some(height) = command.height {
            attributes.insert("height".to_string(), format_cad_number(height));
        }
        if let Some(depth) = command.depth {
            attributes.insert("depth".to_string(), format_cad_number(depth));
        }
        if let Some(target) = command.target {
            attributes.insert("target".to_string(), target.to_string());
        }
        Ok(())
    }

    fn ensure_material_named(&mut self, name: &str) -> Result<SketchId, SketchModelError> {
        if let Some(id) = self.material_by_name(name) {
            return Ok(id);
        }
        self.create_material(name.to_string(), cad_material_color(name))
    }

    pub fn draw_pencil_line(
        &mut self,
        context: SketchId,
        a: Vec3,
        b: Vec3,
    ) -> Result<SketchId, SketchModelError> {
        self.add_entity_with_history(context, SketchEntityKind::Edge { a, b }, "Pencil line")
    }

    pub fn draw_rectangle_face(
        &mut self,
        context: SketchId,
        origin: Vec3,
        axis_u: Vec3,
        axis_v: Vec3,
        label: impl Into<String>,
    ) -> Result<SketchId, SketchModelError> {
        let normal = axis_u.cross(axis_v).try_normalize().unwrap_or(Vec3::Z);
        let vertices = vec![
            origin,
            origin + axis_u,
            origin + axis_u + axis_v,
            origin + axis_v,
        ];
        self.add_entity_with_history(context, SketchEntityKind::Face { vertices, normal }, label)
    }

    pub fn draw_circle_face(
        &mut self,
        context: SketchId,
        center: Vec3,
        normal: Vec3,
        radius: f32,
        segments: usize,
        label: impl Into<String>,
    ) -> Result<SketchId, SketchModelError> {
        let normal = safe_normal(normal);
        let radius = radius.abs().max(PLANAR_GRAPH_MIN_AREA);
        let segments = segments.max(3);
        let vertices = radial_points(center, normal, radius, segments, None);
        self.add_entity_with_history(
            context,
            SketchEntityKind::CircleFace {
                center,
                normal,
                radius,
                segments,
                vertices,
            },
            label,
        )
    }

    pub fn draw_polygon_face(
        &mut self,
        context: SketchId,
        center: Vec3,
        normal: Vec3,
        radius: f32,
        sides: usize,
        label: impl Into<String>,
    ) -> Result<SketchId, SketchModelError> {
        let normal = safe_normal(normal);
        let radius = radius.abs().max(PLANAR_GRAPH_MIN_AREA);
        let sides = sides.max(3);
        let vertices = radial_points(center, normal, radius, sides, None);
        self.add_entity_with_history(
            context,
            SketchEntityKind::PolygonFace {
                center,
                normal,
                radius,
                sides,
                vertices,
            },
            label,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_arc_curve(
        &mut self,
        context: SketchId,
        center: Vec3,
        normal: Vec3,
        radius: f32,
        start_direction: Vec3,
        sweep_radians: f32,
        segments: usize,
        label: impl Into<String>,
    ) -> Result<SketchId, SketchModelError> {
        let normal = safe_normal(normal);
        let radius = radius.abs().max(PLANAR_GRAPH_MIN_AREA);
        let segments = segments.max(1);
        let (axis_u, axis_v, _) = plane_basis(normal, Some(start_direction));
        let points = (0..=segments)
            .map(|index| {
                let t = index as f32 / segments as f32;
                let angle = sweep_radians * t;
                center + (axis_u * angle.cos() + axis_v * angle.sin()) * radius
            })
            .collect();
        self.add_entity_with_history(
            context,
            SketchEntityKind::ArcCurve {
                center,
                normal,
                radius,
                start_direction: axis_u,
                sweep_radians,
                points,
            },
            label,
        )
    }

    pub fn draw_freehand_curve(
        &mut self,
        context: SketchId,
        points: impl IntoIterator<Item = Vec3>,
        label: impl Into<String>,
    ) -> Result<SketchId, SketchModelError> {
        let points: Vec<Vec3> = points.into_iter().collect();
        self.add_entity_with_history(context, SketchEntityKind::FreehandCurve { points }, label)
    }

    pub fn push_pull_face(
        &mut self,
        face: SketchId,
        depth: f32,
    ) -> Result<SketchId, SketchModelError> {
        let (base_vertices, normal) = self.face_geometry(face)?;
        let offset = normal * depth;
        let top_vertices: Vec<_> = base_vertices
            .iter()
            .map(|vertex| *vertex + offset)
            .collect();
        let bounds = SketchBounds::from_points(
            base_vertices
                .iter()
                .copied()
                .chain(top_vertices.iter().copied()),
        )
        .unwrap_or(SketchBounds {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
        });
        self.add_entity_with_history(
            self.active_context,
            SketchEntityKind::PushPullExtrusion {
                source_face: face,
                base_vertices,
                top_vertices,
                normal,
                depth,
                bounds,
            },
            "Push/Pull",
        )
    }

    pub fn record_push_pull_face(
        &mut self,
        context: SketchId,
        origin: Vec3,
        axis_u: Vec3,
        axis_v: Vec3,
        depth: f32,
        label: impl Into<String>,
    ) -> Result<(SketchId, SketchId), SketchModelError> {
        if !self.contexts.contains_key(&context) {
            return Err(SketchModelError::UnknownContext(context));
        }
        let normal = axis_u.cross(axis_v).try_normalize().unwrap_or(Vec3::Z);
        let base_vertices = vec![
            origin,
            origin + axis_u,
            origin + axis_u + axis_v,
            origin + axis_v,
        ];
        let face = self.add_entity(
            context,
            SketchEntityKind::Face {
                vertices: base_vertices.clone(),
                normal,
            },
        )?;
        let offset = normal * depth;
        let top_vertices: Vec<_> = base_vertices
            .iter()
            .map(|vertex| *vertex + offset)
            .collect();
        let bounds = SketchBounds::from_points(
            base_vertices
                .iter()
                .copied()
                .chain(top_vertices.iter().copied()),
        )
        .unwrap_or(SketchBounds {
            min: origin,
            max: origin,
        });
        let extrusion = self.add_entity(
            context,
            SketchEntityKind::PushPullExtrusion {
                source_face: face,
                base_vertices,
                top_vertices,
                normal,
                depth,
                bounds,
            },
        )?;
        self.record_created_entities(label, [(context, face), (context, extrusion)])?;
        Ok((face, extrusion))
    }

    pub fn cut_opening_through_face(
        &mut self,
        host: SketchId,
        center: Vec3,
        size: Vec3,
        through_depth: f32,
    ) -> Result<SketchId, SketchModelError> {
        let (_, normal) = self.face_geometry(host)?;
        let bounds = SketchBounds::from_center_size(center, size).extruded(normal, through_depth);
        self.add_entity_with_history(
            self.active_context,
            SketchEntityKind::Opening {
                host,
                center,
                size,
                normal,
                through_depth,
                bounds,
            },
            "Opening cut",
        )
    }

    pub fn create_hollow_room(
        &mut self,
        shell: SketchId,
        wall_thickness: f32,
        room_depth: f32,
    ) -> Result<SketchId, SketchModelError> {
        let (vertices, normal) = self.face_geometry(shell)?;
        let face_bounds = SketchBounds::from_points(vertices).unwrap_or(SketchBounds {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
        });
        let shell_bounds = face_bounds.extruded(normal, room_depth);
        let interior_bounds = shell_bounds.inset(wall_thickness);
        self.add_entity_with_history(
            self.active_context,
            SketchEntityKind::Room {
                shell,
                shell_bounds,
                interior_bounds,
                wall_thickness,
            },
            "Room hollow",
        )
    }

    pub fn brep_kernel_for_face(
        &self,
        face: SketchId,
    ) -> Result<(SketchBRepKernel, SketchId), SketchModelError> {
        let (vertices, _) = self.face_geometry(face)?;
        let mut kernel = SketchBRepKernel::new();
        let brep_face = kernel.add_face_from_points(vertices)?;
        Ok((kernel, brep_face))
    }

    pub fn entity_inference_candidates(
        &self,
        entity: SketchId,
    ) -> Result<Vec<InferenceCandidate>, SketchModelError> {
        let entity = self
            .entities
            .get(&entity)
            .ok_or(SketchModelError::UnknownEntity(entity))?;
        let mut candidates = Vec::new();
        match &entity.kind {
            SketchEntityKind::Edge { a, b } => {
                let edge_direction = (*b - *a).try_normalize();
                candidates.push(InferenceCandidate::new(
                    InferenceKind::Endpoint,
                    *a,
                    1.0,
                    InferenceKind::Endpoint.tooltip(),
                ));
                candidates.push(InferenceCandidate::new(
                    InferenceKind::Endpoint,
                    *b,
                    1.0,
                    InferenceKind::Endpoint.tooltip(),
                ));
                candidates.push(InferenceCandidate::new(
                    InferenceKind::Midpoint,
                    (*a + *b) * 0.5,
                    0.92,
                    InferenceKind::Midpoint.tooltip(),
                ));
                if let Some(direction) = edge_direction {
                    candidates.push(
                        InferenceCandidate::new(
                            InferenceKind::OnEdge,
                            (*a + *b) * 0.5,
                            0.74,
                            InferenceKind::OnEdge.tooltip(),
                        )
                        .with_direction(direction),
                    );
                }
            }
            SketchEntityKind::Face { vertices, normal } => {
                push_face_inference_candidates(vertices, *normal, &mut candidates);
            }
            SketchEntityKind::CircleFace {
                vertices, normal, ..
            }
            | SketchEntityKind::PolygonFace {
                vertices, normal, ..
            } => {
                push_face_inference_candidates(vertices, *normal, &mut candidates);
            }
            SketchEntityKind::ArcCurve { points, normal, .. } => {
                push_curve_inference_candidates(points, false, Some(*normal), &mut candidates);
            }
            SketchEntityKind::FreehandCurve { points } => {
                push_curve_inference_candidates(points, false, None, &mut candidates);
            }
            _ => {}
        }
        Ok(InferenceService::ranked(candidates))
    }

    pub fn undo_count(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_count(&self) -> usize {
        self.redo_stack.len()
    }

    pub fn undo_last(&mut self) -> Option<SketchEditSummary> {
        let batch = self.undo_stack.pop()?;
        let summary = batch.summary();
        for change in batch.changes.iter().rev() {
            match change {
                SketchEditChange::Created(record) => self.remove_entity_record(record),
                SketchEditChange::Modified {
                    context, before, ..
                } => {
                    self.restore_entity_in_context(*context, before);
                }
            }
        }
        self.redo_stack.push(batch);
        self.undo_generation += 1;
        Some(summary)
    }

    pub fn redo_last(&mut self) -> Option<SketchEditSummary> {
        let batch = self.redo_stack.pop()?;
        let summary = batch.summary();
        for change in &batch.changes {
            match change {
                SketchEditChange::Created(record) => self.restore_entity_record(record),
                SketchEditChange::Modified { context, after, .. } => {
                    self.restore_entity_in_context(*context, after);
                }
            }
        }
        self.undo_stack.push(batch);
        self.undo_generation += 1;
        Some(summary)
    }

    pub fn move_selection(
        &mut self,
        selection: &SelectionSet,
        delta: Vec3,
        label: impl Into<String>,
    ) -> Result<SketchEditSummary, SketchModelError> {
        let label = label.into();
        let mut changes = Vec::with_capacity(selection.len());

        for entity_id in selection.ordered() {
            let context = self.context_for_entity(*entity_id)?;
            let before = self
                .entities
                .get(entity_id)
                .cloned()
                .ok_or(SketchModelError::UnknownEntity(*entity_id))?;
            let mut after = before.clone();
            translate_entity_kind(&mut after.kind, delta);
            changes.push(SketchEditChange::Modified {
                context,
                before,
                after,
            });
        }

        let batch = SketchEditBatch::new(label, changes);
        let summary = batch.summary();
        if summary.entity_count == 0 {
            return Ok(summary);
        }

        for change in &batch.changes {
            if let SketchEditChange::Modified { context, after, .. } = change {
                self.restore_entity_in_context(*context, after);
            }
        }
        self.undo_stack.push(batch);
        self.redo_stack.clear();
        self.undo_generation += 1;
        Ok(summary)
    }

    pub fn scale_selection_about_pivot(
        &mut self,
        selection: &SelectionSet,
        pivot: Vec3,
        scale: Vec3,
        label: impl Into<String>,
    ) -> Result<SketchEditSummary, SketchModelError> {
        if scale.x.abs() <= f32::EPSILON
            || scale.y.abs() <= f32::EPSILON
            || scale.z.abs() <= f32::EPSILON
        {
            return Err(SketchModelError::InvalidGeometry(
                "Scale factors must be non-zero",
            ));
        }
        self.modify_selection(selection, label, |kind| {
            scale_entity_kind(kind, pivot, scale);
        })
    }

    pub fn flip_selection_across_plane(
        &mut self,
        selection: &SelectionSet,
        plane_origin: Vec3,
        plane_normal: Vec3,
        label: impl Into<String>,
    ) -> Result<SketchEditSummary, SketchModelError> {
        let normal = plane_normal
            .try_normalize()
            .ok_or(SketchModelError::InvalidGeometry(
                "Flip plane normal must be non-zero",
            ))?;
        self.modify_selection(selection, label, |kind| {
            flip_entity_kind(kind, plane_origin, normal);
        })
    }

    pub fn copy_selection_linear_array(
        &mut self,
        selection: &SelectionSet,
        delta: Vec3,
        copy_count: usize,
        label: impl Into<String>,
    ) -> Result<Vec<SketchId>, SketchModelError> {
        let originals = self.selected_entity_records(selection)?;
        if originals.is_empty() || copy_count == 0 {
            return Ok(Vec::new());
        }

        let mut created_ids = Vec::with_capacity(originals.len() * copy_count);
        let mut changes = Vec::with_capacity(originals.len() * copy_count);
        for step in 1..=copy_count {
            let mut id_map = BTreeMap::new();
            for record in &originals {
                id_map.insert(record.entity.id, self.allocate_id());
            }

            for record in &originals {
                let mut entity = record.entity.clone();
                let new_id = id_map[&entity.id];
                entity.id = new_id;
                translate_entity_kind(&mut entity.kind, delta * step as f32);
                remap_entity_references(&mut entity.kind, &id_map);
                created_ids.push(new_id);
                changes.push(SketchEditChange::Created(SketchEntityRecord {
                    context: record.context,
                    entity,
                }));
            }
        }

        let batch = SketchEditBatch::new(label, changes);
        for change in &batch.changes {
            if let SketchEditChange::Created(record) = change {
                self.restore_entity_record(record);
            }
        }
        self.undo_stack.push(batch);
        self.redo_stack.clear();
        self.undo_generation += 1;
        Ok(created_ids)
    }

    pub fn reconstruct_planar_faces(
        &mut self,
        context: SketchId,
        normal: Vec3,
    ) -> Result<Vec<SketchId>, SketchModelError> {
        if !self.contexts.contains_key(&context) {
            return Err(SketchModelError::UnknownContext(context));
        }
        let normal = normal.try_normalize().unwrap_or(Vec3::Z);
        let loops = self.detect_planar_face_loops(context, normal)?;
        let mut faces = Vec::with_capacity(loops.len());
        for vertices in loops {
            let face = self.add_entity(context, SketchEntityKind::Face { vertices, normal })?;
            faces.push(face);
        }
        Ok(faces)
    }

    fn add_entity_with_history(
        &mut self,
        context: SketchId,
        kind: SketchEntityKind,
        label: impl Into<String>,
    ) -> Result<SketchId, SketchModelError> {
        let id = self.add_entity(context, kind)?;
        self.record_created_entities(label, [(context, id)])?;
        Ok(id)
    }

    fn record_created_entities(
        &mut self,
        label: impl Into<String>,
        entities: impl IntoIterator<Item = (SketchId, SketchId)>,
    ) -> Result<(), SketchModelError> {
        let mut changes = Vec::new();
        for (context, entity) in entities {
            if !self.contexts.contains_key(&context) {
                return Err(SketchModelError::UnknownContext(context));
            }
            let entity_record = self
                .entities
                .get(&entity)
                .cloned()
                .ok_or(SketchModelError::UnknownEntity(entity))?;
            changes.push(SketchEditChange::Created(SketchEntityRecord {
                context,
                entity: entity_record,
            }));
        }
        self.undo_stack.push(SketchEditBatch::new(label, changes));
        self.redo_stack.clear();
        Ok(())
    }

    fn selected_entity_records(
        &self,
        selection: &SelectionSet,
    ) -> Result<Vec<SketchEntityRecord>, SketchModelError> {
        let mut records = Vec::with_capacity(selection.len());
        for entity_id in selection.ordered() {
            let context = self.context_for_entity(*entity_id)?;
            let entity = self
                .entities
                .get(entity_id)
                .cloned()
                .ok_or(SketchModelError::UnknownEntity(*entity_id))?;
            records.push(SketchEntityRecord { context, entity });
        }
        Ok(records)
    }

    fn modify_selection(
        &mut self,
        selection: &SelectionSet,
        label: impl Into<String>,
        mut edit: impl FnMut(&mut SketchEntityKind),
    ) -> Result<SketchEditSummary, SketchModelError> {
        let label = label.into();
        let mut changes = Vec::with_capacity(selection.len());
        for entity_id in selection.ordered() {
            let context = self.context_for_entity(*entity_id)?;
            let before = self
                .entities
                .get(entity_id)
                .cloned()
                .ok_or(SketchModelError::UnknownEntity(*entity_id))?;
            let mut after = before.clone();
            edit(&mut after.kind);
            changes.push(SketchEditChange::Modified {
                context,
                before,
                after,
            });
        }

        let batch = SketchEditBatch::new(label, changes);
        let summary = batch.summary();
        if summary.entity_count == 0 {
            return Ok(summary);
        }

        for change in &batch.changes {
            if let SketchEditChange::Modified { context, after, .. } = change {
                self.restore_entity_in_context(*context, after);
            }
        }
        self.undo_stack.push(batch);
        self.redo_stack.clear();
        self.undo_generation += 1;
        Ok(summary)
    }

    fn context_for_entity(&self, entity: SketchId) -> Result<SketchId, SketchModelError> {
        if !self.entities.contains_key(&entity) {
            return Err(SketchModelError::UnknownEntity(entity));
        }
        self.contexts
            .iter()
            .find_map(|(context_id, context)| {
                context.entities.contains(&entity).then_some(*context_id)
            })
            .ok_or(SketchModelError::UnknownEntity(entity))
    }

    fn remove_entity_record(&mut self, record: &SketchEntityRecord) {
        self.entities.remove(&record.entity.id);
        if let Some(context) = self.contexts.get_mut(&record.context) {
            context.entities.retain(|id| *id != record.entity.id);
        }
    }

    fn restore_entity_record(&mut self, record: &SketchEntityRecord) {
        self.entities
            .insert(record.entity.id, record.entity.clone());
        if let Some(context) = self.contexts.get_mut(&record.context) {
            if !context.entities.contains(&record.entity.id) {
                context.entities.push(record.entity.id);
            }
        }
    }

    fn restore_entity_in_context(&mut self, context: SketchId, entity: &SketchEntity) {
        self.entities.insert(entity.id, entity.clone());
        if let Some(context) = self.contexts.get_mut(&context) {
            if !context.entities.contains(&entity.id) {
                context.entities.push(entity.id);
            }
        }
    }

    fn face_geometry(&self, face: SketchId) -> Result<(Vec<Vec3>, Vec3), SketchModelError> {
        match self.entities.get(&face) {
            Some(SketchEntity {
                kind: SketchEntityKind::Face { vertices, normal },
                ..
            }) => Ok((vertices.clone(), *normal)),
            Some(SketchEntity {
                kind:
                    SketchEntityKind::CircleFace {
                        vertices, normal, ..
                    }
                    | SketchEntityKind::PolygonFace {
                        vertices, normal, ..
                    },
                ..
            }) => Ok((vertices.clone(), *normal)),
            _ => Err(SketchModelError::UnknownEntity(face)),
        }
    }

    fn detect_planar_face_loops(
        &self,
        context: SketchId,
        normal: Vec3,
    ) -> Result<Vec<Vec<Vec3>>, SketchModelError> {
        let context = self
            .contexts
            .get(&context)
            .ok_or(SketchModelError::UnknownContext(context))?;
        let mut points: BTreeMap<PlanarPointKey, Vec3> = BTreeMap::new();
        let mut adjacency: BTreeMap<PlanarPointKey, BTreeSet<PlanarPointKey>> = BTreeMap::new();
        let mut edges: BTreeSet<(PlanarPointKey, PlanarPointKey)> = BTreeSet::new();

        for entity_id in &context.entities {
            let Some(SketchEntity {
                kind: SketchEntityKind::Edge { a, b },
                visible: true,
                locked: false,
                ..
            }) = self.entities.get(entity_id)
            else {
                continue;
            };
            let a_key = PlanarPointKey::from_vec3(*a);
            let b_key = PlanarPointKey::from_vec3(*b);
            if a_key == b_key {
                continue;
            }
            points.entry(a_key).or_insert(*a);
            points.entry(b_key).or_insert(*b);
            adjacency.entry(a_key).or_default().insert(b_key);
            adjacency.entry(b_key).or_default().insert(a_key);
            edges.insert(normalized_edge(a_key, b_key));
        }

        let mut canonical_cycles: BTreeSet<Vec<PlanarPointKey>> = BTreeSet::new();
        for start in points.keys().copied() {
            let mut path = vec![start];
            collect_planar_cycles(start, start, &adjacency, &mut path, &mut canonical_cycles);
        }

        let mut loops = Vec::new();
        for cycle in canonical_cycles {
            if planar_cycle_has_chord(&cycle, &edges) {
                continue;
            }
            let Some(vertices) = orient_planar_cycle(cycle, &points, normal) else {
                continue;
            };
            loops.push(vertices);
        }
        loops.sort_by(|a, b| {
            planar_loop_area_abs(a, normal)
                .total_cmp(&planar_loop_area_abs(b, normal))
                .then_with(|| a.len().cmp(&b.len()))
        });
        Ok(loops)
    }

    pub fn create_component_definition(
        &mut self,
        name: impl Into<String>,
    ) -> Result<SketchId, SketchModelError> {
        let context_id = self.allocate_id();
        self.contexts
            .insert(context_id, SketchContext::root(context_id));
        let definition_id = self.allocate_id();
        self.definitions.insert(
            definition_id,
            ComponentDefinition {
                id: definition_id,
                name: name.into(),
                context: context_id,
            },
        );
        self.undo_generation += 1;
        Ok(definition_id)
    }

    pub fn add_component_instance(
        &mut self,
        definition: SketchId,
        transform: SketchTransform,
    ) -> Result<SketchId, SketchModelError> {
        if !self.definitions.contains_key(&definition) {
            return Err(SketchModelError::UnknownDefinition(definition));
        }
        self.add_entity_to_active(SketchEntityKind::ComponentInstance {
            definition,
            transform,
        })
    }

    pub fn component_definition_for_instance(&self, instance: SketchId) -> Option<SketchId> {
        match &self.entities.get(&instance)?.kind {
            SketchEntityKind::ComponentInstance { definition, .. } => Some(*definition),
            _ => None,
        }
    }

    pub fn make_unique_instance(
        &mut self,
        instance: SketchId,
    ) -> Result<SketchId, SketchModelError> {
        let old_definition_id = self
            .component_definition_for_instance(instance)
            .ok_or(SketchModelError::NotComponentInstance(instance))?;
        let old_definition = self
            .definitions
            .get(&old_definition_id)
            .cloned()
            .ok_or(SketchModelError::UnknownDefinition(old_definition_id))?;
        let old_entity_ids = self
            .contexts
            .get(&old_definition.context)
            .ok_or(SketchModelError::UnknownContext(old_definition.context))?
            .entities
            .clone();

        let new_context_id = self.allocate_id();
        let mut new_context = SketchContext::root(new_context_id);
        for old_entity_id in old_entity_ids {
            if let Some(old_entity) = self.entities.get(&old_entity_id).cloned() {
                let new_entity_id = self.allocate_id();
                let mut entity = old_entity;
                entity.id = new_entity_id;
                self.entities.insert(new_entity_id, entity);
                new_context.entities.push(new_entity_id);
            }
        }
        self.contexts.insert(new_context_id, new_context);

        let new_definition_id = self.allocate_id();
        self.definitions.insert(
            new_definition_id,
            ComponentDefinition {
                id: new_definition_id,
                name: format!("{} unique", old_definition.name),
                context: new_context_id,
            },
        );

        let entity = self
            .entities
            .get_mut(&instance)
            .ok_or(SketchModelError::UnknownEntity(instance))?;
        match &mut entity.kind {
            SketchEntityKind::ComponentInstance { definition, .. } => {
                *definition = new_definition_id;
            }
            _ => return Err(SketchModelError::NotComponentInstance(instance)),
        }
        self.undo_generation += 1;
        Ok(new_definition_id)
    }

    fn allocate_id(&mut self) -> SketchId {
        let id = SketchId(self.next_id);
        self.next_id += 1;
        id
    }
}

fn snapshot_context(context: &SketchContext) -> SketchContextSnapshot {
    SketchContextSnapshot {
        id: context.id.raw(),
        parent: context.parent.map(SketchId::raw),
        local_to_parent: snapshot_transform(context.local_to_parent),
        entities: context.entities.iter().map(|id| id.raw()).collect(),
    }
}

fn restore_context(snapshot: SketchContextSnapshot) -> SketchContext {
    SketchContext {
        id: SketchId(snapshot.id),
        parent: snapshot.parent.map(SketchId),
        local_to_parent: restore_transform(snapshot.local_to_parent),
        entities: snapshot.entities.into_iter().map(SketchId).collect(),
    }
}

fn snapshot_definition(definition: &ComponentDefinition) -> ComponentDefinitionSnapshot {
    ComponentDefinitionSnapshot {
        id: definition.id.raw(),
        name: definition.name.clone(),
        context: definition.context.raw(),
    }
}

fn restore_definition(snapshot: ComponentDefinitionSnapshot) -> ComponentDefinition {
    ComponentDefinition {
        id: SketchId(snapshot.id),
        name: snapshot.name,
        context: SketchId(snapshot.context),
    }
}

fn snapshot_material(material: &SketchMaterial) -> SketchMaterialSnapshot {
    SketchMaterialSnapshot {
        id: material.id.raw(),
        name: material.name.clone(),
        color: SketchColorSnapshot {
            r: material.color.r,
            g: material.color.g,
            b: material.color.b,
            a: material.color.a,
        },
    }
}

fn restore_material(snapshot: SketchMaterialSnapshot) -> SketchMaterial {
    SketchMaterial {
        id: SketchId(snapshot.id),
        name: snapshot.name,
        color: SketchColor {
            r: snapshot.color.r,
            g: snapshot.color.g,
            b: snapshot.color.b,
            a: snapshot.color.a,
        },
    }
}

fn snapshot_tag(tag: &SketchTag) -> SketchTagSnapshot {
    SketchTagSnapshot {
        id: tag.id.raw(),
        name: tag.name.clone(),
        visible: tag.visible,
    }
}

fn restore_tag(snapshot: SketchTagSnapshot) -> SketchTag {
    SketchTag {
        id: SketchId(snapshot.id),
        name: snapshot.name,
        visible: snapshot.visible,
    }
}

fn snapshot_style(style: &SketchStyle) -> SketchStyleSnapshot {
    SketchStyleSnapshot {
        id: style.id.raw(),
        name: style.name.clone(),
    }
}

fn restore_style(snapshot: SketchStyleSnapshot) -> SketchStyle {
    SketchStyle {
        id: SketchId(snapshot.id),
        name: snapshot.name,
    }
}

fn snapshot_scene(scene: &SketchScene) -> SketchSceneSnapshot {
    SketchSceneSnapshot {
        id: scene.id.raw(),
        name: scene.name.clone(),
        camera: scene.camera.as_ref().map(snapshot_camera),
        style: scene.style.map(SketchId::raw),
        visible_tags: scene
            .visible_tags
            .iter()
            .map(|(tag, visible)| (tag.raw(), *visible))
            .collect(),
    }
}

fn restore_scene(snapshot: SketchSceneSnapshot) -> SketchScene {
    SketchScene {
        id: SketchId(snapshot.id),
        name: snapshot.name,
        camera: snapshot.camera.map(restore_camera),
        style: snapshot.style.map(SketchId),
        visible_tags: snapshot
            .visible_tags
            .into_iter()
            .map(|(tag, visible)| (SketchId(tag), visible))
            .collect(),
    }
}

fn snapshot_camera(camera: &SketchCamera) -> SketchCameraSnapshot {
    SketchCameraSnapshot {
        eye: snapshot_vec3(camera.eye),
        target: snapshot_vec3(camera.target),
        up: snapshot_vec3(camera.up),
    }
}

fn restore_camera(snapshot: SketchCameraSnapshot) -> SketchCamera {
    SketchCamera {
        eye: restore_vec3(snapshot.eye),
        target: restore_vec3(snapshot.target),
        up: restore_vec3(snapshot.up),
    }
}

fn snapshot_entity(entity: &SketchEntity) -> SketchEntitySnapshot {
    SketchEntitySnapshot {
        id: entity.id.raw(),
        kind: snapshot_entity_kind(&entity.kind),
        visible: entity.visible,
        locked: entity.locked,
        material: entity.material.map(SketchId::raw),
        tag: entity.tag.map(SketchId::raw),
        attributes: entity.attributes.clone(),
    }
}

fn restore_entity(snapshot: SketchEntitySnapshot) -> SketchEntity {
    SketchEntity {
        id: SketchId(snapshot.id),
        kind: restore_entity_kind(snapshot.kind),
        visible: snapshot.visible,
        locked: snapshot.locked,
        material: snapshot.material.map(SketchId),
        tag: snapshot.tag.map(SketchId),
        attributes: snapshot.attributes,
    }
}

fn snapshot_entity_kind(kind: &SketchEntityKind) -> SketchEntityKindSnapshot {
    match kind {
        SketchEntityKind::Vertex { point } => SketchEntityKindSnapshot::Vertex {
            point: snapshot_vec3(*point),
        },
        SketchEntityKind::Edge { a, b } => SketchEntityKindSnapshot::Edge {
            a: snapshot_vec3(*a),
            b: snapshot_vec3(*b),
        },
        SketchEntityKind::Face { vertices, normal } => SketchEntityKindSnapshot::Face {
            vertices: vertices.iter().copied().map(snapshot_vec3).collect(),
            normal: snapshot_vec3(*normal),
        },
        SketchEntityKind::CircleFace {
            center,
            normal,
            radius,
            segments,
            vertices,
        } => SketchEntityKindSnapshot::CircleFace {
            center: snapshot_vec3(*center),
            normal: snapshot_vec3(*normal),
            radius: *radius,
            segments: *segments,
            vertices: vertices.iter().copied().map(snapshot_vec3).collect(),
        },
        SketchEntityKind::PolygonFace {
            center,
            normal,
            radius,
            sides,
            vertices,
        } => SketchEntityKindSnapshot::PolygonFace {
            center: snapshot_vec3(*center),
            normal: snapshot_vec3(*normal),
            radius: *radius,
            sides: *sides,
            vertices: vertices.iter().copied().map(snapshot_vec3).collect(),
        },
        SketchEntityKind::ArcCurve {
            center,
            normal,
            radius,
            start_direction,
            sweep_radians,
            points,
        } => SketchEntityKindSnapshot::ArcCurve {
            center: snapshot_vec3(*center),
            normal: snapshot_vec3(*normal),
            radius: *radius,
            start_direction: snapshot_vec3(*start_direction),
            sweep_radians: *sweep_radians,
            points: points.iter().copied().map(snapshot_vec3).collect(),
        },
        SketchEntityKind::FreehandCurve { points } => SketchEntityKindSnapshot::FreehandCurve {
            points: points.iter().copied().map(snapshot_vec3).collect(),
        },
        SketchEntityKind::PushPullExtrusion {
            source_face,
            base_vertices,
            top_vertices,
            normal,
            depth,
            bounds,
        } => SketchEntityKindSnapshot::PushPullExtrusion {
            source_face: source_face.raw(),
            base_vertices: base_vertices.iter().copied().map(snapshot_vec3).collect(),
            top_vertices: top_vertices.iter().copied().map(snapshot_vec3).collect(),
            normal: snapshot_vec3(*normal),
            depth: *depth,
            bounds: snapshot_bounds(*bounds),
        },
        SketchEntityKind::Opening {
            host,
            center,
            size,
            normal,
            through_depth,
            bounds,
        } => SketchEntityKindSnapshot::Opening {
            host: host.raw(),
            center: snapshot_vec3(*center),
            size: snapshot_vec3(*size),
            normal: snapshot_vec3(*normal),
            through_depth: *through_depth,
            bounds: snapshot_bounds(*bounds),
        },
        SketchEntityKind::Room {
            shell,
            shell_bounds,
            interior_bounds,
            wall_thickness,
        } => SketchEntityKindSnapshot::Room {
            shell: shell.raw(),
            shell_bounds: snapshot_bounds(*shell_bounds),
            interior_bounds: snapshot_bounds(*interior_bounds),
            wall_thickness: *wall_thickness,
        },
        SketchEntityKind::Group { context } => SketchEntityKindSnapshot::Group {
            context: context.raw(),
        },
        SketchEntityKind::ComponentInstance {
            definition,
            transform,
        } => SketchEntityKindSnapshot::ComponentInstance {
            definition: definition.raw(),
            transform: snapshot_transform(*transform),
        },
        SketchEntityKind::GuidePoint { point } => SketchEntityKindSnapshot::GuidePoint {
            point: snapshot_vec3(*point),
        },
        SketchEntityKind::GuideLine { origin, direction } => SketchEntityKindSnapshot::GuideLine {
            origin: snapshot_vec3(*origin),
            direction: snapshot_vec3(*direction),
        },
        SketchEntityKind::SectionPlane { origin, normal } => {
            SketchEntityKindSnapshot::SectionPlane {
                origin: snapshot_vec3(*origin),
                normal: snapshot_vec3(*normal),
            }
        }
    }
}

fn restore_entity_kind(snapshot: SketchEntityKindSnapshot) -> SketchEntityKind {
    match snapshot {
        SketchEntityKindSnapshot::Vertex { point } => SketchEntityKind::Vertex {
            point: restore_vec3(point),
        },
        SketchEntityKindSnapshot::Edge { a, b } => SketchEntityKind::Edge {
            a: restore_vec3(a),
            b: restore_vec3(b),
        },
        SketchEntityKindSnapshot::Face { vertices, normal } => SketchEntityKind::Face {
            vertices: vertices.into_iter().map(restore_vec3).collect(),
            normal: restore_vec3(normal),
        },
        SketchEntityKindSnapshot::CircleFace {
            center,
            normal,
            radius,
            segments,
            vertices,
        } => SketchEntityKind::CircleFace {
            center: restore_vec3(center),
            normal: restore_vec3(normal),
            radius,
            segments,
            vertices: vertices.into_iter().map(restore_vec3).collect(),
        },
        SketchEntityKindSnapshot::PolygonFace {
            center,
            normal,
            radius,
            sides,
            vertices,
        } => SketchEntityKind::PolygonFace {
            center: restore_vec3(center),
            normal: restore_vec3(normal),
            radius,
            sides,
            vertices: vertices.into_iter().map(restore_vec3).collect(),
        },
        SketchEntityKindSnapshot::ArcCurve {
            center,
            normal,
            radius,
            start_direction,
            sweep_radians,
            points,
        } => SketchEntityKind::ArcCurve {
            center: restore_vec3(center),
            normal: restore_vec3(normal),
            radius,
            start_direction: restore_vec3(start_direction),
            sweep_radians,
            points: points.into_iter().map(restore_vec3).collect(),
        },
        SketchEntityKindSnapshot::FreehandCurve { points } => SketchEntityKind::FreehandCurve {
            points: points.into_iter().map(restore_vec3).collect(),
        },
        SketchEntityKindSnapshot::PushPullExtrusion {
            source_face,
            base_vertices,
            top_vertices,
            normal,
            depth,
            bounds,
        } => SketchEntityKind::PushPullExtrusion {
            source_face: SketchId(source_face),
            base_vertices: base_vertices.into_iter().map(restore_vec3).collect(),
            top_vertices: top_vertices.into_iter().map(restore_vec3).collect(),
            normal: restore_vec3(normal),
            depth,
            bounds: restore_bounds(bounds),
        },
        SketchEntityKindSnapshot::Opening {
            host,
            center,
            size,
            normal,
            through_depth,
            bounds,
        } => SketchEntityKind::Opening {
            host: SketchId(host),
            center: restore_vec3(center),
            size: restore_vec3(size),
            normal: restore_vec3(normal),
            through_depth,
            bounds: restore_bounds(bounds),
        },
        SketchEntityKindSnapshot::Room {
            shell,
            shell_bounds,
            interior_bounds,
            wall_thickness,
        } => SketchEntityKind::Room {
            shell: SketchId(shell),
            shell_bounds: restore_bounds(shell_bounds),
            interior_bounds: restore_bounds(interior_bounds),
            wall_thickness,
        },
        SketchEntityKindSnapshot::Group { context } => SketchEntityKind::Group {
            context: SketchId(context),
        },
        SketchEntityKindSnapshot::ComponentInstance {
            definition,
            transform,
        } => SketchEntityKind::ComponentInstance {
            definition: SketchId(definition),
            transform: restore_transform(transform),
        },
        SketchEntityKindSnapshot::GuidePoint { point } => SketchEntityKind::GuidePoint {
            point: restore_vec3(point),
        },
        SketchEntityKindSnapshot::GuideLine { origin, direction } => SketchEntityKind::GuideLine {
            origin: restore_vec3(origin),
            direction: restore_vec3(direction),
        },
        SketchEntityKindSnapshot::SectionPlane { origin, normal } => {
            SketchEntityKind::SectionPlane {
                origin: restore_vec3(origin),
                normal: restore_vec3(normal),
            }
        }
    }
}

fn snapshot_transform(transform: SketchTransform) -> SketchTransformSnapshot {
    SketchTransformSnapshot {
        translation: snapshot_vec3(transform.translation),
        rotation: [
            transform.rotation.x,
            transform.rotation.y,
            transform.rotation.z,
            transform.rotation.w,
        ],
        scale: snapshot_vec3(transform.scale),
    }
}

fn restore_transform(snapshot: SketchTransformSnapshot) -> SketchTransform {
    SketchTransform {
        translation: restore_vec3(snapshot.translation),
        rotation: Quat::from_xyzw(
            snapshot.rotation[0],
            snapshot.rotation[1],
            snapshot.rotation[2],
            snapshot.rotation[3],
        ),
        scale: restore_vec3(snapshot.scale),
    }
}

fn snapshot_bounds(bounds: SketchBounds) -> SketchBoundsSnapshot {
    SketchBoundsSnapshot {
        min: snapshot_vec3(bounds.min),
        max: snapshot_vec3(bounds.max),
    }
}

fn restore_bounds(snapshot: SketchBoundsSnapshot) -> SketchBounds {
    SketchBounds {
        min: restore_vec3(snapshot.min),
        max: restore_vec3(snapshot.max),
    }
}

fn translate_entity_kind(kind: &mut SketchEntityKind, delta: Vec3) {
    match kind {
        SketchEntityKind::Vertex { point } => *point += delta,
        SketchEntityKind::Edge { a, b } => {
            *a += delta;
            *b += delta;
        }
        SketchEntityKind::Face { vertices, .. } => {
            translate_points(vertices, delta);
        }
        SketchEntityKind::CircleFace {
            center, vertices, ..
        }
        | SketchEntityKind::PolygonFace {
            center, vertices, ..
        } => {
            *center += delta;
            translate_points(vertices, delta);
        }
        SketchEntityKind::ArcCurve { center, points, .. } => {
            *center += delta;
            translate_points(points, delta);
        }
        SketchEntityKind::FreehandCurve { points } => {
            translate_points(points, delta);
        }
        SketchEntityKind::PushPullExtrusion {
            base_vertices,
            top_vertices,
            bounds,
            ..
        } => {
            translate_points(base_vertices, delta);
            translate_points(top_vertices, delta);
            *bounds = bounds.translated(delta);
        }
        SketchEntityKind::Opening { center, bounds, .. } => {
            *center += delta;
            *bounds = bounds.translated(delta);
        }
        SketchEntityKind::Room {
            shell_bounds,
            interior_bounds,
            ..
        } => {
            *shell_bounds = shell_bounds.translated(delta);
            *interior_bounds = interior_bounds.translated(delta);
        }
        SketchEntityKind::Group { .. } => {}
        SketchEntityKind::ComponentInstance { transform, .. } => {
            transform.translation += delta;
        }
        SketchEntityKind::GuidePoint { point } => *point += delta,
        SketchEntityKind::GuideLine { origin, .. } => *origin += delta,
        SketchEntityKind::SectionPlane { origin, .. } => *origin += delta,
    }
}

fn translate_points(points: &mut [Vec3], delta: Vec3) {
    for point in points {
        *point += delta;
    }
}

fn scale_entity_kind(kind: &mut SketchEntityKind, pivot: Vec3, scale: Vec3) {
    match kind {
        SketchEntityKind::Vertex { point } => {
            *point = scale_point_about_pivot(*point, pivot, scale)
        }
        SketchEntityKind::Edge { a, b } => {
            *a = scale_point_about_pivot(*a, pivot, scale);
            *b = scale_point_about_pivot(*b, pivot, scale);
        }
        SketchEntityKind::Face { vertices, normal } => {
            scale_points(vertices, pivot, scale);
            *normal = scaled_normal(*normal, scale);
            recompute_face_normal(vertices, normal);
        }
        SketchEntityKind::CircleFace {
            center,
            normal,
            radius,
            vertices,
            ..
        }
        | SketchEntityKind::PolygonFace {
            center,
            normal,
            radius,
            vertices,
            ..
        } => {
            *center = scale_point_about_pivot(*center, pivot, scale);
            scale_points(vertices, pivot, scale);
            *normal = scaled_normal(*normal, scale);
            recompute_face_normal(vertices, normal);
            *radius *= dominant_abs_scale(scale);
        }
        SketchEntityKind::ArcCurve {
            center,
            normal,
            radius,
            start_direction,
            points,
            ..
        } => {
            *center = scale_point_about_pivot(*center, pivot, scale);
            scale_points(points, pivot, scale);
            *normal = scaled_normal(*normal, scale);
            *start_direction = scaled_direction(*start_direction, scale);
            *radius *= dominant_abs_scale(scale);
        }
        SketchEntityKind::FreehandCurve { points } => {
            scale_points(points, pivot, scale);
        }
        SketchEntityKind::PushPullExtrusion {
            base_vertices,
            top_vertices,
            normal,
            depth,
            bounds,
            ..
        } => {
            scale_points(base_vertices, pivot, scale);
            scale_points(top_vertices, pivot, scale);
            *normal = scaled_normal(*normal, scale);
            *depth *= scale_along_direction(*normal, scale).abs();
            *bounds = bounds.transformed(|point| scale_point_about_pivot(point, pivot, scale));
        }
        SketchEntityKind::Opening {
            center,
            size,
            normal,
            bounds,
            ..
        } => {
            *center = scale_point_about_pivot(*center, pivot, scale);
            *size *= scale.abs();
            *normal = scaled_normal(*normal, scale);
            *bounds = bounds.transformed(|point| scale_point_about_pivot(point, pivot, scale));
        }
        SketchEntityKind::Room {
            shell_bounds,
            interior_bounds,
            wall_thickness,
            ..
        } => {
            *shell_bounds =
                shell_bounds.transformed(|point| scale_point_about_pivot(point, pivot, scale));
            *interior_bounds =
                interior_bounds.transformed(|point| scale_point_about_pivot(point, pivot, scale));
            *wall_thickness *= scale.min_element().abs();
        }
        SketchEntityKind::Group { .. } => {}
        SketchEntityKind::ComponentInstance { transform, .. } => {
            transform.translation = scale_point_about_pivot(transform.translation, pivot, scale);
            transform.scale *= scale;
        }
        SketchEntityKind::GuidePoint { point } => {
            *point = scale_point_about_pivot(*point, pivot, scale);
        }
        SketchEntityKind::GuideLine { origin, direction } => {
            *origin = scale_point_about_pivot(*origin, pivot, scale);
            *direction = scaled_direction(*direction, scale);
        }
        SketchEntityKind::SectionPlane { origin, normal } => {
            *origin = scale_point_about_pivot(*origin, pivot, scale);
            *normal = scaled_normal(*normal, scale);
        }
    }
}

fn flip_entity_kind(kind: &mut SketchEntityKind, plane_origin: Vec3, plane_normal: Vec3) {
    match kind {
        SketchEntityKind::Vertex { point } => {
            *point = reflect_point_across_plane(*point, plane_origin, plane_normal);
        }
        SketchEntityKind::Edge { a, b } => {
            *a = reflect_point_across_plane(*a, plane_origin, plane_normal);
            *b = reflect_point_across_plane(*b, plane_origin, plane_normal);
        }
        SketchEntityKind::Face { vertices, normal } => {
            reflect_points(vertices, plane_origin, plane_normal);
            vertices.reverse();
            *normal = reflect_direction_across_plane(*normal, plane_normal);
            recompute_face_normal(vertices, normal);
        }
        SketchEntityKind::CircleFace {
            center,
            normal,
            vertices,
            ..
        }
        | SketchEntityKind::PolygonFace {
            center,
            normal,
            vertices,
            ..
        } => {
            *center = reflect_point_across_plane(*center, plane_origin, plane_normal);
            reflect_points(vertices, plane_origin, plane_normal);
            vertices.reverse();
            *normal = reflect_direction_across_plane(*normal, plane_normal);
            recompute_face_normal(vertices, normal);
        }
        SketchEntityKind::ArcCurve {
            center,
            normal,
            start_direction,
            points,
            ..
        } => {
            *center = reflect_point_across_plane(*center, plane_origin, plane_normal);
            reflect_points(points, plane_origin, plane_normal);
            *normal = reflect_direction_across_plane(*normal, plane_normal);
            *start_direction = reflect_direction_across_plane(*start_direction, plane_normal);
        }
        SketchEntityKind::FreehandCurve { points } => {
            reflect_points(points, plane_origin, plane_normal);
        }
        SketchEntityKind::PushPullExtrusion {
            base_vertices,
            top_vertices,
            normal,
            bounds,
            ..
        } => {
            reflect_points(base_vertices, plane_origin, plane_normal);
            reflect_points(top_vertices, plane_origin, plane_normal);
            base_vertices.reverse();
            top_vertices.reverse();
            *normal = reflect_direction_across_plane(*normal, plane_normal);
            *bounds = bounds
                .transformed(|point| reflect_point_across_plane(point, plane_origin, plane_normal));
        }
        SketchEntityKind::Opening {
            center,
            normal,
            bounds,
            ..
        } => {
            *center = reflect_point_across_plane(*center, plane_origin, plane_normal);
            *normal = reflect_direction_across_plane(*normal, plane_normal);
            *bounds = bounds
                .transformed(|point| reflect_point_across_plane(point, plane_origin, plane_normal));
        }
        SketchEntityKind::Room {
            shell_bounds,
            interior_bounds,
            ..
        } => {
            *shell_bounds = shell_bounds
                .transformed(|point| reflect_point_across_plane(point, plane_origin, plane_normal));
            *interior_bounds = interior_bounds
                .transformed(|point| reflect_point_across_plane(point, plane_origin, plane_normal));
        }
        SketchEntityKind::Group { .. } => {}
        SketchEntityKind::ComponentInstance { transform, .. } => {
            transform.translation =
                reflect_point_across_plane(transform.translation, plane_origin, plane_normal);
            let axis = dominant_axis(plane_normal);
            let reflected_scale = -component_by_index_vec3(transform.scale, axis);
            set_component_by_index_vec3(&mut transform.scale, axis, reflected_scale);
        }
        SketchEntityKind::GuidePoint { point } => {
            *point = reflect_point_across_plane(*point, plane_origin, plane_normal);
        }
        SketchEntityKind::GuideLine { origin, direction } => {
            *origin = reflect_point_across_plane(*origin, plane_origin, plane_normal);
            *direction = reflect_direction_across_plane(*direction, plane_normal);
        }
        SketchEntityKind::SectionPlane { origin, normal } => {
            *origin = reflect_point_across_plane(*origin, plane_origin, plane_normal);
            *normal = reflect_direction_across_plane(*normal, plane_normal);
        }
    }
}

fn scale_points(points: &mut [Vec3], pivot: Vec3, scale: Vec3) {
    for point in points {
        *point = scale_point_about_pivot(*point, pivot, scale);
    }
}

fn reflect_points(points: &mut [Vec3], plane_origin: Vec3, plane_normal: Vec3) {
    for point in points {
        *point = reflect_point_across_plane(*point, plane_origin, plane_normal);
    }
}

fn scale_point_about_pivot(point: Vec3, pivot: Vec3, scale: Vec3) -> Vec3 {
    pivot + (point - pivot) * scale
}

fn reflect_point_across_plane(point: Vec3, plane_origin: Vec3, plane_normal: Vec3) -> Vec3 {
    point - plane_normal * (2.0 * (point - plane_origin).dot(plane_normal))
}

fn reflect_direction_across_plane(direction: Vec3, plane_normal: Vec3) -> Vec3 {
    safe_normal(direction - plane_normal * (2.0 * direction.dot(plane_normal)))
}

fn scaled_direction(direction: Vec3, scale: Vec3) -> Vec3 {
    safe_normal(direction * scale)
}

fn scaled_normal(normal: Vec3, scale: Vec3) -> Vec3 {
    safe_normal(Vec3::new(
        normal.x / scale.x,
        normal.y / scale.y,
        normal.z / scale.z,
    ))
}

fn scale_along_direction(direction: Vec3, scale: Vec3) -> f32 {
    let direction = safe_normal(direction).abs();
    direction.dot(scale.abs())
}

fn dominant_abs_scale(scale: Vec3) -> f32 {
    scale.x.abs().max(scale.y.abs()).max(scale.z.abs())
}

fn recompute_face_normal(vertices: &[Vec3], normal: &mut Vec3) {
    if let Some(recomputed) = cad_polygon_normal(vertices) {
        *normal = recomputed;
    } else {
        *normal = safe_normal(*normal);
    }
}

fn remap_entity_references(kind: &mut SketchEntityKind, id_map: &BTreeMap<SketchId, SketchId>) {
    match kind {
        SketchEntityKind::PushPullExtrusion { source_face, .. } => {
            if let Some(mapped) = id_map.get(source_face) {
                *source_face = *mapped;
            }
        }
        SketchEntityKind::Opening { host, .. } => {
            if let Some(mapped) = id_map.get(host) {
                *host = *mapped;
            }
        }
        SketchEntityKind::Room { shell, .. } => {
            if let Some(mapped) = id_map.get(shell) {
                *shell = *mapped;
            }
        }
        _ => {}
    }
}

fn snapshot_vec3(value: Vec3) -> [f32; 3] {
    [value.x, value.y, value.z]
}

fn restore_vec3(snapshot: [f32; 3]) -> Vec3 {
    Vec3::new(snapshot[0], snapshot[1], snapshot[2])
}

fn component_by_index_vec3(v: Vec3, axis: usize) -> f32 {
    match axis {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

fn set_component_by_index_vec3(v: &mut Vec3, axis: usize, value: f32) {
    match axis {
        0 => v.x = value,
        1 => v.y = value,
        _ => v.z = value,
    }
}

fn normalized_edge(a: PlanarPointKey, b: PlanarPointKey) -> (PlanarPointKey, PlanarPointKey) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn collect_planar_cycles(
    start: PlanarPointKey,
    current: PlanarPointKey,
    adjacency: &BTreeMap<PlanarPointKey, BTreeSet<PlanarPointKey>>,
    path: &mut Vec<PlanarPointKey>,
    cycles: &mut BTreeSet<Vec<PlanarPointKey>>,
) {
    let Some(neighbours) = adjacency.get(&current) else {
        return;
    };
    for next in neighbours {
        if *next == start && path.len() >= 3 {
            cycles.insert(canonical_cycle(path));
            continue;
        }
        if path.len() >= PLANAR_GRAPH_MAX_LOOP_VERTICES || path.contains(next) {
            continue;
        }
        path.push(*next);
        collect_planar_cycles(start, *next, adjacency, path, cycles);
        path.pop();
    }
}

fn canonical_cycle(path: &[PlanarPointKey]) -> Vec<PlanarPointKey> {
    let forward = minimal_cycle_rotation(path.iter().copied().collect());
    let reverse = minimal_cycle_rotation(path.iter().rev().copied().collect());
    forward.min(reverse)
}

fn minimal_cycle_rotation(cycle: Vec<PlanarPointKey>) -> Vec<PlanarPointKey> {
    let mut best = cycle.clone();
    for shift in 1..cycle.len() {
        let rotated: Vec<_> = cycle[shift..]
            .iter()
            .chain(cycle[..shift].iter())
            .copied()
            .collect();
        if rotated < best {
            best = rotated;
        }
    }
    best
}

fn planar_cycle_has_chord(
    cycle: &[PlanarPointKey],
    edges: &BTreeSet<(PlanarPointKey, PlanarPointKey)>,
) -> bool {
    for i in 0..cycle.len() {
        for j in (i + 1)..cycle.len() {
            if j == i + 1 || (i == 0 && j == cycle.len() - 1) {
                continue;
            }
            if edges.contains(&normalized_edge(cycle[i], cycle[j])) {
                return true;
            }
        }
    }
    false
}

fn orient_planar_cycle(
    cycle: Vec<PlanarPointKey>,
    points: &BTreeMap<PlanarPointKey, Vec3>,
    normal: Vec3,
) -> Option<Vec<Vec3>> {
    let mut vertices: Vec<Vec3> = cycle
        .into_iter()
        .map(|key| points.get(&key).copied())
        .collect::<Option<Vec<_>>>()?;
    let signed_area = planar_loop_signed_area(&vertices, normal);
    if signed_area.abs() < PLANAR_GRAPH_MIN_AREA {
        return None;
    }
    if signed_area < 0.0 {
        vertices.reverse();
    }
    Some(vertices)
}

fn planar_loop_area_abs(vertices: &[Vec3], normal: Vec3) -> f32 {
    planar_loop_signed_area(vertices, normal).abs()
}

fn planar_loop_signed_area(vertices: &[Vec3], normal: Vec3) -> f32 {
    let drop_axis = dominant_axis(normal);
    let mut sum = 0.0;
    for i in 0..vertices.len() {
        let a = project_planar_point(vertices[i], drop_axis);
        let b = project_planar_point(vertices[(i + 1) % vertices.len()], drop_axis);
        sum += a.x * b.y - b.x * a.y;
    }
    sum * 0.5
}

fn dominant_axis(normal: Vec3) -> usize {
    let abs = normal.abs();
    if abs.x >= abs.y && abs.x >= abs.z {
        0
    } else if abs.y >= abs.z {
        1
    } else {
        2
    }
}

fn project_planar_point(point: Vec3, drop_axis: usize) -> Vec3 {
    match drop_axis {
        0 => Vec3::new(point.y, point.z, 0.0),
        1 => Vec3::new(point.x, point.z, 0.0),
        _ => Vec3::new(point.x, point.y, 0.0),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionSet {
    ordered: Vec<SketchId>,
}

impl SelectionSet {
    pub fn select(&mut self, id: SketchId) {
        if !self.ordered.contains(&id) {
            self.ordered.push(id);
        }
    }

    pub fn clear(&mut self) {
        self.ordered.clear();
    }

    pub fn contains(&self, id: SketchId) -> bool {
        self.ordered.contains(&id)
    }

    pub fn len(&self) -> usize {
        self.ordered.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    pub fn ordered(&self) -> &[SketchId] {
        &self.ordered
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    Vertex,
    Edge,
    Face,
    Instance,
    Guide,
    SectionPlane,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HitRecord {
    pub entity: SketchId,
    pub instance_path: Vec<SketchId>,
    pub kind: HitKind,
    pub world_point: Vec3,
    pub distance: f32,
    pub normal: Option<Vec3>,
}

impl HitRecord {
    pub fn new(
        entity: SketchId,
        instance_path: impl IntoIterator<Item = SketchId>,
        kind: HitKind,
        world_point: Vec3,
        distance: f32,
    ) -> Self {
        Self {
            entity,
            instance_path: instance_path.into_iter().collect(),
            kind,
            world_point,
            distance,
            normal: None,
        }
    }

    pub fn with_normal(mut self, normal: Vec3) -> Self {
        self.normal = Some(normal);
        self
    }
}

#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct SemanticHoverHit(pub Option<HitRecord>);

#[derive(Resource, Debug, Clone, Default)]
pub struct PickService;

impl PickService {
    pub fn best(hits: impl IntoIterator<Item = HitRecord>) -> Option<HitRecord> {
        let mut ranked: Vec<_> = hits.into_iter().collect();
        Self::rank(&mut ranked);
        ranked.into_iter().next()
    }

    pub fn ranked(hits: impl IntoIterator<Item = HitRecord>) -> Vec<HitRecord> {
        let mut ranked: Vec<_> = hits.into_iter().collect();
        Self::rank(&mut ranked);
        ranked
    }

    fn rank(hits: &mut [HitRecord]) {
        hits.sort_by(|a, b| {
            a.distance
                .total_cmp(&b.distance)
                .then_with(|| hit_kind_pick_order(a.kind).cmp(&hit_kind_pick_order(b.kind)))
                .then_with(|| a.entity.raw().cmp(&b.entity.raw()))
        });
    }
}

fn hit_kind_pick_order(kind: HitKind) -> u8 {
    match kind {
        HitKind::Vertex => 0,
        HitKind::Edge => 1,
        HitKind::Face => 2,
        HitKind::Instance => 3,
        HitKind::Guide => 4,
        HitKind::SectionPlane => 5,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SketchRay {
    pub origin: Vec3,
    pub direction: Vec3,
}

impl SketchRay {
    pub fn new(origin: Vec3, direction: Vec3) -> Option<Self> {
        Some(Self {
            origin,
            direction: direction.try_normalize()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenProjection {
    pub screen: Vec2,
    pub ndc: Vec3,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenSpaceSnapSettings {
    pub radius_pixels: f32,
    pub sticky_bonus: f32,
}

impl Default for ScreenSpaceSnapSettings {
    fn default() -> Self {
        Self {
            radius_pixels: 15.0,
            sticky_bonus: 0.22,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenSpaceInferenceCandidate {
    pub inference: InferenceCandidate,
    pub screen_point: Vec2,
    pub screen_distance: f32,
    pub depth: f32,
    pub sticky: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectangleDrawingPlane {
    pub origin: Vec3,
    pub normal: Vec3,
    pub axis_u: Vec3,
    pub axis_v: Vec3,
}

pub fn project_world_to_screen(
    world_point: Vec3,
    view_projection: Mat4,
    viewport_size: Vec2,
) -> Option<ScreenProjection> {
    if viewport_size.x <= 0.0 || viewport_size.y <= 0.0 {
        return None;
    }
    let clip: Vec4 = view_projection * world_point.extend(1.0);
    if clip.w <= 1.0e-6 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    if !ndc.is_finite() {
        return None;
    }
    let screen = Vec2::new(
        (ndc.x * 0.5 + 0.5) * viewport_size.x,
        (0.5 - ndc.y * 0.5) * viewport_size.y,
    );
    Some(ScreenProjection { screen, ndc })
}

pub fn screen_space_inference_candidates(
    candidates: impl IntoIterator<Item = InferenceCandidate>,
    cursor_screen: Vec2,
    view_projection: Mat4,
    viewport_size: Vec2,
    settings: ScreenSpaceSnapSettings,
    sticky_kind: Option<InferenceKind>,
) -> Vec<ScreenSpaceInferenceCandidate> {
    let radius = settings.radius_pixels.max(1.0);
    let mut projected: Vec<_> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let projection =
                project_world_to_screen(candidate.point, view_projection, viewport_size)?;
            let screen_distance = projection.screen.distance(cursor_screen);
            (screen_distance <= radius).then(|| ScreenSpaceInferenceCandidate {
                sticky: sticky_kind == Some(candidate.kind),
                inference: candidate,
                screen_point: projection.screen,
                screen_distance,
                depth: projection.ndc.z,
            })
        })
        .collect();
    rank_screen_space_candidates(&mut projected, radius, settings.sticky_bonus);
    projected
}

pub fn best_screen_space_inference(
    candidates: impl IntoIterator<Item = InferenceCandidate>,
    cursor_screen: Vec2,
    view_projection: Mat4,
    viewport_size: Vec2,
    settings: ScreenSpaceSnapSettings,
    sticky_kind: Option<InferenceKind>,
) -> Option<ScreenSpaceInferenceCandidate> {
    screen_space_inference_candidates(
        candidates,
        cursor_screen,
        view_projection,
        viewport_size,
        settings,
        sticky_kind,
    )
    .into_iter()
    .next()
}

fn rank_screen_space_candidates(
    candidates: &mut [ScreenSpaceInferenceCandidate],
    radius_pixels: f32,
    sticky_bonus: f32,
) {
    candidates.sort_by(|a, b| {
        let a_score = screen_space_snap_score(a, radius_pixels, sticky_bonus);
        let b_score = screen_space_snap_score(b, radius_pixels, sticky_bonus);
        a_score
            .total_cmp(&b_score)
            .then_with(|| a.depth.total_cmp(&b.depth))
            .then_with(|| {
                stable_kind_order(a.inference.kind).cmp(&stable_kind_order(b.inference.kind))
            })
    });
}

fn screen_space_snap_score(
    candidate: &ScreenSpaceInferenceCandidate,
    radius_pixels: f32,
    sticky_bonus: f32,
) -> f32 {
    let distance_score = (candidate.screen_distance / radius_pixels.max(1.0)).clamp(0.0, 1.0);
    let kind_score = match candidate.inference.kind {
        InferenceKind::Endpoint => 0.0,
        InferenceKind::Midpoint => 0.16,
        InferenceKind::Intersection => 0.2,
        InferenceKind::OnEdge => 0.34,
        InferenceKind::FaceCenter => 0.42,
        InferenceKind::OnFace => 0.55,
        InferenceKind::AxisX | InferenceKind::AxisY | InferenceKind::AxisZ => 0.72,
        InferenceKind::Parallel | InferenceKind::Perpendicular => 0.78,
        InferenceKind::FromPoint => 0.82,
    };
    kind_score + distance_score * 0.12 - if candidate.sticky { sticky_bonus } else { 0.0 }
}

pub fn closest_point_on_locked_axis_from_ray(
    ray_origin: Vec3,
    ray_direction: Vec3,
    line_origin: Vec3,
    line_direction: Vec3,
) -> Option<Vec3> {
    let c_dir = ray_direction.try_normalize()?;
    let u = line_direction.try_normalize()?;
    let n = u.cross(c_dir);
    if n.length_squared() < 1.0e-8 {
        return Some(line_origin + u * (ray_origin - line_origin).dot(u));
    }
    let c_cross_n = c_dir.cross(n);
    let denominator = u.dot(c_cross_n);
    if denominator.abs() < 1.0e-8 {
        return Some(line_origin + u * (ray_origin - line_origin).dot(u));
    }
    let s = (ray_origin - line_origin).dot(c_cross_n) / denominator;
    Some(line_origin + u * s)
}

pub fn rectangle_plane_from_view_or_face(
    origin: Vec3,
    view_direction: Vec3,
    hovered_face_normal: Option<Vec3>,
    locked_axis: Option<Vec3>,
) -> Option<RectangleDrawingPlane> {
    let normal = if let Some(axis) = locked_axis {
        axis.try_normalize()?
    } else if let Some(face_normal) = hovered_face_normal {
        face_normal.try_normalize()?
    } else {
        dominant_view_axis(view_direction)?
    };
    let (axis_u, axis_v, normal) = plane_basis(normal, None);
    Some(RectangleDrawingPlane {
        origin,
        normal,
        axis_u,
        axis_v,
    })
}

fn dominant_view_axis(view_direction: Vec3) -> Option<Vec3> {
    let view = view_direction.try_normalize()?;
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    axes.into_iter()
        .max_by(|a, b| view.dot(*a).abs().total_cmp(&view.dot(*b).abs()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionUpdate {
    Ignored,
    Cleared,
    Selected(SketchId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InferenceKind {
    Endpoint,
    Midpoint,
    FaceCenter,
    OnEdge,
    OnFace,
    AxisX,
    AxisY,
    AxisZ,
    Parallel,
    Perpendicular,
    FromPoint,
    Intersection,
}

impl InferenceKind {
    const fn priority(self) -> u8 {
        match self {
            Self::Endpoint => 100,
            Self::Intersection => 95,
            Self::Midpoint => 90,
            Self::FaceCenter => 80,
            Self::OnEdge => 70,
            Self::AxisX | Self::AxisY | Self::AxisZ => 65,
            Self::Parallel | Self::Perpendicular => 60,
            Self::FromPoint => 50,
            Self::OnFace => 20,
        }
    }

    pub const fn tooltip(self) -> &'static str {
        match self {
            Self::Endpoint => "Endpoint",
            Self::Midpoint => "Midpoint",
            Self::FaceCenter => "Face center",
            Self::OnEdge => "On edge",
            Self::OnFace => "On face",
            Self::AxisX => "Red axis",
            Self::AxisY => "Green axis",
            Self::AxisZ => "Blue axis",
            Self::Parallel => "Parallel",
            Self::Perpendicular => "Perpendicular",
            Self::FromPoint => "From point",
            Self::Intersection => "Intersection",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InferenceCandidate {
    pub kind: InferenceKind,
    pub point: Vec3,
    pub direction: Option<Vec3>,
    pub plane_normal: Option<Vec3>,
    pub strength: f32,
    pub tooltip: String,
}

impl InferenceCandidate {
    pub fn new(
        kind: InferenceKind,
        point: Vec3,
        strength: f32,
        tooltip: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            point,
            direction: None,
            plane_normal: None,
            strength,
            tooltip: tooltip.into(),
        }
    }

    pub fn with_direction(mut self, direction: Vec3) -> Self {
        self.direction = Some(direction);
        self
    }

    pub fn with_plane_normal(mut self, plane_normal: Vec3) -> Self {
        self.plane_normal = Some(plane_normal);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InferenceLock {
    kind: InferenceKind,
    anchor: Vec3,
    direction: Option<Vec3>,
    plane_normal: Option<Vec3>,
}

impl InferenceLock {
    pub fn axis(kind: InferenceKind, anchor: Vec3) -> Option<Self> {
        let direction = match kind {
            InferenceKind::AxisX => Vec3::X,
            InferenceKind::AxisY => Vec3::Y,
            InferenceKind::AxisZ => Vec3::Z,
            _ => return None,
        };
        Some(Self {
            kind,
            anchor,
            direction: Some(direction),
            plane_normal: None,
        })
    }

    pub fn from_point(anchor: Vec3, direction: Vec3) -> Option<Self> {
        let direction = direction.try_normalize()?;
        Some(Self {
            kind: InferenceKind::FromPoint,
            anchor,
            direction: Some(direction),
            plane_normal: None,
        })
    }

    pub fn plane(kind: InferenceKind, anchor: Vec3, plane_normal: Vec3) -> Option<Self> {
        if !matches!(kind, InferenceKind::OnFace | InferenceKind::FaceCenter) {
            return None;
        }
        let plane_normal = plane_normal.try_normalize()?;
        Some(Self {
            kind,
            anchor,
            direction: None,
            plane_normal: Some(plane_normal),
        })
    }

    pub fn apply(self, raw_point: Vec3) -> InferenceCandidate {
        let point = if let Some(plane_normal) = self.plane_normal {
            raw_point - plane_normal * (raw_point - self.anchor).dot(plane_normal)
        } else if let Some(direction) = self.direction {
            self.anchor + direction * (raw_point - self.anchor).dot(direction)
        } else {
            self.anchor
        };
        let mut candidate = InferenceCandidate::new(
            self.kind,
            point,
            1.0,
            format!("{} locked", self.kind.tooltip()),
        );
        if let Some(direction) = self.direction {
            candidate = candidate.with_direction(direction);
        }
        if let Some(plane_normal) = self.plane_normal {
            candidate = candidate.with_plane_normal(plane_normal);
        }
        candidate
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct InferenceService;

impl InferenceService {
    pub fn from_pick(
        document: &SketchDocument,
        hit: &HitRecord,
        reference_point: Option<Vec3>,
    ) -> Result<Vec<InferenceCandidate>, SketchModelError> {
        let mut candidates = document.entity_inference_candidates(hit.entity)?;
        push_hit_inference_candidate(hit, &mut candidates);
        if let Some(reference_point) = reference_point {
            push_reference_inference_candidates(reference_point, hit.world_point, &mut candidates);
        }
        Ok(Self::ranked(candidates))
    }

    pub fn best(
        candidates: impl IntoIterator<Item = InferenceCandidate>,
    ) -> Option<InferenceCandidate> {
        let mut ranked: Vec<_> = candidates.into_iter().collect();
        Self::rank(&mut ranked);
        ranked.into_iter().next()
    }

    pub fn ranked(
        candidates: impl IntoIterator<Item = InferenceCandidate>,
    ) -> Vec<InferenceCandidate> {
        let mut ranked: Vec<_> = candidates.into_iter().collect();
        Self::rank(&mut ranked);
        ranked
    }

    fn rank(candidates: &mut [InferenceCandidate]) {
        candidates.sort_by(|a, b| {
            b.strength
                .total_cmp(&a.strength)
                .then_with(|| b.kind.priority().cmp(&a.kind.priority()))
                .then_with(|| stable_kind_order(a.kind).cmp(&stable_kind_order(b.kind)))
        });
    }
}

fn push_hit_inference_candidate(hit: &HitRecord, candidates: &mut Vec<InferenceCandidate>) {
    let mut candidate = match hit.kind {
        HitKind::Vertex => InferenceCandidate::new(
            InferenceKind::Endpoint,
            hit.world_point,
            1.04,
            InferenceKind::Endpoint.tooltip(),
        ),
        HitKind::Edge => InferenceCandidate::new(
            InferenceKind::OnEdge,
            hit.world_point,
            0.82,
            InferenceKind::OnEdge.tooltip(),
        ),
        HitKind::Face => InferenceCandidate::new(
            InferenceKind::OnFace,
            hit.world_point,
            0.78,
            InferenceKind::OnFace.tooltip(),
        ),
        HitKind::Guide => InferenceCandidate::new(
            InferenceKind::FromPoint,
            hit.world_point,
            0.74,
            InferenceKind::FromPoint.tooltip(),
        ),
        HitKind::SectionPlane => InferenceCandidate::new(
            InferenceKind::OnFace,
            hit.world_point,
            0.72,
            "On section plane",
        ),
        HitKind::Instance => InferenceCandidate::new(
            InferenceKind::FromPoint,
            hit.world_point,
            0.54,
            "On component",
        ),
    };
    if let Some(normal) = hit.normal.and_then(|normal| normal.try_normalize()) {
        candidate = candidate.with_plane_normal(normal);
    }
    candidates.push(candidate);
}

fn push_reference_inference_candidates(
    reference_point: Vec3,
    raw_point: Vec3,
    candidates: &mut Vec<InferenceCandidate>,
) {
    let delta = raw_point - reference_point;
    for (kind, axis) in [
        (InferenceKind::AxisX, Vec3::X),
        (InferenceKind::AxisY, Vec3::Y),
        (InferenceKind::AxisZ, Vec3::Z),
    ] {
        let projected = reference_point + axis * delta.dot(axis);
        if (projected - reference_point).length_squared() > PLANAR_GRAPH_MIN_AREA {
            candidates.push(
                InferenceCandidate::new(
                    kind,
                    projected,
                    0.68,
                    format!("{} from point", kind.tooltip()),
                )
                .with_direction(axis),
            );
        }
    }
    if let Some(direction) = delta.try_normalize() {
        candidates.push(
            InferenceCandidate::new(
                InferenceKind::FromPoint,
                raw_point,
                0.62,
                InferenceKind::FromPoint.tooltip(),
            )
            .with_direction(direction),
        );
    }
}

fn stable_kind_order(kind: InferenceKind) -> u8 {
    match kind {
        InferenceKind::Endpoint => 0,
        InferenceKind::Midpoint => 1,
        InferenceKind::FaceCenter => 2,
        InferenceKind::OnEdge => 3,
        InferenceKind::OnFace => 4,
        InferenceKind::AxisX => 5,
        InferenceKind::AxisY => 6,
        InferenceKind::AxisZ => 7,
        InferenceKind::Parallel => 8,
        InferenceKind::Perpendicular => 9,
        InferenceKind::FromPoint => 10,
        InferenceKind::Intersection => 11,
    }
}

fn safe_normal(normal: Vec3) -> Vec3 {
    normal.try_normalize().unwrap_or(Vec3::Z)
}

fn normal_axis_index(normal: IVec3) -> Option<usize> {
    let abs_sum = normal.x.abs() + normal.y.abs() + normal.z.abs();
    if abs_sum != 1 {
        return None;
    }
    if normal.x != 0 {
        Some(0)
    } else if normal.y != 0 {
        Some(1)
    } else {
        Some(2)
    }
}

fn ivec3_to_vec3(value: IVec3) -> Vec3 {
    Vec3::new(value.x as f32, value.y as f32, value.z as f32)
}

fn plane_basis(normal: Vec3, preferred_axis: Option<Vec3>) -> (Vec3, Vec3, Vec3) {
    let normal = safe_normal(normal);
    let axis_u = preferred_axis
        .and_then(|axis| (axis - normal * axis.dot(normal)).try_normalize())
        .unwrap_or_else(|| {
            let fallback = if normal.z.abs() < 0.9 {
                Vec3::Z
            } else {
                Vec3::Y
            };
            normal.cross(fallback).try_normalize().unwrap_or(Vec3::X)
        });
    let axis_v = normal.cross(axis_u).try_normalize().unwrap_or(Vec3::Y);
    (axis_u, axis_v, normal)
}

fn radial_points(
    center: Vec3,
    normal: Vec3,
    radius: f32,
    count: usize,
    preferred_axis: Option<Vec3>,
) -> Vec<Vec3> {
    let count = count.max(3);
    let (axis_u, axis_v, _) = plane_basis(normal, preferred_axis);
    (0..count)
        .map(|index| {
            let angle = std::f32::consts::TAU * index as f32 / count as f32;
            center + (axis_u * angle.cos() + axis_v * angle.sin()) * radius
        })
        .collect()
}

fn cad_rectangle_face(points: &[Vec3]) -> Result<(Vec<Vec3>, Vec3), SketchModelError> {
    match points {
        [a, b, ..] if points.len() == 2 => {
            let delta = *b - *a;
            if delta.length_squared() <= PLANAR_GRAPH_MIN_AREA {
                return Err(SketchModelError::InvalidCadCommand(
                    "RECTANGLE points must not be identical",
                ));
            }
            let (axis_u, axis_v) = if delta.y.abs() > PLANAR_GRAPH_MIN_AREA
                && delta.z.abs() <= PLANAR_GRAPH_MIN_AREA
            {
                (Vec3::X * delta.x, Vec3::Y * delta.y)
            } else if delta.y.abs() > PLANAR_GRAPH_MIN_AREA
                && delta.x.abs() <= PLANAR_GRAPH_MIN_AREA
            {
                (Vec3::Z * delta.z, Vec3::Y * delta.y)
            } else {
                (Vec3::X * delta.x, Vec3::Z * delta.z)
            };
            let vertices = vec![*a, *a + axis_u, *a + axis_u + axis_v, *a + axis_v];
            let normal = safe_normal(axis_u.cross(axis_v));
            Ok((vertices, normal))
        }
        _ if points.len() >= 3 => {
            let normal = cad_polygon_normal(points).ok_or(SketchModelError::InvalidCadCommand(
                "RECTANGLE polygon points must not be collinear",
            ))?;
            let vertices = if points.len() >= 4 {
                points[..4].to_vec()
            } else {
                points.to_vec()
            };
            Ok((vertices, normal))
        }
        _ => Err(SketchModelError::InvalidCadCommand(
            "RECTANGLE requires two diagonal points or a polygon",
        )),
    }
}

fn cad_polygon_normal(points: &[Vec3]) -> Option<Vec3> {
    if points.len() < 3 {
        return None;
    }
    let origin = points[0];
    for index in 1..points.len().saturating_sub(1) {
        let normal = (points[index] - origin).cross(points[index + 1] - origin);
        if normal.length_squared() > PLANAR_GRAPH_MIN_AREA {
            return Some(safe_normal(normal));
        }
    }
    None
}

fn cad_opening_size(normal: Vec3, width: f32, height: f32) -> Vec3 {
    let normal = safe_normal(normal);
    if normal.y.abs() > normal.x.abs().max(normal.z.abs()) {
        Vec3::new(width, 0.0, height)
    } else if normal.x.abs() > normal.z.abs() {
        Vec3::new(0.0, width, height)
    } else {
        Vec3::new(width, height, 0.0)
    }
}

fn cad_material_color(name: &str) -> SketchColor {
    match name.to_ascii_lowercase().as_str() {
        "glowstone" | "glow_stone" | "glow" => SketchColor::rgb(255, 198, 138),
        "limestone" | "stone" => SketchColor::rgb(210, 205, 184),
        "glass" | "liquidglass" | "liquid_glass" => SketchColor::rgba(140, 220, 255, 170),
        "road" | "asphalt" => SketchColor::rgb(42, 45, 52),
        "metal" | "steel" => SketchColor::rgb(150, 160, 166),
        "wood" => SketchColor::rgb(145, 99, 58),
        _ => SketchColor::rgb(190, 205, 210),
    }
}

fn sorted_edge_key(a: SketchId, b: SketchId) -> (u64, u64) {
    let a = a.raw();
    let b = b.raw();
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn points_close(a: Vec3, b: Vec3) -> bool {
    a.distance_squared(b) <= 1.0e-6
}

fn point_on_plane(point: Vec3, normal: Vec3, plane_d: f32) -> bool {
    (normal.dot(point) + plane_d).abs() <= 1.0e-3
}

fn point_on_segment(point: Vec3, a: Vec3, b: Vec3) -> bool {
    let ab = b - a;
    let ap = point - a;
    let ab_len_sq = ab.length_squared();
    if ab_len_sq <= PLANAR_GRAPH_MIN_AREA {
        return points_close(point, a);
    }
    let t = ap.dot(ab) / ab_len_sq;
    if !(-1.0e-4..=1.0001).contains(&t) {
        return false;
    }
    let closest = a + ab * t.clamp(0.0, 1.0);
    closest.distance_squared(point) <= 1.0e-6
}

fn find_point_index(points: &[Vec3], point: Vec3) -> Option<usize> {
    points
        .iter()
        .position(|candidate| points_close(*candidate, point))
}

fn insert_boundary_point(points: &mut Vec<Vec3>, point: Vec3) -> Option<usize> {
    if let Some(index) = find_point_index(points, point) {
        return Some(index);
    }
    let len = points.len();
    for index in 0..len {
        let next = (index + 1) % len;
        if point_on_segment(point, points[index], points[next]) {
            let insert_at = index + 1;
            points.insert(insert_at, point);
            return Some(insert_at);
        }
    }
    None
}

fn polygon_path_between(points: &[Vec3], from: usize, to: usize) -> Vec<Vec3> {
    let mut out = Vec::new();
    let mut index = from;
    loop {
        out.push(points[index]);
        if index == to {
            break;
        }
        index = (index + 1) % points.len();
    }
    out
}

fn format_cad_number(value: f32) -> String {
    let rounded = value.round();
    if (value - rounded).abs() < 1.0e-4 {
        return (rounded as i64).to_string();
    }
    let mut formatted = format!("{value:.4}");
    while formatted.contains('.') && formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
}

fn push_face_inference_candidates(
    vertices: &[Vec3],
    normal: Vec3,
    candidates: &mut Vec<InferenceCandidate>,
) {
    let normal = safe_normal(normal);
    for vertex in vertices {
        candidates.push(
            InferenceCandidate::new(
                InferenceKind::Endpoint,
                *vertex,
                1.0,
                InferenceKind::Endpoint.tooltip(),
            )
            .with_plane_normal(normal),
        );
    }
    push_curve_inference_candidates(vertices, true, Some(normal), candidates);
    if !vertices.is_empty() {
        let center = vertices
            .iter()
            .copied()
            .fold(Vec3::ZERO, |acc, vertex| acc + vertex)
            / vertices.len() as f32;
        candidates.push(
            InferenceCandidate::new(
                InferenceKind::FaceCenter,
                center,
                0.86,
                InferenceKind::FaceCenter.tooltip(),
            )
            .with_plane_normal(normal),
        );
        candidates.push(
            InferenceCandidate::new(
                InferenceKind::OnFace,
                center,
                0.42,
                InferenceKind::OnFace.tooltip(),
            )
            .with_plane_normal(normal),
        );
    }
}

fn push_curve_inference_candidates(
    points: &[Vec3],
    closed: bool,
    plane_normal: Option<Vec3>,
    candidates: &mut Vec<InferenceCandidate>,
) {
    if points.is_empty() {
        return;
    }
    let plane_normal = plane_normal.map(safe_normal);
    if !closed {
        for point in [points[0], points[points.len() - 1]] {
            let mut candidate = InferenceCandidate::new(
                InferenceKind::Endpoint,
                point,
                1.0,
                InferenceKind::Endpoint.tooltip(),
            );
            if let Some(normal) = plane_normal {
                candidate = candidate.with_plane_normal(normal);
            }
            candidates.push(candidate);
        }
    }

    let segment_count = if closed {
        points.len()
    } else {
        points.len().saturating_sub(1)
    };
    for index in 0..segment_count {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        let midpoint = (a + b) * 0.5;
        let mut midpoint_candidate = InferenceCandidate::new(
            InferenceKind::Midpoint,
            midpoint,
            0.92,
            InferenceKind::Midpoint.tooltip(),
        );
        if let Some(normal) = plane_normal {
            midpoint_candidate = midpoint_candidate.with_plane_normal(normal);
        }
        candidates.push(midpoint_candidate);

        if let Some(direction) = (b - a).try_normalize() {
            let mut edge_candidate = InferenceCandidate::new(
                InferenceKind::OnEdge,
                midpoint,
                0.74,
                InferenceKind::OnEdge.tooltip(),
            )
            .with_direction(direction);
            if let Some(normal) = plane_normal {
                edge_candidate = edge_candidate.with_plane_normal(normal);
            }
            candidates.push(edge_candidate);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EditorToolId {
    Select,
    Pencil,
    Rectangle,
    Circle,
    Polygon,
    Arc,
    Freehand,
    House,
    PushPull,
    Move,
    Scale,
    Rotate,
    Room,
    CutOpening,
    Road,
    BotArea,
    Material,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorToolPhase {
    Idle,
    Previewing,
    Committed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorToolDefinition {
    pub id: EditorToolId,
    pub label: &'static str,
    pub begin_hint: &'static str,
    pub preview_hint: &'static str,
    pub commit_label: &'static str,
    pub cancel_hint: &'static str,
    pub uses_inference: bool,
    pub supports_typed_measurement: bool,
}

impl EditorToolDefinition {
    const fn new(
        id: EditorToolId,
        label: &'static str,
        begin_hint: &'static str,
        preview_hint: &'static str,
        commit_label: &'static str,
        cancel_hint: &'static str,
        uses_inference: bool,
        supports_typed_measurement: bool,
    ) -> Self {
        Self {
            id,
            label,
            begin_hint,
            preview_hint,
            commit_label,
            cancel_hint,
            uses_inference,
            supports_typed_measurement,
        }
    }
}

#[derive(Resource, Debug, Clone)]
pub struct EditorToolCatalog {
    definitions: Vec<EditorToolDefinition>,
}

impl Default for EditorToolCatalog {
    fn default() -> Self {
        Self {
            definitions: vec![
                editor_tool_definition(EditorToolId::Select),
                editor_tool_definition(EditorToolId::Pencil),
                editor_tool_definition(EditorToolId::Rectangle),
                editor_tool_definition(EditorToolId::Circle),
                editor_tool_definition(EditorToolId::Polygon),
                editor_tool_definition(EditorToolId::Arc),
                editor_tool_definition(EditorToolId::Freehand),
                editor_tool_definition(EditorToolId::House),
                editor_tool_definition(EditorToolId::PushPull),
                editor_tool_definition(EditorToolId::Move),
                editor_tool_definition(EditorToolId::Scale),
                editor_tool_definition(EditorToolId::Rotate),
                editor_tool_definition(EditorToolId::Room),
                editor_tool_definition(EditorToolId::CutOpening),
                editor_tool_definition(EditorToolId::Road),
                editor_tool_definition(EditorToolId::BotArea),
                editor_tool_definition(EditorToolId::Material),
            ],
        }
    }
}

impl EditorToolCatalog {
    pub fn definition(&self, id: EditorToolId) -> Option<&EditorToolDefinition> {
        self.definitions
            .iter()
            .find(|definition| definition.id == id)
    }

    pub fn definitions(&self) -> &[EditorToolDefinition] {
        &self.definitions
    }
}

fn editor_tool_definition(tool: EditorToolId) -> EditorToolDefinition {
    match tool {
        EditorToolId::Select => EditorToolDefinition::new(
            tool,
            "SELECT",
            "Select or inspect faces, edges, rooms, roads, and components.",
            "Hover geometry to inspect entity, component, material, and snap references.",
            "Selection",
            "Selection preview cancelled.",
            true,
            false,
        ),
        EditorToolId::Pencil => EditorToolDefinition::new(
            tool,
            "PENCIL",
            "Click an endpoint, midpoint, face center, or grid point to start a voxel line.",
            "Move to a snapped endpoint or axis inference; click to commit the line.",
            "Pencil line",
            "Pencil line cancelled.",
            true,
            true,
        ),
        EditorToolId::Rectangle => EditorToolDefinition::new(
            tool,
            "RECTANGLE",
            "Click a start corner on floor, wall, roof, or side plane.",
            "Move to a snapped opposite corner; click to commit a face.",
            "Rectangle face",
            "Rectangle preview cancelled.",
            true,
            true,
        ),
        EditorToolId::Circle => EditorToolDefinition::new(
            tool,
            "CIRCLE",
            "Click a center point on a locked face plane, then move to set radius.",
            "Move through endpoint, midpoint, axis, or typed-radius inference; click to commit.",
            "Circle face",
            "Circle preview cancelled.",
            true,
            true,
        ),
        EditorToolId::Polygon => EditorToolDefinition::new(
            tool,
            "POLYGON",
            "Click a center point on a locked plane, then move to set polygon radius.",
            "Move through snap references or type side count/radius before committing.",
            "Polygon face",
            "Polygon preview cancelled.",
            true,
            true,
        ),
        EditorToolId::Arc => EditorToolDefinition::new(
            tool,
            "ARC",
            "Click an arc center/start reference on a face plane.",
            "Move through endpoint, midpoint, and tangent references to shape the arc.",
            "Arc curve",
            "Arc preview cancelled.",
            true,
            true,
        ),
        EditorToolId::Freehand => EditorToolDefinition::new(
            tool,
            "FREEHAND",
            "Press and drag on a locked sketch plane to collect a freehand curve.",
            "Smooth sampled points into selectable curve segments with midpoint inference.",
            "Freehand curve",
            "Freehand preview cancelled.",
            true,
            true,
        ),
        EditorToolId::House => EditorToolDefinition::new(
            tool,
            "HOUSE",
            "Start a guided house: draw footprint, pull walls, cut openings, hollow room.",
            "Follow the current house stage; toolbox clicks safely switch steps.",
            "House step",
            "House step cancelled.",
            true,
            true,
        ),
        EditorToolId::PushPull => EditorToolDefinition::new(
            tool,
            "PUSH/PULL",
            "Click a face, then move the mouse to choose wall, roof, or floor depth.",
            "Move along the face normal with endpoint/midpoint/face-center references.",
            "Push/Pull",
            "Push/Pull preview cancelled.",
            true,
            true,
        ),
        EditorToolId::Move => EditorToolDefinition::new(
            tool,
            "MOVE",
            "Select geometry, then drag from an endpoint, midpoint, face center, or axis handle.",
            "Move along snapped endpoint, midpoint, face-center, axis, or typed-distance inference.",
            "Move selection",
            "Move preview cancelled.",
            true,
            true,
        ),
        EditorToolId::Scale => EditorToolDefinition::new(
            tool,
            "SCALE",
            "Select geometry, then drag a corner or edge handle from a snapped pivot.",
            "Scale from pivot, midpoint, opposite-corner, or typed-ratio inference.",
            "Scale selection",
            "Scale preview cancelled.",
            true,
            true,
        ),
        EditorToolId::Rotate => EditorToolDefinition::new(
            tool,
            "ROTATE",
            "Select geometry, then drag a rotate ring around a snapped axis.",
            "Rotate around face normal or world axis with 15/45/90 degree inference.",
            "Rotate selection",
            "Rotate preview cancelled.",
            true,
            true,
        ),
        EditorToolId::Room => EditorToolDefinition::new(
            tool,
            "ROOM",
            "Click a solid shell face or footprint to create livable hollow space.",
            "Preview interior bounds while preserving the wall shell thickness.",
            "Room hollow",
            "Room hollow preview cancelled.",
            true,
            true,
        ),
        EditorToolId::CutOpening => EditorToolDefinition::new(
            tool,
            "OPENING",
            "Click a wall face to place a door or window opening.",
            "Drag snapped width and height on the wall face; click to cut through wall thickness.",
            "Opening cut",
            "Opening preview cancelled.",
            true,
            true,
        ),
        EditorToolId::Road => EditorToolDefinition::new(
            tool,
            "ROAD",
            "Click road endpoints to create editable road components.",
            "Move to a branch, curve, bridge, or roundabout endpoint; click to commit.",
            "Road component",
            "Road preview cancelled.",
            true,
            true,
        ),
        EditorToolId::BotArea => EditorToolDefinition::new(
            tool,
            "BOT AREA",
            "Click two corners to mark the exact city area bots may build inside.",
            "Preview the bounded city zone so bots stay under player command.",
            "Bot city area",
            "Bot area preview cancelled.",
            true,
            true,
        ),
        EditorToolId::Material => EditorToolDefinition::new(
            tool,
            "MATERIAL",
            "Pick a style/material for the selected component or active tool.",
            "Hover/select a component to preview material assignment.",
            "Material style",
            "Material preview cancelled.",
            false,
            false,
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HouseBuildStage {
    Footprint,
    PullWalls,
    CutOpenings,
    HollowRoom,
    RoofAndDetails,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HouseWorkflowGuide {
    pub stage: HouseBuildStage,
    pub material: SketchId,
}

impl HouseWorkflowGuide {
    pub fn status(self) -> &'static str {
        match self.stage {
            HouseBuildStage::Footprint => {
                "HOUSE: Footprint first. Draw Rectangle/Pencil, then Push/Pull walls, Opening cuts, Room hollow, roof/floor details."
            }
            HouseBuildStage::PullWalls => {
                "HOUSE: Push/Pull the footprint into walls or roof massing; Esc cancels and RMB only orbits."
            }
            HouseBuildStage::CutOpenings => {
                "HOUSE: Opening cuts doors/windows through wall thickness from the selected face."
            }
            HouseBuildStage::HollowRoom => {
                "HOUSE: Room hollows a usable interior while preserving the outer shell."
            }
            HouseBuildStage::RoofAndDetails => {
                "HOUSE: Add roof/floor detail, material style, and component cleanup."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorCancelReason {
    Escape,
    ToolSwitch,
    ToolboxClick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolInputEffect {
    OrbitOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTransaction {
    pub label: String,
    pub touched: Vec<SketchId>,
}

impl ToolTransaction {
    fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            touched: Vec::new(),
        }
    }
}

#[derive(Resource, Debug, Clone)]
pub struct ToolController {
    active_tool: EditorToolId,
    tool_phase: EditorToolPhase,
    selection: SelectionSet,
    inference_lock: Option<InferenceLock>,
    preview_generation: u64,
    open_transaction: Option<ToolTransaction>,
    last_transaction_label: Option<String>,
    last_cancelled_transaction_label: Option<String>,
    active_tool_label: &'static str,
    active_tool_hint: String,
    house_guide: Option<HouseWorkflowGuide>,
}

impl Default for ToolController {
    fn default() -> Self {
        let definition = editor_tool_definition(EditorToolId::Select);
        Self {
            active_tool: EditorToolId::Select,
            tool_phase: EditorToolPhase::Idle,
            selection: SelectionSet::default(),
            inference_lock: None,
            preview_generation: 0,
            open_transaction: None,
            last_transaction_label: None,
            last_cancelled_transaction_label: None,
            active_tool_label: definition.label,
            active_tool_hint: definition.begin_hint.to_owned(),
            house_guide: None,
        }
    }
}

impl ToolController {
    pub fn active_tool(&self) -> EditorToolId {
        self.active_tool
    }

    pub fn tool_phase(&self) -> EditorToolPhase {
        self.tool_phase
    }

    pub fn active_tool_label(&self) -> &'static str {
        self.active_tool_label
    }

    pub fn active_tool_hint(&self) -> &str {
        self.active_tool_hint.as_str()
    }

    pub fn activate(&mut self, tool: EditorToolId) {
        if self.active_tool != tool {
            self.cancel_open_transaction_for_lifecycle();
            self.active_tool = tool;
            self.inference_lock = None;
            self.open_transaction = None;
            self.tool_phase = EditorToolPhase::Idle;
            if tool != EditorToolId::House {
                self.house_guide = None;
            }
            self.sync_lifecycle_from_definition();
            self.preview_generation += 1;
        }
    }

    pub fn start_house_workflow(&mut self, material: SketchId) {
        self.activate(EditorToolId::House);
        self.house_guide = Some(HouseWorkflowGuide {
            stage: HouseBuildStage::Footprint,
            material,
        });
        if let Some(guide) = self.house_guide {
            self.active_tool_hint = guide.status().to_owned();
        }
        self.tool_phase = EditorToolPhase::Idle;
        self.preview_generation += 1;
    }

    pub fn house_guide(&self) -> Option<HouseWorkflowGuide> {
        self.house_guide
    }

    pub fn selection(&self) -> &SelectionSet {
        &self.selection
    }

    pub fn selection_mut(&mut self) -> &mut SelectionSet {
        &mut self.selection
    }

    pub fn select_hit(&mut self, hit: &HitRecord, additive: bool) -> SelectionUpdate {
        if !additive {
            self.selection.clear();
        }
        self.selection.select(hit.entity);
        self.active_tool_hint = format!(
            "Selected {:?} entity {}. Pick another linked face or choose a modeling tool.",
            hit.kind,
            hit.entity.raw()
        );
        self.tool_phase = EditorToolPhase::Committed;
        self.preview_generation += 1;
        SelectionUpdate::Selected(hit.entity)
    }

    pub fn clear_selection(&mut self) -> SelectionUpdate {
        if self.selection.is_empty() {
            return SelectionUpdate::Ignored;
        }
        self.selection.clear();
        self.active_tool_hint =
            "Selection cleared. Pick a linked face, edge, room, road, or component.".to_owned();
        self.tool_phase = EditorToolPhase::Idle;
        self.preview_generation += 1;
        SelectionUpdate::Cleared
    }

    pub fn preview_generation(&self) -> u64 {
        self.preview_generation
    }

    pub fn inference_lock(&self) -> Option<InferenceLock> {
        self.inference_lock
    }

    pub fn lock_inference(&mut self, inference_lock: InferenceLock) {
        let tooltip = inference_lock.apply(inference_lock.anchor).tooltip;
        self.inference_lock = Some(inference_lock);
        self.active_tool_hint = format!("{tooltip}. {}", self.active_tool_hint);
        self.preview_generation += 1;
    }

    pub fn project_locked_inference(&self, raw_point: Vec3) -> Option<InferenceCandidate> {
        self.inference_lock.map(|lock| lock.apply(raw_point))
    }

    pub fn begin_transaction(&mut self, label: impl Into<String>) {
        let definition = editor_tool_definition(self.active_tool);
        self.open_transaction = Some(ToolTransaction::new(label));
        self.tool_phase = EditorToolPhase::Previewing;
        self.active_tool_label = definition.label;
        self.active_tool_hint = definition.preview_hint.to_owned();
    }

    pub fn open_transaction_label(&self) -> Option<&str> {
        self.open_transaction
            .as_ref()
            .map(|transaction| transaction.label.as_str())
    }

    pub fn last_transaction_label(&self) -> Option<&str> {
        self.last_transaction_label.as_deref()
    }

    pub fn last_cancelled_transaction_label(&self) -> Option<&str> {
        self.last_cancelled_transaction_label.as_deref()
    }

    pub fn touch_entity(&mut self, id: SketchId) {
        if let Some(transaction) = &mut self.open_transaction {
            if !transaction.touched.contains(&id) {
                transaction.touched.push(id);
            }
        }
    }

    pub fn commit_transaction(&mut self) -> Option<ToolTransaction> {
        let committed = self.open_transaction.take()?;
        let definition = editor_tool_definition(self.active_tool);
        self.last_transaction_label = Some(committed.label.clone());
        self.tool_phase = EditorToolPhase::Committed;
        self.active_tool_label = definition.label;
        self.active_tool_hint = format!(
            "{} committed: {}.",
            definition.commit_label, committed.label
        );
        self.preview_generation += 1;
        Some(committed)
    }

    pub fn cancel_transaction(&mut self) {
        if self.cancel_open_transaction_for_lifecycle() {
            self.preview_generation += 1;
        }
    }

    pub fn cancel_active_operation(&mut self, _reason: EditorCancelReason) -> bool {
        let had_operation = self.cancel_open_transaction_for_lifecycle();
        if had_operation {
            self.preview_generation += 1;
        }
        had_operation
    }

    pub fn handle_right_mouse_orbit(&self) -> ToolInputEffect {
        ToolInputEffect::OrbitOnly
    }

    fn cancel_open_transaction_for_lifecycle(&mut self) -> bool {
        let Some(transaction) = self.open_transaction.take() else {
            return false;
        };
        let definition = editor_tool_definition(self.active_tool);
        self.last_cancelled_transaction_label = Some(transaction.label);
        self.tool_phase = EditorToolPhase::Cancelled;
        self.active_tool_label = definition.label;
        self.active_tool_hint = definition.cancel_hint.to_owned();
        true
    }

    fn sync_lifecycle_from_definition(&mut self) {
        let definition = editor_tool_definition(self.active_tool);
        self.active_tool_label = definition.label;
        self.active_tool_hint = definition.begin_hint.to_owned();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SketchRegistryError {
    DuplicateId(String),
    UnknownExtension(String),
}

impl fmt::Display for SketchRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "duplicate sketch registry id {id}"),
            Self::UnknownExtension(id) => write!(f, "unknown sketch extension {id}"),
        }
    }
}

impl Error for SketchRegistryError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
}

impl SketchExtensionManifest {
    pub fn new(id: impl Into<String>, name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: version.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchToolDescriptor {
    pub id: String,
    pub label: String,
    pub editor_tool: EditorToolId,
    pub extension: Option<String>,
}

impl SketchToolDescriptor {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        editor_tool: EditorToolId,
        extension: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            editor_tool,
            extension,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchCommandDescriptor {
    pub id: String,
    pub label: String,
    pub tool: Option<EditorToolId>,
    pub extension: Option<String>,
}

impl SketchCommandDescriptor {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        tool: Option<EditorToolId>,
        extension: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            tool,
            extension,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchIoFormat {
    pub extension: String,
    pub label: String,
    pub preserves_semantics: bool,
}

impl SketchIoFormat {
    pub fn new(
        extension: impl Into<String>,
        label: impl Into<String>,
        preserves_semantics: bool,
    ) -> Self {
        Self {
            extension: extension.into(),
            label: label.into(),
            preserves_semantics,
        }
    }
}

#[derive(Resource, Debug, Clone)]
pub struct SketchCommandRegistry {
    extensions: BTreeMap<String, SketchExtensionManifest>,
    tools: BTreeMap<String, SketchToolDescriptor>,
    commands: BTreeMap<String, SketchCommandDescriptor>,
    importers: BTreeMap<String, SketchIoFormat>,
    exporters: BTreeMap<String, SketchIoFormat>,
}

impl Default for SketchCommandRegistry {
    fn default() -> Self {
        let mut registry = Self::empty();
        for (id, label, tool) in [
            ("editor.select", "Select", EditorToolId::Select),
            ("editor.pencil", "Pencil", EditorToolId::Pencil),
            ("editor.rectangle", "Rectangle", EditorToolId::Rectangle),
            ("editor.circle", "Circle", EditorToolId::Circle),
            ("editor.polygon", "Polygon", EditorToolId::Polygon),
            ("editor.arc", "Arc", EditorToolId::Arc),
            ("editor.freehand", "Freehand", EditorToolId::Freehand),
            ("editor.house", "House", EditorToolId::House),
            ("editor.push_pull", "Push/Pull", EditorToolId::PushPull),
            ("editor.move", "Move", EditorToolId::Move),
            ("editor.scale", "Scale", EditorToolId::Scale),
            ("editor.rotate", "Rotate", EditorToolId::Rotate),
            ("editor.room", "Room", EditorToolId::Room),
            ("editor.opening", "Opening", EditorToolId::CutOpening),
            ("editor.road", "Road", EditorToolId::Road),
            ("editor.bot_area", "Bot Area", EditorToolId::BotArea),
            ("editor.material", "Material", EditorToolId::Material),
        ] {
            registry
                .register_tool(SketchToolDescriptor::new(id, label, tool, None))
                .expect("built-in tool ids are unique");
            registry
                .register_command(SketchCommandDescriptor::new(id, label, Some(tool), None))
                .expect("built-in command ids are unique");
        }
        for format in [
            SketchIoFormat::new("gltf", "glTF scene", true),
            SketchIoFormat::new("dae", "COLLADA scene", true),
            SketchIoFormat::new("obj", "OBJ mesh", false),
            SketchIoFormat::new("stl", "STL mesh", false),
        ] {
            registry
                .register_importer(format.clone())
                .expect("built-in importer ids are unique");
            registry
                .register_exporter(format)
                .expect("built-in exporter ids are unique");
        }
        registry
    }
}

impl SketchCommandRegistry {
    pub fn empty() -> Self {
        Self {
            extensions: BTreeMap::new(),
            tools: BTreeMap::new(),
            commands: BTreeMap::new(),
            importers: BTreeMap::new(),
            exporters: BTreeMap::new(),
        }
    }

    pub fn register_extension(
        &mut self,
        extension: SketchExtensionManifest,
    ) -> Result<(), SketchRegistryError> {
        if self.extensions.contains_key(&extension.id) {
            return Err(SketchRegistryError::DuplicateId(extension.id));
        }
        self.extensions.insert(extension.id.clone(), extension);
        Ok(())
    }

    pub fn extension(&self, id: &str) -> Option<&SketchExtensionManifest> {
        self.extensions.get(id)
    }

    pub fn register_tool(&mut self, tool: SketchToolDescriptor) -> Result<(), SketchRegistryError> {
        self.check_extension(tool.extension.as_deref())?;
        if self.tools.contains_key(&tool.id) {
            return Err(SketchRegistryError::DuplicateId(tool.id));
        }
        self.tools.insert(tool.id.clone(), tool);
        Ok(())
    }

    pub fn tool(&self, id: &str) -> Option<&SketchToolDescriptor> {
        self.tools.get(id)
    }

    pub fn register_command(
        &mut self,
        command: SketchCommandDescriptor,
    ) -> Result<(), SketchRegistryError> {
        self.check_extension(command.extension.as_deref())?;
        if self.commands.contains_key(&command.id) {
            return Err(SketchRegistryError::DuplicateId(command.id));
        }
        self.commands.insert(command.id.clone(), command);
        Ok(())
    }

    pub fn command(&self, id: &str) -> Option<&SketchCommandDescriptor> {
        self.commands.get(id)
    }

    pub fn register_importer(&mut self, format: SketchIoFormat) -> Result<(), SketchRegistryError> {
        if self.importers.contains_key(&format.extension) {
            return Err(SketchRegistryError::DuplicateId(format.extension));
        }
        self.importers.insert(format.extension.clone(), format);
        Ok(())
    }

    pub fn importer(&self, extension: &str) -> Option<&SketchIoFormat> {
        self.importers.get(extension)
    }

    pub fn register_exporter(&mut self, format: SketchIoFormat) -> Result<(), SketchRegistryError> {
        if self.exporters.contains_key(&format.extension) {
            return Err(SketchRegistryError::DuplicateId(format.extension));
        }
        self.exporters.insert(format.extension.clone(), format);
        Ok(())
    }

    pub fn exporter(&self, extension: &str) -> Option<&SketchIoFormat> {
        self.exporters.get(extension)
    }

    fn check_extension(&self, extension: Option<&str>) -> Result<(), SketchRegistryError> {
        if let Some(extension) = extension {
            if !self.extensions.contains_key(extension) {
                return Err(SketchRegistryError::UnknownExtension(extension.to_owned()));
            }
        }
        Ok(())
    }
}

pub struct SketchModelPlugin;

impl Plugin for SketchModelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SketchDocument>()
            .init_resource::<PickService>()
            .init_resource::<InferenceService>()
            .init_resource::<ToolController>()
            .init_resource::<EditorToolCatalog>()
            .init_resource::<SketchCommandRegistry>()
            .init_resource::<SketchVoxelLinkIndex>()
            .init_resource::<SemanticHoverHit>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_bootstraps_root_context_and_side_tables() {
        let doc = SketchDocument::new();

        assert_eq!(doc.root_context(), doc.active_context());
        assert!(doc.context(doc.root_context()).is_some());
        assert_eq!(doc.default_tag_name(), Some("Untagged"));
        assert_eq!(doc.default_material_name(), Some("Default"));
        assert_eq!(doc.default_style_name(), Some("Modeling"));
    }

    #[test]
    fn voxel_link_index_registers_cells_faces_and_semantic_hits() {
        let context = SketchId::new_for_test(7);
        let face = SketchId::new_for_test(42);
        let extrusion = SketchId::new_for_test(43);
        let normal = IVec3::Y;
        let cell = IVec3::new(3, 4, 5);
        let mut links = SketchVoxelLinkIndex::default();

        assert!(links.link_face_cell(
            cell,
            normal,
            SketchVoxelLink::new(face, context, SketchVoxelLinkRole::Face),
        ));
        links.link_cell(
            cell + IVec3::Y,
            SketchVoxelLink::new(extrusion, context, SketchVoxelLinkRole::Extrusion),
        );

        let face_links = links.links_for_face(cell, normal);
        assert_eq!(face_links.len(), 1);
        assert_eq!(face_links[0].entity, face);
        assert_eq!(face_links[0].role, SketchVoxelLinkRole::Face);

        let cell_links = links.links_for_cell(cell);
        assert!(cell_links.iter().any(|link| link.entity == face));
        let hit = links
            .hit_for_face(cell, normal, Vec3::new(3.5, 5.0, 5.5), 12.0)
            .expect("linked voxel face should become a semantic hit");
        assert_eq!(hit.entity, face);
        assert_eq!(hit.kind, HitKind::Face);
        assert_eq!(hit.normal, Some(Vec3::Y));

        links.remove_entity(face);
        assert!(links.links_for_face(cell, normal).is_empty());
        assert!(links.links_for_cell(cell + IVec3::Y).iter().any(|link| {
            link.entity == extrusion && link.role == SketchVoxelLinkRole::Extrusion
        }));
    }

    #[test]
    fn voxel_link_index_rejects_malformed_face_normals() {
        let mut links = SketchVoxelLinkIndex::default();
        let context = SketchId::new_for_test(1);
        let entity = SketchId::new_for_test(2);

        assert!(!links.link_face_cell(
            IVec3::ZERO,
            IVec3::new(1, 1, 0),
            SketchVoxelLink::new(entity, context, SketchVoxelLinkRole::Face),
        ));
        assert!(links
            .links_for_face(IVec3::ZERO, IVec3::new(1, 1, 0))
            .is_empty());
        assert!(SketchVoxelFaceKey::new(IVec3::ZERO, IVec3::ZERO).is_none());
    }

    #[test]
    fn brep_kernel_builds_linked_face_topology() {
        let mut kernel = SketchBRepKernel::new();
        let face = kernel
            .add_face_from_points([
                Vec3::ZERO,
                Vec3::new(4.0, 0.0, 0.0),
                Vec3::new(4.0, 3.0, 0.0),
                Vec3::new(0.0, 3.0, 0.0),
            ])
            .unwrap();

        assert_eq!(kernel.vertices().len(), 4);
        assert_eq!(kernel.edges().len(), 4);
        assert_eq!(kernel.faces().len(), 1);
        assert_eq!(kernel.face(face).unwrap().outer_loop.len(), 4);
        assert_eq!(kernel.face(face).unwrap().normal, Vec3::Z);
        assert!(kernel
            .vertices()
            .values()
            .all(|vertex| vertex.connected_edges.len() == 2));
        assert!(kernel
            .edges()
            .values()
            .all(|edge| edge.faces.contains(&face)));
    }

    #[test]
    fn brep_split_face_with_coplanar_edge_replaces_face_with_two_loops() {
        let mut kernel = SketchBRepKernel::new();
        let face = kernel
            .add_face_from_points([
                Vec3::ZERO,
                Vec3::new(4.0, 0.0, 0.0),
                Vec3::new(4.0, 3.0, 0.0),
                Vec3::new(0.0, 3.0, 0.0),
            ])
            .unwrap();

        let (a, b) = kernel
            .split_face_with_edge(face, Vec3::new(2.0, 0.0, 0.0), Vec3::new(2.0, 3.0, 0.0))
            .unwrap();

        assert!(kernel.face(face).is_none());
        assert_eq!(kernel.faces().len(), 2);
        assert_eq!(kernel.face_vertices(a).unwrap().len(), 4);
        assert_eq!(kernel.face_vertices(b).unwrap().len(), 4);
        let shared_split_edges = kernel
            .edges()
            .values()
            .filter(|edge| edge.faces.contains(&a) && edge.faces.contains(&b))
            .count();
        assert_eq!(shared_split_edges, 1);
        assert!(kernel
            .vertices()
            .values()
            .any(|vertex| vertex.position == Vec3::new(2.0, 0.0, 0.0)));
        assert!(kernel
            .vertices()
            .values()
            .any(|vertex| vertex.position == Vec3::new(2.0, 3.0, 0.0)));
    }

    #[test]
    fn brep_push_pull_generates_top_face_and_side_faces() {
        let mut kernel = SketchBRepKernel::new();
        let face = kernel
            .add_face_from_points([
                Vec3::ZERO,
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(2.0, 2.0, 0.0),
                Vec3::new(0.0, 2.0, 0.0),
            ])
            .unwrap();

        let extrusion = kernel.push_pull_face(face, 3.0).unwrap();

        assert_eq!(extrusion.source_face, face);
        assert_eq!(extrusion.side_faces.len(), 4);
        assert_eq!(kernel.faces().len(), 6);
        let top_vertices = kernel.face_vertices(extrusion.top_face).unwrap();
        assert!(top_vertices.iter().all(|point| point.z == 3.0));
        for side_face in extrusion.side_faces {
            assert_eq!(kernel.face_vertices(side_face).unwrap().len(), 4);
        }
    }

    #[test]
    fn document_exports_semantic_face_into_brep_kernel() {
        let mut doc = SketchDocument::new();
        let face = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::X * 6.0,
                Vec3::Y * 4.0,
                "B-Rep source face",
            )
            .unwrap();

        let (kernel, brep_face) = doc.brep_kernel_for_face(face).unwrap();

        assert_eq!(kernel.vertices().len(), 4);
        assert_eq!(kernel.edges().len(), 4);
        assert_eq!(
            kernel.face_vertices(brep_face).unwrap()[2],
            Vec3::new(6.0, 4.0, 0.0)
        );
        assert_eq!(kernel.face(brep_face).unwrap().normal, Vec3::Z);
    }

    #[test]
    fn document_creates_entities_inside_active_context() {
        let mut doc = SketchDocument::new();
        let edge = doc
            .add_entity_to_active(SketchEntityKind::Edge {
                a: Vec3::ZERO,
                b: Vec3::X,
            })
            .unwrap();

        assert!(doc.entity(edge).is_some());
        assert_eq!(
            doc.context(doc.active_context()).unwrap().entities,
            vec![edge]
        );
    }

    #[test]
    fn component_definition_instances_share_definition_until_make_unique() {
        let mut doc = SketchDocument::new();
        let definition = doc.create_component_definition("Window Bay").unwrap();
        let first = doc
            .add_component_instance(definition, SketchTransform::from_translation(Vec3::X))
            .unwrap();
        let second = doc
            .add_component_instance(definition, SketchTransform::from_translation(Vec3::Y))
            .unwrap();

        assert_eq!(
            doc.component_definition_for_instance(first),
            Some(definition)
        );
        assert_eq!(
            doc.component_definition_for_instance(second),
            Some(definition)
        );

        let unique = doc.make_unique_instance(second).unwrap();

        assert_ne!(unique, definition);
        assert_eq!(
            doc.component_definition_for_instance(first),
            Some(definition)
        );
        assert_eq!(doc.component_definition_for_instance(second), Some(unique));
    }

    #[test]
    fn move_selection_translates_loose_geometry_and_instances_with_undo() {
        let mut doc = SketchDocument::new();
        let line = doc
            .draw_pencil_line(doc.active_context(), Vec3::ZERO, Vec3::X * 4.0)
            .unwrap();
        let definition = doc.create_component_definition("Window Bay").unwrap();
        let instance = doc
            .add_component_instance(definition, SketchTransform::from_translation(Vec3::Y))
            .unwrap();
        let mut selection = SelectionSet::default();
        selection.select(line);
        selection.select(instance);

        let moved = doc
            .move_selection(&selection, Vec3::new(2.0, 3.0, 4.0), "Move selection")
            .unwrap();

        assert_eq!(moved.label, "Move selection");
        assert_eq!(moved.entity_count, 2);
        assert!(matches!(
            &doc.entity(line).unwrap().kind,
            SketchEntityKind::Edge { a, b }
                if *a == Vec3::new(2.0, 3.0, 4.0) && *b == Vec3::new(6.0, 3.0, 4.0)
        ));
        assert!(matches!(
            &doc.entity(instance).unwrap().kind,
            SketchEntityKind::ComponentInstance { definition: got, transform }
                if *got == definition
                    && transform.translation == Vec3::new(2.0, 4.0, 4.0)
        ));

        let undone = doc.undo_last().expect("undo move");
        assert_eq!(undone.label, "Move selection");
        assert!(matches!(
            &doc.entity(line).unwrap().kind,
            SketchEntityKind::Edge { a, b } if *a == Vec3::ZERO && *b == Vec3::X * 4.0
        ));
        assert!(matches!(
            &doc.entity(instance).unwrap().kind,
            SketchEntityKind::ComponentInstance { transform, .. }
                if transform.translation == Vec3::Y
        ));

        let redone = doc.redo_last().expect("redo move");
        assert_eq!(redone.entity_count, 2);
        assert!(matches!(
            &doc.entity(line).unwrap().kind,
            SketchEntityKind::Edge { a, .. } if *a == Vec3::new(2.0, 3.0, 4.0)
        ));
    }

    #[test]
    fn scale_selection_about_pivot_resizes_geometry_and_instances_with_undo() {
        let mut doc = SketchDocument::new();
        let face = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::X * 2.0,
                Vec3::Y,
                "Scale source",
            )
            .unwrap();
        let definition = doc.create_component_definition("Glass Panel").unwrap();
        let instance = doc
            .add_component_instance(definition, SketchTransform::from_translation(Vec3::X))
            .unwrap();
        let mut selection = SelectionSet::default();
        selection.select(face);
        selection.select(instance);

        let scaled = doc
            .scale_selection_about_pivot(
                &selection,
                Vec3::ZERO,
                Vec3::new(3.0, 2.0, 1.0),
                "Scale exact",
            )
            .unwrap();

        assert_eq!(scaled.label, "Scale exact");
        assert_eq!(scaled.entity_count, 2);
        assert!(matches!(
            &doc.entity(face).unwrap().kind,
            SketchEntityKind::Face { vertices, normal }
                if vertices[1] == Vec3::new(6.0, 0.0, 0.0)
                    && vertices[2] == Vec3::new(6.0, 2.0, 0.0)
                    && *normal == Vec3::Z
        ));
        assert!(matches!(
            &doc.entity(instance).unwrap().kind,
            SketchEntityKind::ComponentInstance { transform, .. }
                if transform.translation == Vec3::new(3.0, 0.0, 0.0)
                    && transform.scale == Vec3::new(3.0, 2.0, 1.0)
        ));

        let undone = doc.undo_last().expect("undo scale");
        assert_eq!(undone.label, "Scale exact");
        assert!(matches!(
            &doc.entity(face).unwrap().kind,
            SketchEntityKind::Face { vertices, .. }
                if vertices[1] == Vec3::new(2.0, 0.0, 0.0)
                    && vertices[2] == Vec3::new(2.0, 1.0, 0.0)
        ));
    }

    #[test]
    fn flip_selection_across_plane_mirrors_geometry_and_component_instances() {
        let mut doc = SketchDocument::new();
        let line = doc
            .draw_pencil_line(doc.active_context(), Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0))
            .unwrap();
        let definition = doc.create_component_definition("Door").unwrap();
        let instance = doc
            .add_component_instance(
                definition,
                SketchTransform::from_translation(Vec3::new(3.0, 0.0, 0.0)),
            )
            .unwrap();
        let mut selection = SelectionSet::default();
        selection.select(line);
        selection.select(instance);

        let flipped = doc
            .flip_selection_across_plane(&selection, Vec3::X, Vec3::X, "Flip red axis")
            .unwrap();

        assert_eq!(flipped.label, "Flip red axis");
        assert!(matches!(
            &doc.entity(line).unwrap().kind,
            SketchEntityKind::Edge { a, b }
                if *a == Vec3::new(2.0, 0.0, 0.0) && *b == Vec3::ZERO
        ));
        assert!(matches!(
            &doc.entity(instance).unwrap().kind,
            SketchEntityKind::ComponentInstance { transform, .. }
                if transform.translation == Vec3::new(-1.0, 0.0, 0.0)
                    && transform.scale == Vec3::new(-1.0, 1.0, 1.0)
        ));
    }

    #[test]
    fn scale_and_flip_reject_degenerate_inference_inputs() {
        let mut doc = SketchDocument::new();
        let line = doc
            .draw_pencil_line(doc.active_context(), Vec3::ZERO, Vec3::X)
            .unwrap();
        let mut selection = SelectionSet::default();
        selection.select(line);

        assert!(matches!(
            doc.scale_selection_about_pivot(&selection, Vec3::ZERO, Vec3::ZERO, "Bad scale"),
            Err(SketchModelError::InvalidGeometry(
                "Scale factors must be non-zero"
            ))
        ));
        assert!(matches!(
            doc.flip_selection_across_plane(&selection, Vec3::ZERO, Vec3::ZERO, "Bad flip"),
            Err(SketchModelError::InvalidGeometry(
                "Flip plane normal must be non-zero"
            ))
        ));
    }

    #[test]
    fn copy_selection_creates_linear_array_and_shared_component_definitions() {
        let mut doc = SketchDocument::new();
        let face = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::X * 4.0,
                Vec3::Y * 3.0,
                "Room face",
            )
            .unwrap();
        let extrusion = doc.push_pull_face(face, 3.0).unwrap();
        let definition = doc.create_component_definition("Glass Door").unwrap();
        let instance = doc
            .add_component_instance(definition, SketchTransform::from_translation(Vec3::Z))
            .unwrap();
        let mut selection = SelectionSet::default();
        selection.select(face);
        selection.select(extrusion);
        selection.select(instance);

        let copies = doc
            .copy_selection_linear_array(&selection, Vec3::new(10.0, 0.0, 0.0), 2, "Array copy")
            .unwrap();

        assert_eq!(copies.len(), 6);
        assert_eq!(doc.undo_count(), 3);
        let first_face_copy = copies[0];
        let first_extrusion_copy = copies[1];
        let first_instance_copy = copies[2];
        assert!(matches!(
            &doc.entity(first_face_copy).unwrap().kind,
            SketchEntityKind::Face { vertices, .. } if vertices[0] == Vec3::X * 10.0
        ));
        assert!(matches!(
            &doc.entity(first_extrusion_copy).unwrap().kind,
            SketchEntityKind::PushPullExtrusion { source_face, base_vertices, .. }
                if *source_face == first_face_copy && base_vertices[0] == Vec3::X * 10.0
        ));
        assert!(matches!(
            &doc.entity(first_instance_copy).unwrap().kind,
            SketchEntityKind::ComponentInstance { definition: got, transform }
                if *got == definition && transform.translation == Vec3::new(10.0, 0.0, 1.0)
        ));

        let undone = doc.undo_last().expect("undo array");
        assert_eq!(undone.label, "Array copy");
        assert_eq!(undone.entity_count, 6);
        assert!(copies.iter().all(|id| doc.entity(*id).is_none()));

        let redone = doc.redo_last().expect("redo array");
        assert_eq!(redone.entity_count, 6);
        assert!(copies.iter().all(|id| doc.entity(*id).is_some()));
    }

    #[test]
    fn selection_set_preserves_order_and_avoids_duplicates() {
        let a = SketchId::new_for_test(10);
        let b = SketchId::new_for_test(20);
        let mut selection = SelectionSet::default();

        selection.select(a);
        selection.select(b);
        selection.select(a);

        assert_eq!(selection.ordered(), &[a, b]);
        assert!(selection.contains(a));
        assert_eq!(selection.len(), 2);
    }

    #[test]
    fn tool_controller_selects_semantic_hits_with_replace_and_additive_modes() {
        let first = SketchId::new_for_test(10);
        let second = SketchId::new_for_test(20);
        let mut controller = ToolController::default();
        let first_hit = HitRecord::new(first, [], HitKind::Face, Vec3::ZERO, 1.0);
        let second_hit = HitRecord::new(second, [], HitKind::Face, Vec3::X, 2.0);

        assert_eq!(
            controller.select_hit(&first_hit, false),
            SelectionUpdate::Selected(first)
        );
        assert_eq!(controller.selection().ordered(), &[first]);
        let generation_after_first = controller.preview_generation();

        assert_eq!(
            controller.select_hit(&second_hit, true),
            SelectionUpdate::Selected(second)
        );
        assert_eq!(controller.selection().ordered(), &[first, second]);
        assert!(controller.preview_generation() > generation_after_first);

        assert_eq!(
            controller.select_hit(&first_hit, false),
            SelectionUpdate::Selected(first)
        );
        assert_eq!(controller.selection().ordered(), &[first]);
        assert!(controller
            .active_tool_hint()
            .contains("Selected Face entity 10"));
    }

    #[test]
    fn tool_controller_clear_selection_reports_when_it_changed_state() {
        let entity = SketchId::new_for_test(33);
        let mut controller = ToolController::default();
        let hit = HitRecord::new(entity, [], HitKind::Face, Vec3::ZERO, 1.0);
        controller.select_hit(&hit, false);
        let generation_after_select = controller.preview_generation();

        assert_eq!(controller.clear_selection(), SelectionUpdate::Cleared);
        assert!(controller.selection().is_empty());
        assert!(controller.preview_generation() > generation_after_select);
        assert_eq!(controller.clear_selection(), SelectionUpdate::Ignored);
    }

    #[test]
    fn pick_service_ranks_raw_hits_before_inference_is_applied() {
        let near_face = HitRecord::new(
            SketchId::new_for_test(20),
            [],
            HitKind::Face,
            Vec3::new(0.5, 0.5, 0.0),
            1.0,
        );
        let far_vertex = HitRecord::new(
            SketchId::new_for_test(10),
            [],
            HitKind::Vertex,
            Vec3::ZERO,
            8.0,
        );

        let best = PickService::best([far_vertex.clone(), near_face.clone()]).unwrap();

        assert_eq!(best.entity, near_face.entity);
        assert_eq!(best.kind, HitKind::Face);
    }

    #[test]
    fn pick_service_uses_hit_kind_only_as_same_distance_tiebreaker() {
        let face = HitRecord::new(
            SketchId::new_for_test(30),
            [],
            HitKind::Face,
            Vec3::new(1.0, 1.0, 0.0),
            2.0,
        );
        let vertex = HitRecord::new(
            SketchId::new_for_test(40),
            [],
            HitKind::Vertex,
            Vec3::new(1.0, 1.0, 0.0),
            2.0,
        );

        let best = PickService::best([face, vertex]).unwrap();

        assert_eq!(best.entity, SketchId::new_for_test(40));
        assert_eq!(best.kind, HitKind::Vertex);
    }

    #[test]
    fn inference_service_builds_input_point_candidates_from_pick() {
        let mut doc = SketchDocument::new();
        let face = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::X * 4.0,
                Vec3::Y * 3.0,
                "Rectangle",
            )
            .unwrap();
        let hit = HitRecord::new(face, [], HitKind::Face, Vec3::new(2.0, 1.5, 0.0), 1.0)
            .with_normal(Vec3::Z);

        let candidates = InferenceService::from_pick(&doc, &hit, None).unwrap();
        let kinds: BTreeSet<_> = candidates.iter().map(|candidate| candidate.kind).collect();

        assert!(kinds.contains(&InferenceKind::Endpoint));
        assert!(kinds.contains(&InferenceKind::Midpoint));
        assert!(kinds.contains(&InferenceKind::FaceCenter));
        assert!(kinds.contains(&InferenceKind::OnFace));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.kind == InferenceKind::OnFace
                && candidate.point == Vec3::new(2.0, 1.5, 0.0)
                && candidate.plane_normal == Some(Vec3::Z)));
    }

    #[test]
    fn inference_service_adds_reference_axis_and_from_point_candidates() {
        let mut doc = SketchDocument::new();
        let face = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::X * 4.0,
                Vec3::Y * 3.0,
                "Rectangle",
            )
            .unwrap();
        let hit = HitRecord::new(face, [], HitKind::Face, Vec3::new(4.0, 2.0, 0.0), 1.0)
            .with_normal(Vec3::Z);

        let candidates =
            InferenceService::from_pick(&doc, &hit, Some(Vec3::new(1.0, 2.0, 0.0))).unwrap();

        assert!(candidates.iter().any(|candidate| {
            candidate.kind == InferenceKind::AxisX
                && candidate.point == Vec3::new(4.0, 2.0, 0.0)
                && candidate.direction == Some(Vec3::X)
        }));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.kind == InferenceKind::FromPoint
                && candidate.point == Vec3::new(4.0, 2.0, 0.0)));
    }

    #[test]
    fn project_world_to_screen_maps_ndc_to_viewport_pixels() {
        let projection =
            project_world_to_screen(Vec3::ZERO, Mat4::IDENTITY, Vec2::new(200.0, 100.0))
                .expect("projection");

        assert_eq!(projection.screen, Vec2::new(100.0, 50.0));
        assert_eq!(projection.ndc, Vec3::ZERO);
    }

    #[test]
    fn screen_space_inference_prefers_endpoint_priority_inside_snap_radius() {
        let endpoint = InferenceCandidate::new(
            InferenceKind::Endpoint,
            Vec3::new(0.10, 0.0, 0.0),
            1.0,
            "Endpoint",
        );
        let face = InferenceCandidate::new(InferenceKind::OnFace, Vec3::ZERO, 1.0, "On face");

        let best = best_screen_space_inference(
            [face, endpoint],
            Vec2::new(100.0, 100.0),
            Mat4::IDENTITY,
            Vec2::new(200.0, 200.0),
            ScreenSpaceSnapSettings::default(),
            None,
        )
        .expect("best candidate");

        assert_eq!(best.inference.kind, InferenceKind::Endpoint);
    }

    #[test]
    fn screen_space_inference_sticky_candidate_can_survive_small_distance_loss() {
        let midpoint = InferenceCandidate::new(
            InferenceKind::Midpoint,
            Vec3::new(0.12, 0.0, 0.0),
            1.0,
            "Midpoint",
        );
        let edge = InferenceCandidate::new(InferenceKind::OnEdge, Vec3::ZERO, 1.0, "On edge");

        let best = best_screen_space_inference(
            [edge, midpoint],
            Vec2::new(100.0, 100.0),
            Mat4::IDENTITY,
            Vec2::new(200.0, 200.0),
            ScreenSpaceSnapSettings::default(),
            Some(InferenceKind::Midpoint),
        )
        .expect("best candidate");

        assert_eq!(best.inference.kind, InferenceKind::Midpoint);
        assert!(best.sticky);
    }

    #[test]
    fn locked_axis_projection_uses_skew_line_math_from_camera_ray() {
        let locked = closest_point_on_locked_axis_from_ray(
            Vec3::new(4.0, 5.0, 10.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::X,
        )
        .expect("locked point");

        assert!((locked - Vec3::new(4.0, 2.0, 3.0)).length() < 1.0e-5);
    }

    #[test]
    fn locked_axis_projection_falls_back_when_ray_parallel_to_axis() {
        let locked = closest_point_on_locked_axis_from_ray(
            Vec3::new(6.0, 2.0, 0.0),
            Vec3::X,
            Vec3::new(1.0, 2.0, 0.0),
            Vec3::X,
        )
        .expect("locked point");

        assert_eq!(locked, Vec3::new(6.0, 2.0, 0.0));
    }

    #[test]
    fn rectangle_plane_inherits_hovered_face_normal_before_view_axis() {
        let plane = rectangle_plane_from_view_or_face(Vec3::ZERO, Vec3::Z, Some(Vec3::Y), None)
            .expect("face plane");

        assert_eq!(plane.normal, Vec3::Y);
        assert!(plane.axis_u.dot(plane.normal).abs() < 1.0e-5);
        assert!(plane.axis_v.dot(plane.normal).abs() < 1.0e-5);
        assert!(plane.axis_u.dot(plane.axis_v).abs() < 1.0e-5);
    }

    #[test]
    fn rectangle_plane_uses_dominant_view_axis_in_empty_space() {
        let plane =
            rectangle_plane_from_view_or_face(Vec3::ZERO, Vec3::new(0.2, -0.3, 0.9), None, None)
                .expect("view plane");

        assert_eq!(plane.normal, Vec3::Z);
    }

    #[test]
    fn rectangle_plane_arrow_lock_overrides_face_and_view_axis() {
        let plane =
            rectangle_plane_from_view_or_face(Vec3::ZERO, Vec3::Z, Some(Vec3::Y), Some(Vec3::X))
                .expect("locked plane");

        assert_eq!(plane.normal, Vec3::X);
    }

    #[test]
    fn inference_service_prefers_endpoint_over_weaker_face_hit() {
        let endpoint = InferenceCandidate::new(
            InferenceKind::Endpoint,
            Vec3::new(1.0, 2.0, 3.0),
            0.85,
            "Endpoint",
        );
        let face = InferenceCandidate::new(
            InferenceKind::OnFace,
            Vec3::new(1.2, 2.0, 3.0),
            0.40,
            "On Face",
        );

        let best = InferenceService::best([face, endpoint]).unwrap();

        assert_eq!(best.kind, InferenceKind::Endpoint);
        assert_eq!(best.tooltip, "Endpoint");
    }

    #[test]
    fn inference_lock_projects_raw_points_to_locked_axis() {
        let lock =
            InferenceLock::axis(InferenceKind::AxisX, Vec3::new(1.0, 2.0, 3.0)).expect("axis lock");

        let candidate = lock.apply(Vec3::new(6.0, 9.0, -4.0));

        assert_eq!(candidate.kind, InferenceKind::AxisX);
        assert_eq!(candidate.point, Vec3::new(6.0, 2.0, 3.0));
        assert_eq!(candidate.direction, Some(Vec3::X));
        assert_eq!(candidate.tooltip, "Red axis locked");
    }

    #[test]
    fn inference_lock_projects_from_reference_point_direction() {
        let lock =
            InferenceLock::from_point(Vec3::new(2.0, 2.0, 0.0), Vec3::Y).expect("from-point lock");

        let candidate = lock.apply(Vec3::new(9.0, 8.0, 3.0));

        assert_eq!(candidate.kind, InferenceKind::FromPoint);
        assert_eq!(candidate.point, Vec3::new(2.0, 8.0, 0.0));
        assert_eq!(candidate.direction, Some(Vec3::Y));
        assert_eq!(candidate.tooltip, "From point locked");
    }

    #[test]
    fn inference_lock_rejects_zero_reference_direction() {
        assert_eq!(InferenceLock::from_point(Vec3::ZERO, Vec3::ZERO), None);
    }

    #[test]
    fn inference_lock_projects_raw_points_to_locked_face_plane() {
        let lock = InferenceLock::plane(InferenceKind::OnFace, Vec3::new(0.0, 5.0, 0.0), Vec3::Y)
            .expect("face-plane lock");

        let candidate = lock.apply(Vec3::new(3.0, 9.0, -2.0));

        assert_eq!(candidate.kind, InferenceKind::OnFace);
        assert_eq!(candidate.point, Vec3::new(3.0, 5.0, -2.0));
        assert_eq!(candidate.plane_normal, Some(Vec3::Y));
        assert_eq!(candidate.tooltip, "On face locked");
    }

    #[test]
    fn inference_lock_rejects_zero_plane_normal() {
        assert_eq!(
            InferenceLock::plane(InferenceKind::OnFace, Vec3::ZERO, Vec3::ZERO),
            None
        );
    }

    #[test]
    fn face_inference_candidates_include_on_face_and_on_edge_hints() {
        let mut doc = SketchDocument::new();
        let face = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::X * 4.0,
                Vec3::Y * 3.0,
                "Rectangle",
            )
            .unwrap();

        let candidates = doc.entity_inference_candidates(face).unwrap();
        let kinds: BTreeSet<_> = candidates.iter().map(|candidate| candidate.kind).collect();
        let on_face = candidates
            .iter()
            .find(|candidate| candidate.kind == InferenceKind::OnFace)
            .expect("on-face candidate");
        let on_edge = candidates
            .iter()
            .find(|candidate| candidate.kind == InferenceKind::OnEdge)
            .expect("on-edge candidate");

        assert!(kinds.contains(&InferenceKind::OnFace));
        assert!(kinds.contains(&InferenceKind::OnEdge));
        assert_eq!(on_face.point, Vec3::new(2.0, 1.5, 0.0));
        assert_eq!(on_face.plane_normal, Some(Vec3::Z));
        assert_eq!(on_edge.direction, Some(Vec3::X));
        assert_eq!(on_edge.plane_normal, Some(Vec3::Z));
    }

    #[test]
    fn tool_controller_clears_inference_lock_when_switching_tools() {
        let mut controller = ToolController::default();
        controller.activate(EditorToolId::Pencil);
        let lock = InferenceLock::axis(InferenceKind::AxisY, Vec3::ZERO).expect("axis lock");

        controller.lock_inference(lock);
        assert_eq!(controller.inference_lock(), Some(lock));
        controller.begin_transaction("Pencil preview");
        assert_eq!(controller.inference_lock(), Some(lock));

        controller.activate(EditorToolId::Rectangle);

        assert_eq!(controller.active_tool(), EditorToolId::Rectangle);
        assert_eq!(controller.inference_lock(), None);
    }

    #[test]
    fn tool_controller_projects_points_through_locked_inference() {
        let mut controller = ToolController::default();
        controller.activate(EditorToolId::Pencil);
        controller.begin_transaction("Pencil preview");
        let lock =
            InferenceLock::from_point(Vec3::new(3.0, 0.0, 3.0), Vec3::Z).expect("from-point lock");

        controller.lock_inference(lock);
        let projected = controller
            .project_locked_inference(Vec3::new(9.0, 4.0, 10.0))
            .expect("projected lock candidate");

        assert_eq!(projected.kind, InferenceKind::FromPoint);
        assert_eq!(projected.point, Vec3::new(3.0, 0.0, 10.0));
        assert!(controller.active_tool_hint().contains("From point locked"));
    }

    #[test]
    fn hit_record_keeps_instance_path_for_nested_picks() {
        let root = SketchId::new_for_test(1);
        let nested = SketchId::new_for_test(2);
        let face = SketchId::new_for_test(3);
        let hit = HitRecord::new(
            face,
            [root, nested],
            HitKind::Face,
            Vec3::new(4.0, 5.0, 6.0),
            12.0,
        );

        assert_eq!(hit.entity, face);
        assert_eq!(hit.instance_path, vec![root, nested]);
        assert_eq!(hit.kind, HitKind::Face);
    }

    #[test]
    fn tool_controller_transactions_group_touched_entities() {
        let a = SketchId::new_for_test(1);
        let b = SketchId::new_for_test(2);
        let mut controller = ToolController::default();

        controller.activate(EditorToolId::Pencil);
        controller.begin_transaction("Draw wall");
        controller.touch_entity(a);
        controller.touch_entity(b);
        controller.touch_entity(a);
        let committed = controller.commit_transaction().unwrap();

        assert_eq!(controller.active_tool(), EditorToolId::Pencil);
        assert_eq!(committed.label, "Draw wall");
        assert_eq!(committed.touched, vec![a, b]);
        assert_eq!(controller.last_transaction_label(), Some("Draw wall"));
    }

    #[test]
    fn tool_controller_exposes_open_transaction_label_for_status_ui() {
        let mut controller = ToolController::default();

        assert_eq!(controller.open_transaction_label(), None);
        controller.begin_transaction("House footprint");

        assert_eq!(controller.open_transaction_label(), Some("House footprint"));
    }

    #[test]
    fn default_editor_tool_catalog_exposes_lifecycle_for_house_builder_tools() {
        let catalog = EditorToolCatalog::default();

        for tool in [
            EditorToolId::Select,
            EditorToolId::Pencil,
            EditorToolId::Rectangle,
            EditorToolId::House,
            EditorToolId::PushPull,
            EditorToolId::Room,
            EditorToolId::CutOpening,
            EditorToolId::Road,
            EditorToolId::BotArea,
            EditorToolId::Material,
        ] {
            let definition = catalog.definition(tool).expect("built-in editor tool");
            assert_eq!(definition.id, tool);
            assert!(!definition.label.is_empty());
            assert!(!definition.begin_hint.is_empty());
            assert!(!definition.preview_hint.is_empty());
            assert!(!definition.commit_label.is_empty());
            assert!(!definition.cancel_hint.is_empty());
        }

        let pencil = catalog.definition(EditorToolId::Pencil).unwrap();
        assert!(pencil.uses_inference);
        assert!(pencil.supports_typed_measurement);
        assert_eq!(pencil.label, "PENCIL");

        let opening = catalog.definition(EditorToolId::CutOpening).unwrap();
        assert!(opening.preview_hint.contains("wall"));
        assert!(opening.commit_label.contains("Opening"));
    }

    #[test]
    fn tool_controller_lifecycle_tracks_preview_commit_and_cancel() {
        let mut controller = ToolController::default();

        controller.activate(EditorToolId::Pencil);
        assert_eq!(controller.tool_phase(), EditorToolPhase::Idle);
        assert_eq!(controller.active_tool_label(), "PENCIL");
        assert!(controller.active_tool_hint().contains("endpoint"));

        controller.begin_transaction("Pencil line");
        assert_eq!(controller.tool_phase(), EditorToolPhase::Previewing);
        assert_eq!(controller.open_transaction_label(), Some("Pencil line"));
        assert!(controller.active_tool_hint().contains("snapped"));

        let committed = controller.commit_transaction().expect("commit");
        assert_eq!(committed.label, "Pencil line");
        assert_eq!(controller.tool_phase(), EditorToolPhase::Committed);
        assert_eq!(controller.last_transaction_label(), Some("Pencil line"));
        assert!(controller.active_tool_hint().contains("Pencil line"));

        controller.begin_transaction("Rectangle preview");
        assert_eq!(controller.tool_phase(), EditorToolPhase::Previewing);
        assert!(controller.cancel_active_operation(EditorCancelReason::Escape));
        assert_eq!(controller.tool_phase(), EditorToolPhase::Cancelled);
        assert_eq!(
            controller.last_cancelled_transaction_label(),
            Some("Rectangle preview")
        );
        assert!(controller.active_tool_hint().contains("cancel"));
    }

    #[test]
    fn tool_switch_cancels_active_preview_and_resets_new_tool_lifecycle() {
        let mut controller = ToolController::default();

        controller.activate(EditorToolId::Rectangle);
        controller.begin_transaction("Rectangle preview");
        controller.activate(EditorToolId::PushPull);

        assert_eq!(controller.active_tool(), EditorToolId::PushPull);
        assert_eq!(controller.open_transaction_label(), None);
        assert_eq!(
            controller.last_cancelled_transaction_label(),
            Some("Rectangle preview")
        );
        assert_eq!(controller.tool_phase(), EditorToolPhase::Idle);
        assert_eq!(controller.active_tool_label(), "PUSH/PULL");
        assert!(controller.active_tool_hint().contains("face"));
    }

    #[test]
    fn inference_kind_provides_shared_sketchup_tooltips() {
        assert_eq!(InferenceKind::Endpoint.tooltip(), "Endpoint");
        assert_eq!(InferenceKind::Midpoint.tooltip(), "Midpoint");
        assert_eq!(InferenceKind::FaceCenter.tooltip(), "Face center");
    }

    #[test]
    fn tags_materials_styles_and_scenes_are_first_class_side_tables() {
        let mut doc = SketchDocument::new();
        let wall = doc
            .add_entity_to_active(SketchEntityKind::Face {
                vertices: vec![Vec3::ZERO, Vec3::X, Vec3::X + Vec3::Y],
                normal: Vec3::Z,
            })
            .unwrap();
        let facade = doc.create_tag("Facade").unwrap();
        let glass = doc
            .create_material("Blue glass", SketchColor::rgba(80, 190, 255, 180))
            .unwrap();
        let style = doc.create_style("Blueprint glass").unwrap();

        doc.assign_entity_tag(wall, facade).unwrap();
        doc.assign_entity_material(wall, glass).unwrap();
        doc.set_active_style(style).unwrap();
        doc.set_tag_visibility(facade, false).unwrap();

        assert_eq!(doc.entity(wall).unwrap().tag, Some(facade));
        assert_eq!(doc.entity(wall).unwrap().material, Some(glass));
        assert_eq!(doc.material(glass).unwrap().color.a, 180);
        assert_eq!(doc.active_style(), style);
        assert!(!doc.entity_effective_visible(wall).unwrap());
        assert_eq!(
            doc.context(doc.active_context()).unwrap().entities,
            vec![wall],
            "tags control visibility, not geometric ownership"
        );

        let scene = doc
            .capture_scene(
                "Facade hidden",
                Some(SketchCamera {
                    eye: Vec3::new(4.0, 5.0, 6.0),
                    target: Vec3::ZERO,
                    up: Vec3::Y,
                }),
            )
            .unwrap();
        doc.set_tag_visibility(facade, true).unwrap();
        doc.apply_scene(scene).unwrap();

        assert_eq!(doc.active_scene(), Some(scene));
        assert!(!doc.tag_visible(facade).unwrap());
        assert_eq!(doc.active_style(), style);
    }

    #[test]
    fn stable_scene_style_snapshot_roundtrips_presentation_state() {
        let mut doc = SketchDocument::new();
        let facade = doc.create_tag("Facade").unwrap();
        let glass = doc
            .create_material("Blue glass", SketchColor::rgba(80, 190, 255, 180))
            .unwrap();
        let style = doc.create_style("Blueprint glass").unwrap();
        let face = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::X * 8.0,
                Vec3::Y * 4.0,
                "Saved wall face",
            )
            .unwrap();
        doc.assign_entity_tag(face, facade).unwrap();
        doc.assign_entity_material(face, glass).unwrap();
        doc.set_active_style(style).unwrap();
        doc.set_tag_visibility(facade, false).unwrap();
        doc.set_model_attribute("voxel_native", "presentation", "scene_style")
            .unwrap();
        let scene = doc
            .capture_scene(
                "Facade hidden",
                Some(SketchCamera {
                    eye: Vec3::new(10.0, 7.0, 12.0),
                    target: Vec3::new(2.0, 0.0, 1.0),
                    up: Vec3::Y,
                }),
            )
            .unwrap();
        doc.set_tag_visibility(facade, true).unwrap();
        doc.apply_scene(scene).unwrap();

        let ron = doc.to_stable_ron().expect("stable RON");

        assert!(ron.contains("version: 1"));
        assert!(ron.contains("Blueprint glass"));
        assert!(ron.contains("Facade hidden"));
        assert!(ron.contains("Blue glass"));

        let restored = SketchDocument::from_stable_ron(&ron).expect("restore stable RON");

        assert_eq!(restored.active_scene(), Some(scene));
        assert_eq!(restored.active_style(), style);
        assert_eq!(restored.default_tag_name(), Some("Untagged"));
        assert_eq!(
            restored.model_attribute("voxel_native", "presentation"),
            Some("scene_style")
        );
        assert!(!restored.tag_visible(facade).unwrap());
        assert_eq!(restored.entity(face).unwrap().material, Some(glass));
        assert_eq!(
            restored.scene(scene).unwrap().camera.as_ref().unwrap().eye,
            Vec3::new(10.0, 7.0, 12.0)
        );
    }

    #[test]
    fn attributes_are_namespaced_for_model_and_entity_metadata() {
        let mut doc = SketchDocument::new();
        let entity = doc
            .add_entity_to_active(SketchEntityKind::Edge {
                a: Vec3::ZERO,
                b: Vec3::X,
            })
            .unwrap();

        doc.set_model_attribute("voxel_native", "kernel", "dual")
            .unwrap();
        doc.set_entity_attribute(entity, "city_builder", "role", "window_bay")
            .unwrap();

        assert_eq!(doc.model_attribute("voxel_native", "kernel"), Some("dual"));
        assert_eq!(
            doc.entity_attribute(entity, "city_builder", "role")
                .unwrap(),
            Some("window_bay")
        );
    }

    #[test]
    fn extension_registry_exposes_commands_tools_and_io_formats() {
        let mut registry = SketchCommandRegistry::empty();
        let extension =
            SketchExtensionManifest::new("voxel_native.arch", "Voxel Architecture Tools", "0.1.0");
        registry.register_extension(extension.clone()).unwrap();
        registry
            .register_tool(SketchToolDescriptor::new(
                "voxel_native.arch.line",
                "Line",
                EditorToolId::Pencil,
                Some(extension.id.clone()),
            ))
            .unwrap();
        registry
            .register_command(SketchCommandDescriptor::new(
                "voxel_native.arch.draw_line",
                "Draw Line",
                Some(EditorToolId::Pencil),
                Some(extension.id.clone()),
            ))
            .unwrap();
        registry
            .register_importer(SketchIoFormat::new("gltf", "glTF scene", true))
            .unwrap();
        registry
            .register_exporter(SketchIoFormat::new("obj", "OBJ mesh", false))
            .unwrap();

        assert_eq!(
            registry.tool("voxel_native.arch.line").unwrap().editor_tool,
            EditorToolId::Pencil
        );
        assert_eq!(
            registry
                .command("voxel_native.arch.draw_line")
                .unwrap()
                .tool,
            Some(EditorToolId::Pencil)
        );
        assert!(registry.importer("gltf").unwrap().preserves_semantics);
        assert!(!registry.exporter("obj").unwrap().preserves_semantics);
    }

    #[test]
    fn default_registry_seeds_builtin_editor_tools_and_neutral_io_strategy() {
        let registry = SketchCommandRegistry::default();

        assert_eq!(
            registry.command("editor.pencil").unwrap().tool,
            Some(EditorToolId::Pencil)
        );
        assert_eq!(
            registry.tool("editor.push_pull").unwrap().editor_tool,
            EditorToolId::PushPull
        );
        assert!(registry.importer("gltf").unwrap().preserves_semantics);
        assert!(!registry.exporter("stl").unwrap().preserves_semantics);
    }

    #[test]
    fn tool_controller_house_workflow_sets_guided_stage_and_material() {
        let wall_material = SketchId::new_for_test(42);
        let mut controller = ToolController::default();

        controller.start_house_workflow(wall_material);

        let guide = controller.house_guide().expect("house guide");
        assert_eq!(controller.active_tool(), EditorToolId::House);
        assert_eq!(guide.stage, HouseBuildStage::Footprint);
        assert_eq!(guide.material, wall_material);
        assert!(guide.status().contains("Footprint"));
        assert!(guide.status().contains("Opening"));
    }

    #[test]
    fn escape_cancels_active_editor_operation_but_right_mouse_only_orbits() {
        let mut controller = ToolController::default();
        controller.activate(EditorToolId::Rectangle);
        controller.begin_transaction("Rectangle preview");
        let before_orbit = controller.preview_generation();

        assert_eq!(
            controller.handle_right_mouse_orbit(),
            ToolInputEffect::OrbitOnly
        );
        assert_eq!(
            controller.open_transaction_label(),
            Some("Rectangle preview")
        );
        assert_eq!(controller.preview_generation(), before_orbit);

        assert!(controller.cancel_active_operation(EditorCancelReason::Escape));
        assert_eq!(controller.open_transaction_label(), None);
        assert!(controller.preview_generation() > before_orbit);
    }

    #[test]
    fn rectangle_faces_support_floor_roof_and_vertical_wall_planes() {
        let mut doc = SketchDocument::new();

        let floor = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::X * 8.0,
                Vec3::Y * 6.0,
                "Rectangle floor",
            )
            .unwrap();
        let wall = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::Z * 4.0,
                Vec3::Y * 6.0,
                "Rectangle wall",
            )
            .unwrap();

        let SketchEntityKind::Face {
            normal: floor_normal,
            ..
        } = &doc.entity(floor).unwrap().kind
        else {
            panic!("floor should be a face");
        };
        let SketchEntityKind::Face {
            vertices,
            normal: wall_normal,
        } = &doc.entity(wall).unwrap().kind
        else {
            panic!("wall should be a face");
        };

        assert_eq!(*floor_normal, Vec3::Z);
        assert_eq!(vertices.len(), 4);
        assert_eq!(*wall_normal, -Vec3::X);
    }

    #[test]
    fn drafting_essentials_catalog_exposes_circle_polygon_arc_and_freehand() {
        let catalog = EditorToolCatalog::default();
        let registry = SketchCommandRegistry::default();

        for (tool, command_id, label) in [
            (EditorToolId::Circle, "editor.circle", "CIRCLE"),
            (EditorToolId::Polygon, "editor.polygon", "POLYGON"),
            (EditorToolId::Arc, "editor.arc", "ARC"),
            (EditorToolId::Freehand, "editor.freehand", "FREEHAND"),
        ] {
            let definition = catalog
                .definition(tool)
                .expect("drafting primitive should be in the built-in tool catalog");
            assert_eq!(definition.label, label);
            assert!(definition.uses_inference);
            assert!(definition.supports_typed_measurement);
            assert_eq!(
                registry
                    .tool(command_id)
                    .map(|descriptor| descriptor.editor_tool),
                Some(tool)
            );
            assert_eq!(
                registry
                    .command(command_id)
                    .and_then(|descriptor| descriptor.tool),
                Some(tool)
            );
        }
    }

    #[test]
    fn circle_polygon_arc_and_freehand_create_semantic_drafting_entities() {
        let mut doc = SketchDocument::new();

        let circle = doc
            .draw_circle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::Z,
                4.0,
                24,
                "Circle face",
            )
            .unwrap();
        let polygon = doc
            .draw_polygon_face(
                doc.active_context(),
                Vec3::new(12.0, 0.0, 0.0),
                Vec3::Z,
                3.0,
                6,
                "Polygon face",
            )
            .unwrap();
        let arc = doc
            .draw_arc_curve(
                doc.active_context(),
                Vec3::new(0.0, 10.0, 0.0),
                Vec3::Z,
                5.0,
                Vec3::X,
                std::f32::consts::FRAC_PI_2,
                8,
                "Arc curve",
            )
            .unwrap();
        let freehand = doc
            .draw_freehand_curve(
                doc.active_context(),
                [
                    Vec3::new(0.0, 0.0, 1.0),
                    Vec3::new(1.0, 0.5, 1.0),
                    Vec3::new(2.0, 0.25, 1.0),
                ],
                "Freehand curve",
            )
            .unwrap();

        assert!(matches!(
            &doc.entity(circle).unwrap().kind,
            SketchEntityKind::CircleFace {
                radius,
                segments: 24,
                vertices,
                ..
            } if (*radius - 4.0).abs() < f32::EPSILON && vertices.len() == 24
        ));
        assert!(matches!(
            &doc.entity(polygon).unwrap().kind,
            SketchEntityKind::PolygonFace {
                sides: 6,
                vertices,
                ..
            } if vertices.len() == 6
        ));
        assert!(matches!(
            &doc.entity(arc).unwrap().kind,
            SketchEntityKind::ArcCurve {
                sweep_radians,
                points,
                ..
            } if (*sweep_radians - std::f32::consts::FRAC_PI_2).abs() < f32::EPSILON
                && points.len() == 9
        ));
        assert!(matches!(
            &doc.entity(freehand).unwrap().kind,
            SketchEntityKind::FreehandCurve { points } if points.len() == 3
        ));

        let circle_kinds: BTreeSet<_> = doc
            .entity_inference_candidates(circle)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.kind)
            .collect();
        let arc_kinds: BTreeSet<_> = doc
            .entity_inference_candidates(arc)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.kind)
            .collect();
        assert!(circle_kinds.contains(&InferenceKind::Endpoint));
        assert!(circle_kinds.contains(&InferenceKind::Midpoint));
        assert!(circle_kinds.contains(&InferenceKind::FaceCenter));
        assert!(circle_kinds.contains(&InferenceKind::OnFace));
        assert!(arc_kinds.contains(&InferenceKind::Endpoint));
        assert!(arc_kinds.contains(&InferenceKind::Midpoint));
        assert!(arc_kinds.contains(&InferenceKind::OnEdge));

        let snapshot = doc.to_stable_ron().expect("serialize semantic primitives");
        let restored =
            SketchDocument::from_stable_ron(&snapshot).expect("restore semantic primitives");
        assert!(matches!(
            &restored.entity(circle).unwrap().kind,
            SketchEntityKind::CircleFace { segments: 24, .. }
        ));
        assert!(matches!(
            &restored.entity(freehand).unwrap().kind,
            SketchEntityKind::FreehandCurve { points } if points.len() == 3
        ));

        assert_eq!(doc.undo_count(), 4);
        for expected in ["Freehand curve", "Arc curve", "Polygon face", "Circle face"] {
            assert_eq!(doc.undo_last().unwrap().label, expected);
        }
        assert_eq!(doc.redo_count(), 4);
    }

    #[test]
    fn pencil_and_rectangle_share_endpoint_midpoint_face_center_inference() {
        let mut doc = SketchDocument::new();
        let line = doc
            .draw_pencil_line(doc.active_context(), Vec3::ZERO, Vec3::new(6.0, 0.0, 0.0))
            .unwrap();
        let face = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::X * 4.0,
                Vec3::Y * 3.0,
                "Rectangle",
            )
            .unwrap();

        let line_kinds: BTreeSet<_> = doc
            .entity_inference_candidates(line)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.kind)
            .collect();
        let face_kinds: BTreeSet<_> = doc
            .entity_inference_candidates(face)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.kind)
            .collect();

        assert!(line_kinds.contains(&InferenceKind::Endpoint));
        assert!(line_kinds.contains(&InferenceKind::Midpoint));
        assert!(line_kinds.contains(&InferenceKind::OnEdge));
        assert!(face_kinds.contains(&InferenceKind::Endpoint));
        assert!(face_kinds.contains(&InferenceKind::Midpoint));
        assert!(face_kinds.contains(&InferenceKind::FaceCenter));
        assert!(face_kinds.contains(&InferenceKind::OnEdge));
        assert!(face_kinds.contains(&InferenceKind::OnFace));
    }

    #[test]
    fn opening_room_and_pushpull_create_undoable_house_semantics() {
        let mut doc = SketchDocument::new();
        let pencil = doc
            .draw_pencil_line(
                doc.active_context(),
                Vec3::new(-1.0, 0.0, 0.0),
                Vec3::new(-1.0, 0.0, 4.0),
            )
            .unwrap();
        let wall = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::X * 8.0,
                Vec3::Z * 4.0,
                "Wall face",
            )
            .unwrap();

        let extrusion = doc.push_pull_face(wall, 6.0).unwrap();
        let opening = doc
            .cut_opening_through_face(
                wall,
                Vec3::new(3.0, 0.0, 1.5),
                Vec3::new(2.0, 0.0, 3.0),
                1.25,
            )
            .unwrap();
        let room = doc.create_hollow_room(wall, 0.35, 6.0).unwrap();

        assert!(matches!(
            &doc.entity(extrusion).unwrap().kind,
            SketchEntityKind::PushPullExtrusion {
                source_face,
                depth,
                ..
            } if *source_face == wall && (*depth - 6.0).abs() < f32::EPSILON
        ));
        assert!(matches!(
            &doc.entity(opening).unwrap().kind,
            SketchEntityKind::Opening {
                host,
                through_depth,
                ..
            } if *host == wall && (*through_depth - 1.25).abs() < f32::EPSILON
        ));
        let SketchEntityKind::Room {
            shell,
            shell_bounds,
            interior_bounds,
            wall_thickness,
        } = &doc.entity(room).unwrap().kind
        else {
            panic!("room tool should create room semantics");
        };
        assert_eq!(*shell, wall);
        assert!((*wall_thickness - 0.35).abs() < f32::EPSILON);
        assert!(
            interior_bounds.size().x < shell_bounds.size().x
                && interior_bounds.size().z < shell_bounds.size().z,
            "room must preserve an outer shell instead of deleting the entire wall mass"
        );
        assert!(
            doc.entity(pencil).is_some(),
            "pencil edge remains selectable"
        );
        assert!(
            doc.entity(wall).is_some(),
            "host wall face remains selectable"
        );

        assert_eq!(doc.undo_count(), 5);
        assert_eq!(doc.redo_count(), 0);
        for expected_label in [
            "Room hollow",
            "Opening cut",
            "Push/Pull",
            "Wall face",
            "Pencil line",
        ] {
            let undone = doc.undo_last().expect("undo step");
            assert_eq!(undone.label, expected_label);
        }
        assert!(doc.entity(wall).is_none());
        assert_eq!(doc.undo_count(), 0);
        assert_eq!(doc.redo_count(), 5);

        for expected_label in [
            "Pencil line",
            "Wall face",
            "Push/Pull",
            "Opening cut",
            "Room hollow",
        ] {
            let redone = doc.redo_last().expect("redo step");
            assert_eq!(redone.label, expected_label);
        }
        assert!(doc.entity(wall).is_some());
        assert!(doc.entity(room).is_some());
    }

    #[test]
    fn planar_graph_reconstructs_closed_rectangle_face_from_edges() {
        let mut doc = SketchDocument::new();
        for (a, b) in [
            (Vec3::ZERO, Vec3::X * 4.0),
            (Vec3::X * 4.0, Vec3::new(4.0, 3.0, 0.0)),
            (Vec3::new(4.0, 3.0, 0.0), Vec3::Y * 3.0),
            (Vec3::Y * 3.0, Vec3::ZERO),
        ] {
            doc.add_entity_to_active(SketchEntityKind::Edge { a, b })
                .unwrap();
        }

        let faces = doc
            .reconstruct_planar_faces(doc.active_context(), Vec3::Z)
            .unwrap();

        assert_eq!(faces.len(), 1);
        let face = doc.entity(faces[0]).unwrap();
        let SketchEntityKind::Face { vertices, normal } = &face.kind else {
            panic!("reconstructed entity should be a face");
        };
        assert_eq!(vertices.len(), 4);
        assert_eq!(*normal, Vec3::Z);
    }

    #[test]
    fn planar_graph_splits_rectangle_when_diagonal_edge_exists() {
        let mut doc = SketchDocument::new();
        let p00 = Vec3::ZERO;
        let p40 = Vec3::X * 4.0;
        let p43 = Vec3::new(4.0, 3.0, 0.0);
        let p03 = Vec3::Y * 3.0;
        for (a, b) in [(p00, p40), (p40, p43), (p43, p03), (p03, p00), (p00, p43)] {
            doc.add_entity_to_active(SketchEntityKind::Edge { a, b })
                .unwrap();
        }

        let faces = doc
            .reconstruct_planar_faces(doc.active_context(), Vec3::Z)
            .unwrap();
        let face_vertex_counts: Vec<_> = faces
            .iter()
            .map(|id| match &doc.entity(*id).unwrap().kind {
                SketchEntityKind::Face { vertices, .. } => vertices.len(),
                _ => 0,
            })
            .collect();

        assert_eq!(faces.len(), 2);
        assert_eq!(face_vertex_counts, vec![3, 3]);
    }

    #[test]
    fn cad_command_protocol_executes_road_and_room_as_semantic_batches() {
        let mut doc = SketchDocument::new();

        let road = SketchCadCommand::new(SketchCadTool::Road)
            .with_material("GlowStone")
            .with_width(3.0)
            .with_label("Bot road")
            .with_points([
                Vec3::new(-15.2, 4.0, 22.1),
                Vec3::new(-5.0, 4.0, 10.5),
                Vec3::new(12.4, 4.5, -2.3),
            ]);
        let road_result = doc
            .execute_cad_command(doc.active_context(), &road)
            .expect("road command should execute");

        assert_eq!(road_result.label, "Bot road");
        assert_eq!(road_result.entities.len(), 1);
        let road_id = road_result.entities[0];
        assert!(matches!(
            &doc.entity(road_id).unwrap().kind,
            SketchEntityKind::FreehandCurve { points } if points.len() == 3
        ));
        assert_eq!(
            doc.entity_attribute(road_id, "cad", "tool").unwrap(),
            Some("ROAD")
        );
        assert_eq!(
            doc.entity_attribute(road_id, "cad", "width").unwrap(),
            Some("3")
        );

        let room = SketchCadCommand::new(SketchCadTool::Room)
            .with_material("Limestone")
            .with_height(4.0)
            .with_width(0.35)
            .with_label("Bot room")
            .with_points([
                Vec3::new(-7.0, 4.0, 12.0),
                Vec3::new(-3.0, 4.0, 12.0),
                Vec3::new(-3.0, 4.0, 8.0),
                Vec3::new(-7.0, 4.0, 8.0),
            ]);
        let room_result = doc
            .execute_cad_command(doc.active_context(), &room)
            .expect("room command should execute");

        assert_eq!(room_result.label, "Bot room");
        assert_eq!(room_result.entities.len(), 2);
        let room_id = *room_result
            .entities
            .iter()
            .find(|id| {
                matches!(
                    doc.entity(**id).unwrap().kind,
                    SketchEntityKind::Room { .. }
                )
            })
            .expect("room command should return room entity");
        let SketchEntityKind::Room {
            shell,
            wall_thickness,
            shell_bounds,
            interior_bounds,
        } = &doc.entity(room_id).unwrap().kind
        else {
            panic!("room entity should be semantic");
        };
        assert!(room_result.entities.contains(shell));
        assert!((*wall_thickness - 0.35).abs() < f32::EPSILON);
        assert!(interior_bounds.size().x < shell_bounds.size().x);
        assert_eq!(
            doc.entity_attribute(room_id, "cad", "tool").unwrap(),
            Some("ROOM")
        );

        assert_eq!(doc.undo_count(), 2);
        assert_eq!(doc.undo_last().unwrap().label, "Bot room");
        assert!(doc.entity(room_id).is_none());
        assert!(doc.entity(road_id).is_some());
        assert_eq!(doc.undo_last().unwrap().label, "Bot road");
        assert!(doc.entity(road_id).is_none());
    }

    #[test]
    fn cad_command_protocol_groups_pencil_segments_into_one_undo_batch() {
        let mut doc = SketchDocument::new();
        let pencil = SketchCadCommand::new(SketchCadTool::Pencil)
            .with_label("Bot pencil path")
            .with_points([Vec3::ZERO, Vec3::X * 4.0, Vec3::new(4.0, 3.0, 0.0)]);

        let result = doc
            .execute_cad_command(doc.active_context(), &pencil)
            .expect("pencil command should execute");

        assert_eq!(result.entities.len(), 2);
        assert!(result
            .entities
            .iter()
            .all(|id| matches!(doc.entity(*id).unwrap().kind, SketchEntityKind::Edge { .. })));
        assert_eq!(doc.undo_count(), 1);
        assert_eq!(doc.undo_last().unwrap().entity_count, 2);
        assert!(result.entities.iter().all(|id| doc.entity(*id).is_none()));
    }

    #[test]
    fn cad_command_protocol_targets_pushpull_and_opening_semantics() {
        let mut doc = SketchDocument::new();
        let face = doc
            .draw_rectangle_face(
                doc.active_context(),
                Vec3::ZERO,
                Vec3::X * 6.0,
                Vec3::Z * 4.0,
                "Wall",
            )
            .unwrap();

        let push = SketchCadCommand::new(SketchCadTool::PushPull)
            .with_target(face)
            .with_depth(2.5)
            .with_label("Bot push");
        let push_result = doc
            .execute_cad_command(doc.active_context(), &push)
            .expect("push command should execute");
        let extrusion = push_result.entities[0];
        assert!(matches!(
            &doc.entity(extrusion).unwrap().kind,
            SketchEntityKind::PushPullExtrusion {
                source_face,
                depth,
                ..
            } if *source_face == face && (*depth - 2.5).abs() < f32::EPSILON
        ));

        let opening = SketchCadCommand::new(SketchCadTool::Opening)
            .with_target(face)
            .with_width(2.0)
            .with_height(3.0)
            .with_depth(0.8)
            .with_points([Vec3::new(3.0, 0.0, 1.5)])
            .with_label("Bot opening");
        let opening_result = doc
            .execute_cad_command(doc.active_context(), &opening)
            .expect("opening command should execute");
        assert!(matches!(
            &doc.entity(opening_result.entities[0]).unwrap().kind,
            SketchEntityKind::Opening {
                host,
                through_depth,
                ..
            } if *host == face && (*through_depth - 0.8).abs() < f32::EPSILON
        ));

        let bad = SketchCadCommand::new(SketchCadTool::PushPull).with_depth(1.0);
        assert!(matches!(
            doc.execute_cad_command(doc.active_context(), &bad),
            Err(SketchModelError::InvalidCadCommand(_))
        ));
    }
}
