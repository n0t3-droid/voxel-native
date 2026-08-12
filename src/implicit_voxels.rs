//! Conservative implicit-shape voxel classification and bounded microvoxels.
//!
//! The prototype is deliberately renderer- and ECS-independent. It classifies
//! complete axis-aligned cells, never only their centres, so thin shells and
//! rotated quadrics cannot silently disappear between samples. Adaptive
//! subdivision visits only conservative surface cells and obeys hard depth and
//! node limits.

use std::fmt;

pub const MAX_CLIP_PLANES: usize = 6;
pub const MAX_EXACT_GRID_INTEGER: i64 = 1_i64 << 53;
pub const HARD_MAX_MICRO_DEPTH: u8 = 8;
pub const HARD_MAX_MICRO_NODES: usize = 4_096;
/// Maximum pending DFS nodes: one active branch plus seven siblings per level.
pub const HARD_MAX_MICRO_STACK_NODES: usize = 1 + 7 * HARD_MAX_MICRO_DEPTH as usize;
pub const HARD_MAX_MICRO_SPLITS: usize = (HARD_MAX_MICRO_NODES - 1) / 8;
pub const HARD_MAX_MICRO_REACHABLE_NODES: usize = 1 + 8 * HARD_MAX_MICRO_SPLITS;
pub const HARD_MAX_MICRO_LEAVES: usize = 1 + 7 * HARD_MAX_MICRO_SPLITS;
pub const ORIENTED_ELLIPSOID_VERTEX_SAMPLES_PER_CLASSIFICATION: usize = 8;
pub const ORIENTED_ELLIPSOID_INTERVAL_AXES_PER_CLASSIFICATION: usize = 3;
pub const HARD_MAX_MICRO_CLASSIFICATIONS: usize = HARD_MAX_MICRO_REACHABLE_NODES;
pub const HARD_MAX_MICRO_ORIENTED_VERTEX_SAMPLES: usize =
    HARD_MAX_MICRO_CLASSIFICATIONS * ORIENTED_ELLIPSOID_VERTEX_SAMPLES_PER_CLASSIFICATION;
pub const HARD_MAX_MICRO_ORIENTED_INTERVAL_AXES: usize =
    HARD_MAX_MICRO_CLASSIFICATIONS * ORIENTED_ELLIPSOID_INTERVAL_AXES_PER_CLASSIFICATION;
pub const HARD_MAX_MICRO_CLIP_AABB_TESTS: usize = HARD_MAX_MICRO_CLASSIFICATIONS * MAX_CLIP_PLANES;
pub const DEFAULT_MIN_SURFACE_DEPTH: u8 = 2;
pub const DEFAULT_CURVATURE_RATIO: f64 = 0.25;

const NUMERIC_GUARD_ULPS: f64 = 256.0;
const ROTATION_TOLERANCE: f64 = 1.0e-10;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Vec3d {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3d {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);

    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub fn splat(value: f64) -> Self {
        Self::new(value, value, value)
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    pub fn dot(self, other: Self) -> f64 {
        self.x
            .mul_add(other.x, self.y.mul_add(other.y, self.z * other.z))
    }

    pub fn length_squared(self) -> f64 {
        self.dot(self)
    }

    pub fn length(self) -> f64 {
        self.length_squared().sqrt()
    }

    pub fn abs(self) -> Self {
        Self::new(self.x.abs(), self.y.abs(), self.z.abs())
    }

    fn component(self, axis: usize) -> f64 {
        match axis {
            0 => self.x,
            1 => self.y,
            2 => self.z,
            _ => unreachable!("three-dimensional vector axis"),
        }
    }

    pub fn min_component(self) -> f64 {
        self.x.min(self.y).min(self.z)
    }

    pub fn max_component(self) -> f64 {
        self.x.max(self.y).max(self.z)
    }

    pub fn max_abs_component(self) -> f64 {
        self.x.abs().max(self.y.abs()).max(self.z.abs())
    }

    pub fn checked_add(self, other: Self) -> Result<Self, GeometryError> {
        finite_vec(Self::new(
            self.x + other.x,
            self.y + other.y,
            self.z + other.z,
        ))
    }

    fn checked_sub(self, other: Self) -> Result<Self, GeometryError> {
        finite_vec(Self::new(
            self.x - other.x,
            self.y - other.y,
            self.z - other.z,
        ))
    }

    fn checked_scale(self, scale: f64) -> Result<Self, GeometryError> {
        if !scale.is_finite() {
            return Err(GeometryError::NonFiniteInput);
        }
        finite_vec(Self::new(self.x * scale, self.y * scale, self.z * scale))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb3d {
    min: Vec3d,
    max: Vec3d,
}

impl Aabb3d {
    pub fn new(min: Vec3d, max: Vec3d) -> Result<Self, GeometryError> {
        if !min.is_finite() || !max.is_finite() {
            return Err(GeometryError::NonFiniteInput);
        }
        if min.x >= max.x || min.y >= max.y || min.z >= max.z {
            return Err(GeometryError::InvalidAabb);
        }
        let span = max.checked_sub(min)?;
        if !span.is_finite() {
            return Err(GeometryError::ArithmeticOverflow);
        }
        Ok(Self { min, max })
    }

    /// Constructs an exactly representable integer grid cell.
    ///
    /// Binary64 represents every integer through `2^53` exactly. Coordinates
    /// outside that interval fail instead of aliasing adjacent cells.
    pub fn from_grid(origin: [i64; 3], edge: i64) -> Result<Self, GeometryError> {
        if edge <= 0 {
            return Err(GeometryError::InvalidGridEdge(edge));
        }
        let mut maximum = [0_i64; 3];
        for axis in 0..3 {
            maximum[axis] = origin[axis]
                .checked_add(edge)
                .ok_or(GeometryError::ArithmeticOverflow)?;
            if origin[axis].unsigned_abs() > MAX_EXACT_GRID_INTEGER as u64
                || maximum[axis].unsigned_abs() > MAX_EXACT_GRID_INTEGER as u64
            {
                let offending = if origin[axis].unsigned_abs() > MAX_EXACT_GRID_INTEGER as u64 {
                    origin[axis]
                } else {
                    maximum[axis]
                };
                return Err(GeometryError::GridCoordinateOutOfRange(offending));
            }
        }
        Self::new(
            Vec3d::new(origin[0] as f64, origin[1] as f64, origin[2] as f64),
            Vec3d::new(maximum[0] as f64, maximum[1] as f64, maximum[2] as f64),
        )
    }

    pub const fn min(self) -> Vec3d {
        self.min
    }

    pub const fn max(self) -> Vec3d {
        self.max
    }

    pub fn center(self) -> Vec3d {
        // Multiplying each endpoint before addition avoids overflow in
        // `(min + max) / 2` for otherwise valid finite bounds.
        Vec3d::new(
            self.min.x * 0.5 + self.max.x * 0.5,
            self.min.y * 0.5 + self.max.y * 0.5,
            self.min.z * 0.5 + self.max.z * 0.5,
        )
    }

    pub fn half_extents(self) -> Vec3d {
        Vec3d::new(
            (self.max.x - self.min.x) * 0.5,
            (self.max.y - self.min.y) * 0.5,
            (self.max.z - self.min.z) * 0.5,
        )
    }

    fn diagonal_length(self) -> f64 {
        self.max
            .checked_sub(self.min)
            .map(Vec3d::length)
            .unwrap_or(f64::INFINITY)
    }

    pub fn volume(self) -> Result<f64, GeometryError> {
        let span = self.max.checked_sub(self.min)?;
        let volume = span.x * span.y * span.z;
        if volume.is_finite() {
            Ok(volume)
        } else {
            Err(GeometryError::ArithmeticOverflow)
        }
    }

    fn corner(self, octant: u8) -> Vec3d {
        debug_assert!(octant < 8);
        Vec3d::new(
            if octant & 1 == 0 {
                self.min.x
            } else {
                self.max.x
            },
            if octant & 4 == 0 {
                self.min.y
            } else {
                self.max.y
            },
            if octant & 2 == 0 {
                self.min.z
            } else {
                self.max.z
            },
        )
    }

    pub fn contains_point(self, point: Vec3d) -> bool {
        point.x >= self.min.x
            && point.x <= self.max.x
            && point.y >= self.min.y
            && point.y <= self.max.y
            && point.z >= self.min.z
            && point.z <= self.max.z
    }

    pub fn child(self, octant: u8) -> Result<Self, GeometryError> {
        if octant >= 8 {
            return Err(GeometryError::InvalidOctant(octant));
        }
        let midpoint = self.center();
        if (self.min.x < self.max.x && !(midpoint.x > self.min.x && midpoint.x < self.max.x))
            || (self.min.y < self.max.y && !(midpoint.y > self.min.y && midpoint.y < self.max.y))
            || (self.min.z < self.max.z && !(midpoint.z > self.min.z && midpoint.z < self.max.z))
        {
            return Err(GeometryError::PrecisionExhausted);
        }
        Self::new(
            Vec3d::new(
                if octant & 1 == 0 {
                    self.min.x
                } else {
                    midpoint.x
                },
                if octant & 4 == 0 {
                    self.min.y
                } else {
                    midpoint.y
                },
                if octant & 2 == 0 {
                    self.min.z
                } else {
                    midpoint.z
                },
            ),
            Vec3d::new(
                if octant & 1 == 0 {
                    midpoint.x
                } else {
                    self.max.x
                },
                if octant & 4 == 0 {
                    midpoint.y
                } else {
                    self.max.y
                },
                if octant & 2 == 0 {
                    midpoint.z
                } else {
                    self.max.z
                },
            ),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellClassification {
    Outside,
    Inside,
    Surface,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipPlane {
    normal: Vec3d,
    offset: f64,
}

impl ClipPlane {
    /// Keeps the half-space `normal · point <= offset`.
    pub fn new(normal: Vec3d, offset: f64) -> Result<Self, GeometryError> {
        if !normal.is_finite() || !offset.is_finite() {
            return Err(GeometryError::NonFiniteInput);
        }
        let length = normal.length();
        if !(length > 0.0) || !length.is_finite() {
            return Err(GeometryError::InvalidClipPlane);
        }
        let normal = normal.checked_scale(1.0 / length)?;
        let offset = offset / length;
        if !offset.is_finite() {
            return Err(GeometryError::ArithmeticOverflow);
        }
        Ok(Self { normal, offset })
    }

    pub const fn normal(self) -> Vec3d {
        self.normal
    }

    pub const fn offset(self) -> f64 {
        self.offset
    }

    fn classify_aabb(self, aabb: &Aabb3d) -> CellClassification {
        let center = aabb.center();
        let half = aabb.half_extents();
        let projected_center = self.normal.dot(center);
        let projected_radius = self.normal.abs().dot(half);
        let minimum = projected_center - projected_radius;
        let maximum = projected_center + projected_radius;
        let guard = scalar_guard(
            center
                .max_abs_component()
                .max(self.offset.abs())
                .max(projected_radius),
        );
        if minimum > self.offset + guard {
            CellClassification::Outside
        } else if maximum < self.offset - guard {
            CellClassification::Inside
        } else {
            CellClassification::Surface
        }
    }

    fn contains_point(self, point: Vec3d) -> bool {
        let projection = self.normal.dot(point);
        if !projection.is_finite() {
            return false;
        }
        projection <= self.offset + scalar_guard(projection.abs().max(self.offset.abs()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipSet {
    planes: [Option<ClipPlane>; MAX_CLIP_PLANES],
    len: u8,
}

impl ClipSet {
    pub const EMPTY: Self = Self {
        planes: [None; MAX_CLIP_PLANES],
        len: 0,
    };

    pub fn new(planes: &[ClipPlane]) -> Result<Self, GeometryError> {
        if planes.len() > MAX_CLIP_PLANES {
            return Err(GeometryError::TooManyClipPlanes(planes.len()));
        }
        let mut result = Self::EMPTY;
        for (index, plane) in planes.iter().copied().enumerate() {
            result.planes[index] = Some(plane);
        }
        result.len = planes.len() as u8;
        Ok(result)
    }

    pub const fn len(self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = ClipPlane> + '_ {
        self.planes[..self.len()].iter().flatten().copied()
    }

    fn apply(self, base: CellClassification, aabb: &Aabb3d) -> CellClassification {
        if base == CellClassification::Outside {
            return base;
        }
        let mut fully_inside = base == CellClassification::Inside;
        for plane in self.iter() {
            match plane.classify_aabb(aabb) {
                CellClassification::Outside => return CellClassification::Outside,
                CellClassification::Surface => fully_inside = false,
                CellClassification::Inside => {}
            }
        }
        if fully_inside {
            CellClassification::Inside
        } else {
            CellClassification::Surface
        }
    }

    fn contains_point(self, point: Vec3d) -> bool {
        self.iter().all(|plane| plane.contains_point(point))
    }
}

impl Default for ClipSet {
    fn default() -> Self {
        Self::EMPTY
    }
}

pub trait ConservativeImplicitVolume {
    fn classify_aabb(&self, aabb: &Aabb3d) -> CellClassification;
    fn contains_point(&self, point: Vec3d) -> bool;
    /// Smallest relevant curvature/thickness scale, in world units.
    fn detail_scale(&self) -> f64;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SphereVolume {
    center: Vec3d,
    outer_radius: f64,
    inner_radius: f64,
    clips: ClipSet,
}

impl SphereVolume {
    pub fn new(
        center: Vec3d,
        outer_radius: f64,
        inner_radius: f64,
        clips: ClipSet,
    ) -> Result<Self, GeometryError> {
        validate_center_radii(center, Vec3d::splat(outer_radius))?;
        if !inner_radius.is_finite() || inner_radius < 0.0 || inner_radius >= outer_radius {
            return Err(GeometryError::InvalidInnerRadius {
                inner: inner_radius,
                outer: outer_radius,
            });
        }
        if inner_radius > 0.0 {
            validate_representable_inner_ratio(inner_radius / outer_radius)?;
        }
        Ok(Self {
            center,
            outer_radius,
            inner_radius,
            clips,
        })
    }

    pub fn solid(center: Vec3d, radius: f64) -> Result<Self, GeometryError> {
        Self::new(center, radius, 0.0, ClipSet::EMPTY)
    }

    pub const fn center(self) -> Vec3d {
        self.center
    }

    pub const fn outer_radius(self) -> f64 {
        self.outer_radius
    }

    pub const fn inner_radius(self) -> f64 {
        self.inner_radius
    }

    fn normalized_range(self, aabb: &Aabb3d) -> (f64, f64, f64) {
        let mut minimum_squared = 0.0;
        let mut maximum_squared = 0.0;
        for axis in 0..3 {
            let center = self.center.component(axis);
            let minimum = aabb.min.component(axis);
            let maximum = aabb.max.component(axis);
            let nearest = distance_to_interval(center, minimum, maximum) / self.outer_radius;
            let farthest =
                (minimum - center).abs().max((maximum - center).abs()) / self.outer_radius;
            minimum_squared = nearest.mul_add(nearest, minimum_squared);
            maximum_squared = farthest.mul_add(farthest, maximum_squared);
        }
        let guard = quadratic_guard(aabb, self.center, self.outer_radius, maximum_squared);
        (minimum_squared, maximum_squared, guard)
    }
}

impl ConservativeImplicitVolume for SphereVolume {
    fn classify_aabb(&self, aabb: &Aabb3d) -> CellClassification {
        let (minimum, maximum, guard) = self.normalized_range(aabb);
        let inner_ratio = self.inner_radius / self.outer_radius;
        self.clips.apply(
            classify_quadratic_range(minimum, maximum, inner_ratio * inner_ratio, guard),
            aabb,
        )
    }

    fn contains_point(&self, point: Vec3d) -> bool {
        if !point.is_finite() || !self.clips.contains_point(point) {
            return false;
        }
        let Ok(delta) = point.checked_sub(self.center) else {
            return false;
        };
        let normalized = Vec3d::new(
            delta.x / self.outer_radius,
            delta.y / self.outer_radius,
            delta.z / self.outer_radius,
        );
        let q = normalized.length_squared();
        let inner_ratio = self.inner_radius / self.outer_radius;
        q <= 1.0 && (self.inner_radius == 0.0 || q >= inner_ratio * inner_ratio)
    }

    fn detail_scale(&self) -> f64 {
        if self.inner_radius > 0.0 {
            self.outer_radius
                .min(self.inner_radius)
                .min(self.outer_radius - self.inner_radius)
        } else {
            self.outer_radius
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisAlignedEllipsoid {
    center: Vec3d,
    radii: Vec3d,
    inner_ratio: f64,
    clips: ClipSet,
}

impl AxisAlignedEllipsoid {
    pub fn new(
        center: Vec3d,
        radii: Vec3d,
        inner_ratio: f64,
        clips: ClipSet,
    ) -> Result<Self, GeometryError> {
        validate_center_radii(center, radii)?;
        validate_inner_ratio(inner_ratio)?;
        Ok(Self {
            center,
            radii,
            inner_ratio,
            clips,
        })
    }

    pub const fn center(self) -> Vec3d {
        self.center
    }

    pub const fn radii(self) -> Vec3d {
        self.radii
    }

    pub const fn inner_ratio(self) -> f64 {
        self.inner_ratio
    }

    fn normalized_range(self, aabb: &Aabb3d) -> (f64, f64, f64) {
        let mut minimum_squared = 0.0;
        let mut maximum_squared = 0.0;
        for axis in 0..3 {
            let center = self.center.component(axis);
            let radius = self.radii.component(axis);
            let minimum = aabb.min.component(axis);
            let maximum = aabb.max.component(axis);
            let nearest = distance_to_interval(center, minimum, maximum) / radius;
            let farthest = (minimum - center).abs().max((maximum - center).abs()) / radius;
            minimum_squared = nearest.mul_add(nearest, minimum_squared);
            maximum_squared = farthest.mul_add(farthest, maximum_squared);
        }
        let guard = quadratic_guard(
            aabb,
            self.center,
            self.radii.min_component(),
            maximum_squared,
        );
        (minimum_squared, maximum_squared, guard)
    }
}

impl ConservativeImplicitVolume for AxisAlignedEllipsoid {
    fn classify_aabb(&self, aabb: &Aabb3d) -> CellClassification {
        let (minimum, maximum, guard) = self.normalized_range(aabb);
        self.clips.apply(
            classify_quadratic_range(minimum, maximum, self.inner_ratio * self.inner_ratio, guard),
            aabb,
        )
    }

    fn contains_point(&self, point: Vec3d) -> bool {
        if !point.is_finite() || !self.clips.contains_point(point) {
            return false;
        }
        let Ok(delta) = point.checked_sub(self.center) else {
            return false;
        };
        let q = normalized_squared(delta, self.radii);
        q <= 1.0 && (self.inner_ratio == 0.0 || q >= self.inner_ratio * self.inner_ratio)
    }

    fn detail_scale(&self) -> f64 {
        ellipsoid_detail_scale(self.radii, self.inner_ratio)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rotation3d {
    /// Local principal axes expressed in world space.
    axes: [Vec3d; 3],
}

impl Rotation3d {
    pub const IDENTITY: Self = Self {
        axes: [
            Vec3d::new(1.0, 0.0, 0.0),
            Vec3d::new(0.0, 1.0, 0.0),
            Vec3d::new(0.0, 0.0, 1.0),
        ],
    };

    pub fn from_axes(axes: [Vec3d; 3]) -> Result<Self, GeometryError> {
        if axes.iter().any(|axis| !axis.is_finite()) {
            return Err(GeometryError::NonFiniteInput);
        }
        for axis in &axes {
            if (axis.length_squared() - 1.0).abs() > ROTATION_TOLERANCE {
                return Err(GeometryError::InvalidRotation);
            }
        }
        if axes[0].dot(axes[1]).abs() > ROTATION_TOLERANCE
            || axes[0].dot(axes[2]).abs() > ROTATION_TOLERANCE
            || axes[1].dot(axes[2]).abs() > ROTATION_TOLERANCE
        {
            return Err(GeometryError::InvalidRotation);
        }
        let handedness = cross(axes[0], axes[1]).dot(axes[2]);
        if (handedness - 1.0).abs() > ROTATION_TOLERANCE * 4.0 {
            return Err(GeometryError::InvalidRotation);
        }
        Ok(Self { axes })
    }

    pub fn from_axis_angle(axis: Vec3d, radians: f64) -> Result<Self, GeometryError> {
        if !axis.is_finite() || !radians.is_finite() {
            return Err(GeometryError::NonFiniteInput);
        }
        let length = axis.length();
        if !(length > 0.0) || !length.is_finite() {
            return Err(GeometryError::InvalidRotation);
        }
        let axis = axis.checked_scale(1.0 / length)?;
        let (sine, cosine) = radians.sin_cos();
        let one_minus_cosine = 1.0 - cosine;
        let x = axis.x;
        let y = axis.y;
        let z = axis.z;
        // Columns of Rodrigues' right-handed rotation matrix.
        Self::from_axes([
            Vec3d::new(
                cosine + x * x * one_minus_cosine,
                y * x * one_minus_cosine + z * sine,
                z * x * one_minus_cosine - y * sine,
            ),
            Vec3d::new(
                x * y * one_minus_cosine - z * sine,
                cosine + y * y * one_minus_cosine,
                z * y * one_minus_cosine + x * sine,
            ),
            Vec3d::new(
                x * z * one_minus_cosine + y * sine,
                y * z * one_minus_cosine - x * sine,
                cosine + z * z * one_minus_cosine,
            ),
        ])
    }

    pub const fn axes(self) -> [Vec3d; 3] {
        self.axes
    }

    pub fn to_local(self, world_delta: Vec3d) -> Vec3d {
        Vec3d::new(
            self.axes[0].dot(world_delta),
            self.axes[1].dot(world_delta),
            self.axes[2].dot(world_delta),
        )
    }

    pub fn to_world(self, local: Vec3d) -> Vec3d {
        Vec3d::new(
            self.axes[0].x.mul_add(
                local.x,
                self.axes[1].x.mul_add(local.y, self.axes[2].x * local.z),
            ),
            self.axes[0].y.mul_add(
                local.x,
                self.axes[1].y.mul_add(local.y, self.axes[2].y * local.z),
            ),
            self.axes[0].z.mul_add(
                local.x,
                self.axes[1].z.mul_add(local.y, self.axes[2].z * local.z),
            ),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrientedEllipsoid {
    center: Vec3d,
    radii: Vec3d,
    rotation: Rotation3d,
    inner_ratio: f64,
    clips: ClipSet,
}

impl OrientedEllipsoid {
    pub fn new(
        center: Vec3d,
        radii: Vec3d,
        rotation: Rotation3d,
        inner_ratio: f64,
        clips: ClipSet,
    ) -> Result<Self, GeometryError> {
        validate_center_radii(center, radii)?;
        validate_inner_ratio(inner_ratio)?;
        Ok(Self {
            center,
            radii,
            rotation,
            inner_ratio,
            clips,
        })
    }

    pub const fn center(self) -> Vec3d {
        self.center
    }

    pub const fn radii(self) -> Vec3d {
        self.radii
    }

    pub const fn rotation(self) -> Rotation3d {
        self.rotation
    }

    fn normalized_quadratic(self, point: Vec3d) -> f64 {
        let Ok(delta) = point.checked_sub(self.center) else {
            return f64::INFINITY;
        };
        normalized_squared(self.rotation.to_local(delta), self.radii)
    }

    /// Lower bound on the quadratic over the AABB.
    ///
    /// Each principal coordinate is an interval obtained by projecting the
    /// box. Ignoring correlation between those intervals can only lower the
    /// sum of minima, so the result is conservative for rejecting cells.
    fn interval_lower_bound(self, aabb: &Aabb3d) -> f64 {
        let center = aabb.center();
        let half = aabb.half_extents();
        let Ok(delta) = center.checked_sub(self.center) else {
            return 0.0;
        };
        let world_scale = center
            .max_abs_component()
            .max(self.center.max_abs_component())
            .max(half.max_abs_component())
            .max(1.0);
        let projection_uncertainty = scalar_guard(world_scale);
        let mut lower = 0.0;
        for axis in 0..3 {
            let principal_axis = self.rotation.axes[axis];
            let projected_center = principal_axis.dot(delta);
            let projected_radius = principal_axis.abs().dot(half) + projection_uncertainty;
            let minimum_absolute = (projected_center.abs() - projected_radius).max(0.0);
            let normalized = minimum_absolute / self.radii.component(axis);
            lower = normalized.mul_add(normalized, lower);
        }
        (lower - scalar_guard(lower)).max(0.0)
    }

    /// Exact in real arithmetic: a convex quadratic reaches its maximum on a
    /// box at one of the box's eight vertices.
    fn vertex_maximum(self, aabb: &Aabb3d) -> f64 {
        let mut maximum: f64 = 0.0;
        for octant in 0..8 {
            maximum = maximum.max(self.normalized_quadratic(aabb.corner(octant)));
        }
        maximum
    }
}

impl ConservativeImplicitVolume for OrientedEllipsoid {
    fn classify_aabb(&self, aabb: &Aabb3d) -> CellClassification {
        let minimum_lower_bound = self.interval_lower_bound(aabb);
        let maximum = self.vertex_maximum(aabb);
        let guard = quadratic_guard(aabb, self.center, self.radii.min_component(), maximum);
        self.clips.apply(
            classify_quadratic_range(
                minimum_lower_bound,
                maximum,
                self.inner_ratio * self.inner_ratio,
                guard,
            ),
            aabb,
        )
    }

    fn contains_point(&self, point: Vec3d) -> bool {
        if !point.is_finite() || !self.clips.contains_point(point) {
            return false;
        }
        let q = self.normalized_quadratic(point);
        q <= 1.0 && (self.inner_ratio == 0.0 || q >= self.inner_ratio * self.inner_ratio)
    }

    fn detail_scale(&self) -> f64 {
        ellipsoid_detail_scale(self.radii, self.inner_ratio)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MortonPath {
    code: u32,
    depth: u8,
}

impl MortonPath {
    pub const ROOT: Self = Self { code: 0, depth: 0 };

    pub const fn code(self) -> u32 {
        self.code
    }

    pub const fn depth(self) -> u8 {
        self.depth
    }

    /// Left-aligns this prefix at the fixed hard depth. Comparing these keys
    /// yields spatial Morton order even when neighboring leaves have different
    /// depths; comparing the raw `(code, depth)` pair does not.
    pub const fn spatial_key(self) -> u32 {
        self.code << (3 * (HARD_MAX_MICRO_DEPTH - self.depth))
    }

    pub fn child(self, octant: u8) -> Result<Self, GeometryError> {
        if octant >= 8 {
            return Err(GeometryError::InvalidOctant(octant));
        }
        if self.depth >= HARD_MAX_MICRO_DEPTH {
            return Err(GeometryError::DepthLimitExceeded(self.depth));
        }
        Ok(Self {
            code: (self.code << 3) | u32::from(octant),
            depth: self.depth + 1,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MicroVoxelLeaf {
    pub path: MortonPath,
    pub bounds: Aabb3d,
    pub classification: CellClassification,
}

/// Maximum vector payload retained by a production result. Allocator metadata
/// and the `Vec` header itself are deliberately not folded into this number.
pub const HARD_MAX_MICRO_RESULT_PAYLOAD_BYTES: usize =
    HARD_MAX_MICRO_NODES * std::mem::size_of::<MicroVoxelLeaf>();
/// Maximum simultaneous vector payload during the deterministic DFS build.
pub const HARD_MAX_MICRO_BUILD_PAYLOAD_BYTES: usize = HARD_MAX_MICRO_RESULT_PAYLOAD_BYTES
    + HARD_MAX_MICRO_STACK_NODES * std::mem::size_of::<MicroVoxelLeaf>();

#[derive(Debug, Clone, PartialEq)]
pub struct MicroVoxelResult {
    pub leaves: Vec<MicroVoxelLeaf>,
    pub visited_nodes: usize,
    pub split_nodes: usize,
    pub maximum_depth: u8,
    pub budget_limited: bool,
    pub precision_limited: bool,
}

impl MicroVoxelResult {
    pub fn surface_leaves(&self) -> usize {
        self.leaves
            .iter()
            .filter(|leaf| leaf.classification == CellClassification::Surface)
            .count()
    }

    pub fn inside_leaves(&self) -> usize {
        self.leaves
            .iter()
            .filter(|leaf| leaf.classification == CellClassification::Inside)
            .count()
    }

    pub fn outside_leaves(&self) -> usize {
        self.leaves
            .iter()
            .filter(|leaf| leaf.classification == CellClassification::Outside)
            .count()
    }
}

#[derive(Debug, Clone, Copy)]
struct MicroVoxelBudget {
    max_depth: u8,
    max_nodes: usize,
    minimum_surface_depth: u8,
    curvature_ratio: f64,
}

impl MicroVoxelBudget {
    const PRODUCTION: Self = Self {
        max_depth: HARD_MAX_MICRO_DEPTH,
        max_nodes: HARD_MAX_MICRO_NODES,
        minimum_surface_depth: DEFAULT_MIN_SURFACE_DEPTH,
        curvature_ratio: DEFAULT_CURVATURE_RATIO,
    };

    #[cfg(test)]
    fn for_test(
        max_depth: u8,
        max_nodes: usize,
        minimum_surface_depth: u8,
        curvature_ratio: f64,
    ) -> Self {
        assert!(max_depth <= HARD_MAX_MICRO_DEPTH);
        assert!(max_nodes > 0 && max_nodes <= HARD_MAX_MICRO_NODES);
        assert!(minimum_surface_depth <= max_depth);
        assert!(curvature_ratio.is_finite() && curvature_ratio > 0.0);
        Self {
            max_depth,
            max_nodes,
            minimum_surface_depth,
            curvature_ratio,
        }
    }
}

/// Fixed-budget adaptive classifier. The public API intentionally exposes no
/// user-controlled node/depth knobs.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveMicroVoxelizer {
    budget: MicroVoxelBudget,
}

impl AdaptiveMicroVoxelizer {
    pub const fn new() -> Self {
        Self {
            budget: MicroVoxelBudget::PRODUCTION,
        }
    }

    #[cfg(test)]
    fn with_test_budget(budget: MicroVoxelBudget) -> Self {
        Self { budget }
    }

    pub const fn hard_node_limit(self) -> usize {
        self.budget.max_nodes
    }

    pub const fn hard_depth_limit(self) -> u8 {
        self.budget.max_depth
    }

    pub const fn hard_result_payload_bytes(self) -> usize {
        self.budget.max_nodes * std::mem::size_of::<MicroVoxelLeaf>()
    }

    pub fn build<S: ConservativeImplicitVolume>(self, shape: &S, root: Aabb3d) -> MicroVoxelResult {
        let root_classification = shape.classify_aabb(&root);
        let mut result = MicroVoxelResult {
            leaves: Vec::with_capacity(self.budget.max_nodes),
            visited_nodes: 1,
            split_nodes: 0,
            maximum_depth: 0,
            budget_limited: false,
            precision_limited: false,
        };
        let stack_capacity = 1 + 7 * self.budget.max_depth as usize;
        let mut stack = Vec::with_capacity(stack_capacity);
        stack.push(MicroVoxelLeaf {
            path: MortonPath::ROOT,
            bounds: root,
            classification: root_classification,
        });

        while let Some(node) = stack.pop() {
            result.maximum_depth = result.maximum_depth.max(node.path.depth);
            let should_split = node.classification == CellClassification::Surface
                && node.path.depth < self.budget.max_depth
                && (node.path.depth < self.budget.minimum_surface_depth
                    || node.bounds.diagonal_length()
                        > shape.detail_scale() * self.budget.curvature_ratio);
            if !should_split {
                result.leaves.push(node);
                continue;
            }
            if result.visited_nodes.saturating_add(8) > self.budget.max_nodes {
                result.budget_limited = true;
                result.leaves.push(node);
                continue;
            }

            let mut children: [Option<MicroVoxelLeaf>; 8] = std::array::from_fn(|_| None);
            let mut split_valid = true;
            for octant in 0..8_u8 {
                let bounds = match node.bounds.child(octant) {
                    Ok(bounds) => bounds,
                    Err(GeometryError::PrecisionExhausted) => {
                        split_valid = false;
                        result.precision_limited = true;
                        break;
                    }
                    Err(_) => {
                        split_valid = false;
                        result.precision_limited = true;
                        break;
                    }
                };
                let path = node
                    .path
                    .child(octant)
                    .expect("budget depth never exceeds hard Morton depth");
                children[usize::from(octant)] = Some(MicroVoxelLeaf {
                    path,
                    bounds,
                    classification: shape.classify_aabb(&bounds),
                });
            }
            if !split_valid {
                result.leaves.push(node);
                continue;
            }

            result.visited_nodes += 8;
            result.split_nodes += 1;
            // Reverse push produces ascending Morton octants when popped.
            for child in children.into_iter().rev().flatten() {
                stack.push(child);
            }
            debug_assert!(stack.len() <= stack_capacity);
        }

        debug_assert!(result.visited_nodes <= self.budget.max_nodes);
        debug_assert!(result.leaves.len() <= result.visited_nodes);
        result
    }
}

impl Default for AdaptiveMicroVoxelizer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GeometryError {
    NonFiniteInput,
    ArithmeticOverflow,
    InvalidAabb,
    InvalidGridEdge(i64),
    GridCoordinateOutOfRange(i64),
    PrecisionExhausted,
    InvalidRadius(Vec3d),
    InvalidInnerRadius { inner: f64, outer: f64 },
    InvalidInnerRatio(f64),
    UnrepresentableInnerSurface(f64),
    InvalidClipPlane,
    TooManyClipPlanes(usize),
    InvalidRotation,
    InvalidOctant(u8),
    DepthLimitExceeded(u8),
}

impl fmt::Display for GeometryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteInput => write!(formatter, "geometry input must be finite"),
            Self::ArithmeticOverflow => write!(formatter, "checked geometry arithmetic overflow"),
            Self::InvalidAabb => write!(
                formatter,
                "AABB minimum must be strictly below maximum on every axis"
            ),
            Self::InvalidGridEdge(edge) => write!(formatter, "grid edge {edge} must be positive"),
            Self::GridCoordinateOutOfRange(value) => write!(
                formatter,
                "grid coordinate {value} exceeds exact binary64 integer range"
            ),
            Self::PrecisionExhausted => {
                write!(
                    formatter,
                    "cell can no longer be split at binary64 precision"
                )
            }
            Self::InvalidRadius(radii) => {
                write!(formatter, "radii must be positive and finite: {radii:?}")
            }
            Self::InvalidInnerRadius { inner, outer } => write!(
                formatter,
                "inner radius {inner} must satisfy 0 <= inner < outer {outer}"
            ),
            Self::InvalidInnerRatio(ratio) => {
                write!(formatter, "inner ratio {ratio} must satisfy 0 <= ratio < 1")
            }
            Self::UnrepresentableInnerSurface(ratio) => write!(
                formatter,
                "positive inner ratio {ratio} cannot represent a squared shell boundary"
            ),
            Self::InvalidClipPlane => write!(formatter, "clip-plane normal must be non-zero"),
            Self::TooManyClipPlanes(count) => write!(
                formatter,
                "{count} clip planes exceeds fixed limit {MAX_CLIP_PLANES}"
            ),
            Self::InvalidRotation => {
                write!(formatter, "rotation basis is not right-handed orthonormal")
            }
            Self::InvalidOctant(octant) => write!(formatter, "octant {octant} is outside 0..8"),
            Self::DepthLimitExceeded(depth) => write!(
                formatter,
                "Morton depth {depth} reached hard limit {HARD_MAX_MICRO_DEPTH}"
            ),
        }
    }
}

impl std::error::Error for GeometryError {}

fn finite_vec(value: Vec3d) -> Result<Vec3d, GeometryError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(GeometryError::ArithmeticOverflow)
    }
}

fn validate_center_radii(center: Vec3d, radii: Vec3d) -> Result<(), GeometryError> {
    if !center.is_finite() {
        return Err(GeometryError::NonFiniteInput);
    }
    if !radii.is_finite() || radii.x <= 0.0 || radii.y <= 0.0 || radii.z <= 0.0 {
        return Err(GeometryError::InvalidRadius(radii));
    }
    // A radius that cannot move either endpoint at this coordinate is not
    // representable. Reject it instead of producing a silently empty shape.
    for axis in 0..3 {
        let coordinate = center.component(axis);
        let radius = radii.component(axis);
        if coordinate + radius == coordinate || coordinate - radius == coordinate {
            return Err(GeometryError::PrecisionExhausted);
        }
    }
    Ok(())
}

fn validate_inner_ratio(inner_ratio: f64) -> Result<(), GeometryError> {
    if !inner_ratio.is_finite() || inner_ratio < 0.0 || inner_ratio >= 1.0 {
        return Err(GeometryError::InvalidInnerRatio(inner_ratio));
    }
    if inner_ratio > 0.0 {
        validate_representable_inner_ratio(inner_ratio)?;
    }
    Ok(())
}

/// The classifiers compare a normalized squared quadratic against the inner
/// boundary. A positive ratio whose square rounds to zero would turn a shell
/// into a solid at its centre. Such a boundary is outside this binary64
/// representation contract and must fail closed at construction time.
fn validate_representable_inner_ratio(inner_ratio: f64) -> Result<(), GeometryError> {
    if !(inner_ratio > 0.0) || !inner_ratio.is_finite() || inner_ratio * inner_ratio == 0.0 {
        return Err(GeometryError::UnrepresentableInnerSurface(inner_ratio));
    }
    Ok(())
}

fn distance_to_interval(value: f64, minimum: f64, maximum: f64) -> f64 {
    if value < minimum {
        minimum - value
    } else if value > maximum {
        value - maximum
    } else {
        0.0
    }
}

fn normalized_squared(delta: Vec3d, radii: Vec3d) -> f64 {
    let x = delta.x / radii.x;
    let y = delta.y / radii.y;
    let z = delta.z / radii.z;
    x.mul_add(x, y.mul_add(y, z * z))
}

fn scalar_guard(scale: f64) -> f64 {
    NUMERIC_GUARD_ULPS * f64::EPSILON * scale.abs().max(1.0)
}

fn quadratic_guard(aabb: &Aabb3d, center: Vec3d, minimum_radius: f64, maximum_q: f64) -> f64 {
    let coordinate_scale = aabb
        .min
        .max_abs_component()
        .max(aabb.max.max_abs_component())
        .max(center.max_abs_component())
        .max(1.0);
    let coordinate_uncertainty = scalar_guard(coordinate_scale) / minimum_radius;
    let normalized_scale = maximum_q.abs().sqrt().max(1.0);
    scalar_guard(maximum_q.abs().max(1.0))
        + coordinate_uncertainty * (2.0 * normalized_scale + coordinate_uncertainty)
}

fn classify_quadratic_range(
    minimum_lower_bound: f64,
    maximum_upper_candidate: f64,
    inner_squared: f64,
    guard: f64,
) -> CellClassification {
    if !minimum_lower_bound.is_finite()
        || !maximum_upper_candidate.is_finite()
        || !guard.is_finite()
    {
        return CellClassification::Surface;
    }
    let outside_outer = minimum_lower_bound > 1.0 + guard;
    let outside_in_cavity = inner_squared > 0.0 && maximum_upper_candidate < inner_squared - guard;
    if outside_outer || outside_in_cavity {
        return CellClassification::Outside;
    }
    let inside_outer = maximum_upper_candidate < 1.0 - guard;
    let outside_inner = inner_squared == 0.0 || minimum_lower_bound > inner_squared + guard;
    if inside_outer && outside_inner {
        CellClassification::Inside
    } else {
        CellClassification::Surface
    }
}

fn ellipsoid_detail_scale(radii: Vec3d, inner_ratio: f64) -> f64 {
    let minimum = radii.min_component();
    let maximum = radii.max_component();
    // c^2/a is a conservative smallest curvature-radius scale for an
    // ellipsoid with semi-axes bounded by c <= axis <= a.
    let curvature_radius = minimum * minimum / maximum;
    if inner_ratio > 0.0 {
        curvature_radius
            .min(curvature_radius * inner_ratio)
            .min(minimum * (1.0 - inner_ratio))
    } else {
        curvature_radius
    }
}

fn cross(a: Vec3d, b: Vec3d) -> Vec3d {
    Vec3d::new(
        a.y * b.z - a.z * b.y,
        a.z * b.x - a.x * b.z,
        a.x * b.y - a.y * b.x,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_4, PI};
    use std::hint::black_box;
    use std::mem::size_of;
    use std::time::Instant;

    fn box3(min: [f64; 3], max: [f64; 3]) -> Aabb3d {
        Aabb3d::new(
            Vec3d::new(min[0], min[1], min[2]),
            Vec3d::new(max[0], max[1], max[2]),
        )
        .unwrap()
    }

    #[derive(Clone, Copy)]
    struct DeterministicRng(u64);

    impl DeterministicRng {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut value = self.0;
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            value ^ (value >> 31)
        }

        fn unit(&mut self) -> f64 {
            (self.next_u64() >> 11) as f64 * (1.0 / ((1_u64 << 53) as f64))
        }

        fn range(&mut self, minimum: f64, maximum: f64) -> f64 {
            minimum + (maximum - minimum) * self.unit()
        }

        fn unit_vector(&mut self) -> Vec3d {
            loop {
                let candidate = Vec3d::new(
                    self.range(-1.0, 1.0),
                    self.range(-1.0, 1.0),
                    self.range(-1.0, 1.0),
                );
                let squared = candidate.length_squared();
                if squared > 1.0e-12 && squared <= 1.0 {
                    return candidate.checked_scale(1.0 / squared.sqrt()).unwrap();
                }
            }
        }
    }

    #[test]
    fn sphere_and_axis_ellipsoid_classification_are_symmetric() {
        let sphere = SphereVolume::solid(Vec3d::ZERO, 3.0).unwrap();
        let ellipsoid =
            AxisAlignedEllipsoid::new(Vec3d::ZERO, Vec3d::new(4.0, 2.0, 1.0), 0.0, ClipSet::EMPTY)
                .unwrap();
        let positive = box3([1.7, 0.2, -0.1], [2.2, 0.7, 0.4]);
        let negative = box3([-2.2, -0.7, -0.4], [-1.7, -0.2, 0.1]);
        assert_eq!(
            sphere.classify_aabb(&positive),
            sphere.classify_aabb(&negative)
        );
        assert_eq!(
            ellipsoid.classify_aabb(&positive),
            ellipsoid.classify_aabb(&negative)
        );
    }

    fn sphere_volume_bounds(resolution: usize) -> (f64, f64) {
        let shape = SphereVolume::solid(Vec3d::ZERO, 1.0).unwrap();
        volume_bounds(&shape, Vec3d::splat(-1.0), Vec3d::splat(1.0), resolution)
    }

    fn ellipsoid_volume_bounds(resolution: usize) -> (f64, f64) {
        let shape =
            AxisAlignedEllipsoid::new(Vec3d::ZERO, Vec3d::new(1.0, 0.75, 0.5), 0.0, ClipSet::EMPTY)
                .unwrap();
        volume_bounds(
            &shape,
            Vec3d::new(-1.0, -0.75, -0.5),
            Vec3d::new(1.0, 0.75, 0.5),
            resolution,
        )
    }

    fn volume_bounds<S: ConservativeImplicitVolume>(
        shape: &S,
        minimum: Vec3d,
        maximum: Vec3d,
        resolution: usize,
    ) -> (f64, f64) {
        let step = Vec3d::new(
            (maximum.x - minimum.x) / resolution as f64,
            (maximum.y - minimum.y) / resolution as f64,
            (maximum.z - minimum.z) / resolution as f64,
        );
        let cell_volume = step.x * step.y * step.z;
        let mut inside = 0_usize;
        let mut surface = 0_usize;
        for y in 0..resolution {
            for z in 0..resolution {
                for x in 0..resolution {
                    let cell_min = Vec3d::new(
                        minimum.x + x as f64 * step.x,
                        minimum.y + y as f64 * step.y,
                        minimum.z + z as f64 * step.z,
                    );
                    let cell = Aabb3d::new(cell_min, cell_min.checked_add(step).unwrap()).unwrap();
                    match shape.classify_aabb(&cell) {
                        CellClassification::Inside => inside += 1,
                        CellClassification::Surface => surface += 1,
                        CellClassification::Outside => {}
                    }
                }
            }
        }
        (
            inside as f64 * cell_volume,
            (inside + surface) as f64 * cell_volume,
        )
    }

    #[test]
    fn analytic_volumes_are_bracketed_and_bounds_converge() {
        let sphere_exact = 4.0 * PI / 3.0;
        let sphere_coarse = sphere_volume_bounds(12);
        let sphere_fine = sphere_volume_bounds(30);
        assert!(sphere_coarse.0 <= sphere_exact && sphere_exact <= sphere_coarse.1);
        assert!(sphere_fine.0 <= sphere_exact && sphere_exact <= sphere_fine.1);
        assert!(sphere_fine.1 - sphere_fine.0 < sphere_coarse.1 - sphere_coarse.0);

        let ellipsoid_exact = 4.0 * PI * 1.0 * 0.75 * 0.5 / 3.0;
        let ellipsoid_coarse = ellipsoid_volume_bounds(12);
        let ellipsoid_fine = ellipsoid_volume_bounds(30);
        assert!(ellipsoid_coarse.0 <= ellipsoid_exact && ellipsoid_exact <= ellipsoid_coarse.1);
        assert!(ellipsoid_fine.0 <= ellipsoid_exact && ellipsoid_exact <= ellipsoid_fine.1);
        assert!(ellipsoid_fine.1 - ellipsoid_fine.0 < ellipsoid_coarse.1 - ellipsoid_coarse.0);
        eprintln!(
            "VOLUME_BOUNDS sphere_exact={sphere_exact:.6} coarse=[{:.6},{:.6}] fine=[{:.6},{:.6}] ellipsoid_exact={ellipsoid_exact:.6} coarse=[{:.6},{:.6}] fine=[{:.6},{:.6}]",
            sphere_coarse.0,
            sphere_coarse.1,
            sphere_fine.0,
            sphere_fine.1,
            ellipsoid_coarse.0,
            ellipsoid_coarse.1,
            ellipsoid_fine.0,
            ellipsoid_fine.1,
        );
    }

    #[test]
    fn thin_shell_is_not_lost_when_cell_center_is_in_cavity() {
        let shell = SphereVolume::new(Vec3d::ZERO, 1.0, 0.99, ClipSet::EMPTY).unwrap();
        let root = box3([-1.1, -1.1, -1.1], [1.1, 1.1, 1.1]);
        assert!(!shell.contains_point(root.center()));
        assert_eq!(shell.classify_aabb(&root), CellClassification::Surface);

        let voxelizer =
            AdaptiveMicroVoxelizer::with_test_budget(MicroVoxelBudget::for_test(6, 1_025, 3, 0.25));
        let result = voxelizer.build(&shell, root);
        assert!(result.surface_leaves() > 0);
        assert!(result.maximum_depth >= 3);
        assert!(result.visited_nodes <= 1_025);
    }

    #[test]
    fn halfspace_clips_produce_caps_and_cuts_conservatively() {
        let left_half = ClipPlane::new(Vec3d::new(1.0, 0.0, 0.0), 0.0).unwrap();
        let lower_half = ClipPlane::new(Vec3d::new(0.0, 1.0, 0.0), 0.0).unwrap();
        let sphere = SphereVolume::new(
            Vec3d::ZERO,
            2.0,
            0.0,
            ClipSet::new(&[left_half, lower_half]).unwrap(),
        )
        .unwrap();
        assert_eq!(
            sphere.classify_aabb(&box3([0.2, -0.5, -0.5], [0.4, -0.2, 0.5])),
            CellClassification::Outside
        );
        assert_eq!(
            sphere.classify_aabb(&box3([-0.5, -0.5, -0.5], [-0.2, -0.2, 0.5])),
            CellClassification::Inside
        );
        assert_eq!(
            sphere.classify_aabb(&box3([-0.1, -0.5, -0.5], [0.1, -0.2, 0.5])),
            CellClassification::Surface
        );
    }

    #[test]
    fn oriented_identity_matches_axis_aligned_and_rotation_keeps_surface_points() {
        let radii = Vec3d::new(3.0, 1.5, 0.75);
        let axis = AxisAlignedEllipsoid::new(Vec3d::ZERO, radii, 0.0, ClipSet::EMPTY).unwrap();
        let oriented = OrientedEllipsoid::new(
            Vec3d::ZERO,
            radii,
            Rotation3d::IDENTITY,
            0.0,
            ClipSet::EMPTY,
        )
        .unwrap();
        for x in -4..4 {
            for y in -3..3 {
                let cell = box3(
                    [x as f64 * 0.5, y as f64 * 0.5, -0.2],
                    [x as f64 * 0.5 + 0.4, y as f64 * 0.5 + 0.4, 0.2],
                );
                assert_eq!(axis.classify_aabb(&cell), oriented.classify_aabb(&cell));
            }
        }

        let rotation = Rotation3d::from_axis_angle(Vec3d::new(0.0, 0.0, 1.0), FRAC_PI_4).unwrap();
        let rotated =
            OrientedEllipsoid::new(Vec3d::ZERO, radii, rotation, 0.0, ClipSet::EMPTY).unwrap();
        let principal_tip = rotation.axes()[0].checked_scale(radii.x).unwrap();
        let tip_cell = Aabb3d::new(
            principal_tip.checked_sub(Vec3d::splat(0.01)).unwrap(),
            principal_tip.checked_add(Vec3d::splat(0.01)).unwrap(),
        )
        .unwrap();
        assert_ne!(
            rotated.classify_aabb(&tip_cell),
            CellClassification::Outside
        );
    }

    #[test]
    fn outside_classification_never_rejects_a_sampled_inside_point() {
        let rotation = Rotation3d::from_axis_angle(Vec3d::new(1.0, 2.0, 3.0), 0.71).unwrap();
        let shape = OrientedEllipsoid::new(
            Vec3d::new(-1.25, 0.75, -2.5),
            Vec3d::new(2.5, 1.2, 0.55),
            rotation,
            0.82,
            ClipSet::EMPTY,
        )
        .unwrap();
        for z in -8..8 {
            for y in -8..8 {
                for x in -8..8 {
                    let minimum = Vec3d::new(
                        -4.0 + x as f64 * 0.5,
                        -3.0 + y as f64 * 0.5,
                        -6.0 + z as f64 * 0.5,
                    );
                    let cell =
                        Aabb3d::new(minimum, minimum.checked_add(Vec3d::splat(0.5)).unwrap())
                            .unwrap();
                    let classification = shape.classify_aabb(&cell);
                    if classification == CellClassification::Outside {
                        for sz in 0..=3 {
                            for sy in 0..=3 {
                                for sx in 0..=3 {
                                    let fraction = Vec3d::new(
                                        sx as f64 / 3.0,
                                        sy as f64 / 3.0,
                                        sz as f64 / 3.0,
                                    );
                                    let span = cell.max.checked_sub(cell.min).unwrap();
                                    let point = Vec3d::new(
                                        cell.min.x + span.x * fraction.x,
                                        cell.min.y + span.y * fraction.y,
                                        cell.min.z + span.z * fraction.z,
                                    );
                                    assert!(!shape.contains_point(point));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn adversarial_known_inside_points_survive_rotation_shells_clips_and_scale() {
        const CASES: usize = 1_024;
        let mut rng = DeterministicRng(0x6f3d_2c91_48a7_b5e0);
        for case_index in 0..CASES {
            let large_coordinate = case_index % 4 >= 2;
            let center_base = match case_index % 4 {
                0 => 0.0,
                1 => -1.0e6,
                2 => 1.0e12,
                _ => -1.0e12,
            };
            let center = Vec3d::new(
                center_base + rng.range(-100.0, 100.0),
                -center_base * 0.5 + rng.range(-100.0, 100.0),
                center_base * 0.25 + rng.range(-100.0, 100.0),
            );
            let radius_floor = if large_coordinate { 8.0 } else { 1.0e-5 };
            let base_radius = radius_floor + rng.range(0.05, 100.0);
            let radii = Vec3d::new(
                base_radius * rng.range(0.2, 5.0),
                base_radius * rng.range(0.2, 5.0),
                base_radius * rng.range(0.2, 5.0),
            );
            let rotation = Rotation3d::from_axis_angle(
                rng.unit_vector(),
                rng.range(-std::f64::consts::PI, std::f64::consts::PI),
            )
            .unwrap();
            let inner_ratio = if case_index % 3 == 0 {
                0.0
            } else if case_index % 3 == 1 {
                rng.range(0.01, 0.6)
            } else {
                rng.range(0.9, 0.995)
            };
            let direction = rng.unit_vector();
            let normalized_radius = match case_index % 4 {
                0 => 0.999,
                1 if inner_ratio > 0.0 => inner_ratio + (1.0 - inner_ratio) * 0.001,
                _ => inner_ratio + (1.0 - inner_ratio) * rng.range(0.1, 0.9),
            };
            let local_point = Vec3d::new(
                radii.x * direction.x * normalized_radius,
                radii.y * direction.y * normalized_radius,
                radii.z * direction.z * normalized_radius,
            );
            let point = center.checked_add(rotation.to_world(local_point)).unwrap();

            let mut planes = Vec::new();
            for _ in 0..case_index % 4 {
                let normal = rng.unit_vector();
                let margin = radii.min_component() * rng.range(0.001, 0.1);
                planes.push(ClipPlane::new(normal, normal.dot(point) + margin).unwrap());
            }
            let forward_clips = ClipSet::new(&planes).unwrap();
            planes.reverse();
            let reverse_clips = ClipSet::new(&planes).unwrap();
            let forward =
                OrientedEllipsoid::new(center, radii, rotation, inner_ratio, forward_clips)
                    .unwrap();
            let reverse =
                OrientedEllipsoid::new(center, radii, rotation, inner_ratio, reverse_clips)
                    .unwrap();
            assert!(forward.contains_point(point), "case {case_index}");
            assert!(reverse.contains_point(point), "case {case_index}");

            let half_edge = radii.min_component() * rng.range(1.0e-5, 0.15);
            let cell = Aabb3d::new(
                point.checked_sub(Vec3d::splat(half_edge)).unwrap(),
                point.checked_add(Vec3d::splat(half_edge)).unwrap(),
            )
            .unwrap();
            let forward_classification = forward.classify_aabb(&cell);
            let reverse_classification = reverse.classify_aabb(&cell);
            assert_ne!(
                forward_classification,
                CellClassification::Outside,
                "known occupied point rejected in case {case_index}"
            );
            assert_eq!(
                forward_classification, reverse_classification,
                "clip order changed case {case_index}"
            );

            if case_index < 4 {
                let root_half = Vec3d::splat(radii.max_component() * 1.75);
                let root = Aabb3d::new(
                    center.checked_sub(root_half).unwrap(),
                    center.checked_add(root_half).unwrap(),
                )
                .unwrap();
                let voxelizer = AdaptiveMicroVoxelizer::with_test_budget(
                    MicroVoxelBudget::for_test(5, 257, 2, 0.25),
                );
                assert_eq!(
                    voxelizer.build(&forward, root),
                    voxelizer.build(&forward, root),
                    "adaptive result changed in case {case_index}"
                );
            }
        }
    }

    #[test]
    fn extreme_coordinates_radii_and_grid_overflow_fail_closed() {
        let center = Vec3d::new(1.0e12, -1.0e12, 5.0e11);
        let sphere = SphereVolume::solid(center, 1_000.0).unwrap();
        let near_surface = box3(
            [center.x + 999.5, center.y - 0.25, center.z - 0.25],
            [center.x + 1_000.5, center.y + 0.25, center.z + 0.25],
        );
        assert_ne!(
            sphere.classify_aabb(&near_surface),
            CellClassification::Outside
        );

        let tiny = SphereVolume::solid(Vec3d::ZERO, 1.0e-12).unwrap();
        let tiny_surface = box3([-1.1e-12, -1.0e-14, -1.0e-14], [-0.9e-12, 1.0e-14, 1.0e-14]);
        assert_ne!(
            tiny.classify_aabb(&tiny_surface),
            CellClassification::Outside
        );
        assert!(tiny.contains_point(Vec3d::new(0.5e-12, 0.0, 0.0)));

        // Normalized membership avoids squaring radii, so shells remain valid
        // well beyond sqrt(f64::MAX).
        let huge = SphereVolume::new(Vec3d::ZERO, 1.0e200, 5.0e199, ClipSet::EMPTY).unwrap();
        assert!(!huge.contains_point(Vec3d::ZERO));
        assert!(huge.contains_point(Vec3d::new(7.5e199, 0.0, 0.0)));
        assert!(!huge.contains_point(Vec3d::new(1.1e200, 0.0, 0.0)));

        let subnormal = SphereVolume::solid(Vec3d::ZERO, 1.0e-320).unwrap();
        assert!(subnormal.contains_point(Vec3d::new(0.5e-320, 0.0, 0.0)));

        assert!(matches!(
            Aabb3d::from_grid([i64::MAX, 0, 0], 1),
            Err(GeometryError::ArithmeticOverflow)
                | Err(GeometryError::GridCoordinateOutOfRange(_))
        ));
        assert_eq!(
            Aabb3d::from_grid([MAX_EXACT_GRID_INTEGER, 0, 0], 1),
            Err(GeometryError::GridCoordinateOutOfRange(
                MAX_EXACT_GRID_INTEGER + 1
            ))
        );
        assert!(matches!(
            SphereVolume::solid(Vec3d::new(1.0e20, 0.0, 0.0), 1.0),
            Err(GeometryError::PrecisionExhausted)
        ));
        assert!(matches!(
            SphereVolume::solid(Vec3d::ZERO, 0.0),
            Err(GeometryError::InvalidRadius(_))
        ));
        assert_eq!(
            ClipPlane::new(Vec3d::new(1.0e-150, 0.0, 0.0), 1.0e200),
            Err(GeometryError::ArithmeticOverflow)
        );
        let finite_plane = ClipPlane::new(Vec3d::new(1.0, 1.0, 1.0), 0.0).unwrap();
        assert!(!finite_plane.contains_point(Vec3d::splat(f64::MAX)));

        assert_eq!(
            Aabb3d::new(Vec3d::ZERO, Vec3d::ZERO),
            Err(GeometryError::InvalidAabb)
        );
        let huge_box = Aabb3d::new(Vec3d::splat(-1.0e200), Vec3d::splat(1.0e200)).unwrap();
        assert_eq!(huge_box.volume(), Err(GeometryError::ArithmeticOverflow));
    }

    #[test]
    fn positive_shell_cavities_never_underflow_into_solid_centres() {
        let collapsed_ratio = 1.0e-200 / 1.0e200;
        assert_eq!(collapsed_ratio, 0.0);
        assert!(matches!(
            SphereVolume::new(Vec3d::ZERO, 1.0e200, 1.0e-200, ClipSet::EMPTY),
            Err(GeometryError::UnrepresentableInnerSurface(0.0))
        ));

        let squared_underflow = 1.0e-200;
        assert!(squared_underflow > 0.0 && squared_underflow * squared_underflow == 0.0);
        assert!(matches!(
            AxisAlignedEllipsoid::new(
                Vec3d::ZERO,
                Vec3d::splat(1.0),
                squared_underflow,
                ClipSet::EMPTY
            ),
            Err(GeometryError::UnrepresentableInnerSurface(value)) if value == squared_underflow
        ));
        assert!(matches!(
            OrientedEllipsoid::new(
                Vec3d::ZERO,
                Vec3d::splat(1.0),
                Rotation3d::IDENTITY,
                f64::from_bits(1),
                ClipSet::EMPTY
            ),
            Err(GeometryError::UnrepresentableInnerSurface(value)) if value == f64::from_bits(1)
        ));

        let minimum_accepted_ratio = f64::MIN_POSITIVE.sqrt();
        let sphere =
            SphereVolume::new(Vec3d::ZERO, 1.0, minimum_accepted_ratio, ClipSet::EMPTY).unwrap();
        assert!(!sphere.contains_point(Vec3d::ZERO));
        assert_ne!(
            sphere.classify_aabb(&box3(
                [-minimum_accepted_ratio * 0.25; 3],
                [minimum_accepted_ratio * 0.25; 3]
            )),
            CellClassification::Inside,
            "an accepted positive shell must never classify its cavity as solid"
        );
    }

    #[test]
    fn negative_grid_cells_and_microvoxels_are_deterministic() {
        let root = Aabb3d::from_grid([-9, -7, -5], 8).unwrap();
        let shape = SphereVolume::solid(Vec3d::new(-5.0, -3.0, -1.0), 3.5).unwrap();
        let voxelizer =
            AdaptiveMicroVoxelizer::with_test_budget(MicroVoxelBudget::for_test(5, 513, 2, 0.3));
        let first = voxelizer.build(&shape, root);
        let second = voxelizer.build(&shape, root);
        assert_eq!(first, second);
        assert!(first
            .leaves
            .windows(2)
            .all(|pair| { pair[0].path.spatial_key() <= pair[1].path.spatial_key() }));
    }

    #[test]
    fn clip_order_does_not_change_classification_or_subdivision() {
        let a = ClipPlane::new(Vec3d::new(1.0, 0.0, 0.0), 0.35).unwrap();
        let b = ClipPlane::new(Vec3d::new(0.0, 1.0, 0.0), 0.2).unwrap();
        let first =
            SphereVolume::new(Vec3d::ZERO, 2.0, 0.5, ClipSet::new(&[a, b]).unwrap()).unwrap();
        let second =
            SphereVolume::new(Vec3d::ZERO, 2.0, 0.5, ClipSet::new(&[b, a]).unwrap()).unwrap();
        let root = box3([-2.2, -2.2, -2.2], [2.2, 2.2, 2.2]);
        let voxelizer =
            AdaptiveMicroVoxelizer::with_test_budget(MicroVoxelBudget::for_test(5, 1_025, 2, 0.25));
        assert_eq!(first.classify_aabb(&root), second.classify_aabb(&root));
        assert_eq!(
            voxelizer.build(&first, root),
            voxelizer.build(&second, root)
        );
    }

    #[test]
    fn adaptive_microvoxel_count_is_hard_bounded_and_partitions_whole_nodes() {
        const NODE_LIMIT: usize = 129;
        let voxelizer = AdaptiveMicroVoxelizer::with_test_budget(MicroVoxelBudget::for_test(
            8, NODE_LIMIT, 3, 0.05,
        ));
        let shape = SphereVolume::new(Vec3d::ZERO, 10.0, 9.95, ClipSet::EMPTY).unwrap();
        let result = voxelizer.build(&shape, box3([-11.0, -11.0, -11.0], [11.0, 11.0, 11.0]));
        assert!(result.budget_limited);
        assert!(result.visited_nodes <= NODE_LIMIT);
        assert!(result.leaves.len() <= result.visited_nodes);
        assert_eq!((result.visited_nodes - 1) % 8, 0);
        assert_eq!(result.visited_nodes, 1 + result.split_nodes * 8);
    }

    #[test]
    fn structure_sizes_and_production_limits_are_pinned() {
        assert_eq!(size_of::<CellClassification>(), 1);
        assert_eq!(size_of::<MortonPath>(), 8);
        assert_eq!(size_of::<MicroVoxelLeaf>(), 64);
        assert_eq!(AdaptiveMicroVoxelizer::new().hard_node_limit(), 4_096);
        assert_eq!(AdaptiveMicroVoxelizer::new().hard_depth_limit(), 8);
        assert_eq!(HARD_MAX_MICRO_STACK_NODES, 57);
        assert_eq!(HARD_MAX_MICRO_SPLITS, 511);
        assert_eq!(HARD_MAX_MICRO_REACHABLE_NODES, 4_089);
        assert_eq!(HARD_MAX_MICRO_LEAVES, 3_578);
        assert_eq!(HARD_MAX_MICRO_CLASSIFICATIONS, 4_089);
        assert_eq!(HARD_MAX_MICRO_ORIENTED_VERTEX_SAMPLES, 32_712);
        assert_eq!(HARD_MAX_MICRO_ORIENTED_INTERVAL_AXES, 12_267);
        assert_eq!(HARD_MAX_MICRO_CLIP_AABB_TESTS, 24_534);
        assert_eq!(HARD_MAX_MICRO_RESULT_PAYLOAD_BYTES, 262_144);
        assert_eq!(HARD_MAX_MICRO_BUILD_PAYLOAD_BYTES, 265_792);
        assert_eq!(
            AdaptiveMicroVoxelizer::new().hard_result_payload_bytes(),
            HARD_MAX_MICRO_RESULT_PAYLOAD_BYTES
        );
        let root = box3([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]);
        let containing = SphereVolume::solid(Vec3d::ZERO, 10.0).unwrap();
        let result = AdaptiveMicroVoxelizer::new().build(&containing, root);
        assert_eq!(result.leaves.capacity(), HARD_MAX_MICRO_NODES);
        assert_eq!(MAX_CLIP_PLANES, 6);
    }

    #[test]
    fn benchmark_conservative_classification_and_adaptive_build() {
        const CENTER_ITERATIONS: usize = 5_000_000;
        const CLASSIFY_ITERATIONS: usize = 2_000_000;
        const ORIENTED_ITERATIONS: usize = 500_000;
        const BUILD_ITERATIONS: usize = 250;

        let sphere = SphereVolume::new(Vec3d::ZERO, 8.0, 7.7, ClipSet::EMPTY).unwrap();
        let cells: Vec<_> = (0..64)
            .map(|index| {
                let x = -9.0 + index as f64 * (18.0 / 64.0);
                box3([x, -0.4, -0.4], [x + 0.35, 0.4, 0.4])
            })
            .collect();

        let center_started = Instant::now();
        let mut center_hits = 0_u64;
        for index in 0..CENTER_ITERATIONS {
            center_hits += u64::from(sphere.contains_point(black_box(cells[index & 63].center())));
        }
        let center_elapsed = center_started.elapsed();

        let conservative_started = Instant::now();
        let mut conservative_hits = 0_u64;
        for index in 0..CLASSIFY_ITERATIONS {
            conservative_hits += u64::from(
                sphere.classify_aabb(black_box(&cells[index & 63])) != CellClassification::Outside,
            );
        }
        let conservative_elapsed = conservative_started.elapsed();

        let rotation = Rotation3d::from_axis_angle(Vec3d::new(1.0, 2.0, 3.0), 0.77).unwrap();
        let ellipsoid = OrientedEllipsoid::new(
            Vec3d::ZERO,
            Vec3d::new(8.0, 4.0, 1.5),
            rotation,
            0.0,
            ClipSet::EMPTY,
        )
        .unwrap();
        let oriented_started = Instant::now();
        let mut oriented_hits = 0_u64;
        for index in 0..ORIENTED_ITERATIONS {
            oriented_hits += u64::from(
                ellipsoid.classify_aabb(black_box(&cells[index & 63]))
                    != CellClassification::Outside,
            );
        }
        let oriented_elapsed = oriented_started.elapsed();

        let voxelizer =
            AdaptiveMicroVoxelizer::with_test_budget(MicroVoxelBudget::for_test(6, 1_025, 2, 0.25));
        let root = box3([-9.0, -9.0, -9.0], [9.0, 9.0, 9.0]);
        let build_started = Instant::now();
        let mut visited = 0_usize;
        for _ in 0..BUILD_ITERATIONS {
            visited = visited.wrapping_add(black_box(voxelizer.build(&sphere, root)).visited_nodes);
        }
        let build_elapsed = build_started.elapsed();

        let center_ns = center_elapsed.as_nanos() as f64 / CENTER_ITERATIONS as f64;
        let conservative_ns = conservative_elapsed.as_nanos() as f64 / CLASSIFY_ITERATIONS as f64;
        let oriented_ns = oriented_elapsed.as_nanos() as f64 / ORIENTED_ITERATIONS as f64;
        let build_us = build_elapsed.as_nanos() as f64 / BUILD_ITERATIONS as f64 / 1_000.0;
        eprintln!(
            "IMPLICIT_BENCH center_ns={center_ns:.2} sphere_aabb_ns={conservative_ns:.2} oriented_aabb_ns={oriented_ns:.2} adaptive_build_us={build_us:.2} adaptive_avg_nodes={} leaf_bytes={} center_hits={center_hits} conservative_hits={conservative_hits} oriented_hits={oriented_hits}",
            visited / BUILD_ITERATIONS,
            size_of::<MicroVoxelLeaf>(),
        );
        assert!(center_hits > 0);
        assert!(
            conservative_hits
                >= center_hits * CLASSIFY_ITERATIONS as u64 / CENTER_ITERATIONS as u64
        );
        assert!(oriented_hits > 0);
        assert!(visited > 0);
    }
}
