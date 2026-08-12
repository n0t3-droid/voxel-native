//! Seam-free macro-scale terrain occlusion inspired by the Virtual Horizon Method.
//!
//! Local voxel AO resolves creases over one-cell neighbourhoods. It cannot
//! communicate that a valley sees less sky than a plateau. This module adds a
//! complementary, low-frequency term: four horizon profiles are sampled at a
//! chunk's shared X/Z corners, then bilinearly interpolated per mesh vertex.
//! Adjacent chunks therefore evaluate the exact same profile at their shared
//! edge and cannot develop lighting seams.
//!
//! The algorithm follows Gu & Dogan's 2.5D Virtual Horizon Method (Building
//! Simulation 2025, DOI 10.26868/25222708.2025.1302): for each azimuth we keep
//! only the maximum elevation angle of the surrounding height field. Our
//! logarithmic distance schedule is a bounded real-time adaptation for a
//! procedural voxel height function, not an irradiance simulator.

use ahash::{AHashMap, AHashSet};
use std::sync::{Arc, OnceLock, RwLock};

use crate::chunk::{ChunkPos, CHUNK_SIZE_I};

/// Eight azimuths provide a compact, rotation-symmetric horizon signature.
const HORIZON_DIRECTIONS: [(i32, i32); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

/// Log-like world-space distances in voxel units. Near relief gets denser
/// sampling while the last probes retain mountain-scale context.
const HORIZON_RADII_VOXELS: [i32; 8] = [4, 8, 16, 28, 44, 68, 104, 152];

/// One local height, then 8 azimuths x 8 distances for one corner sensor.
pub const HEIGHT_SAMPLES_PER_HORIZON_SENSOR: usize =
    1 + HORIZON_DIRECTIONS.len() * HORIZON_RADII_VOXELS.len();

/// Four cold corner sensors for an isolated field. Neighbouring cached fields
/// share two sensors, so their incremental cost is half this value.
pub const HEIGHT_SAMPLES_PER_HORIZON_FIELD: usize = 4 * HEIGHT_SAMPLES_PER_HORIZON_SENSOR;

/// The horizon is queried just above the procedural surface, in voxel units.
const SENSOR_CLEARANCE_VOXELS: i32 = 1;

/// Macro occlusion is intentionally restrained because vertex colour also
/// modulates direct PBR light. Local AO remains the high-contrast detail term.
const MIN_MACRO_LIGHT: f32 = 0.84;

/// Tall authored structures progressively escape the ground-level horizon.
const FULL_SKY_CLEARANCE_VOXELS: f32 = 48.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct HorizonSample {
    /// Maximum terrain elevation for each azimuth, in radians.
    max_elevation_rad: [f32; HORIZON_DIRECTIONS.len()],
    /// Isotropic visible-sky approximation in [0, 1], dimensionless.
    sky_visibility: f32,
    /// Procedural surface height at this sensor, in voxel-world units.
    surface_y_voxels: f32,
}

impl HorizonSample {
    fn build<F>(x: i32, z: i32, surface_height: &F) -> Self
    where
        F: Fn(i32, i32) -> i32,
    {
        let surface_y = surface_height(x, z);
        let sensor_y = surface_y.saturating_add(SENSOR_CLEARANCE_VOXELS);
        let mut max_elevation_rad = [0.0; HORIZON_DIRECTIONS.len()];

        for (direction_index, (direction_x, direction_z)) in
            HORIZON_DIRECTIONS.iter().copied().enumerate()
        {
            let mut maximum = 0.0_f32;
            for radius in HORIZON_RADII_VOXELS {
                let sample_x = x.saturating_add(direction_x.saturating_mul(radius));
                let sample_z = z.saturating_add(direction_z.saturating_mul(radius));
                let sample_y = surface_height(sample_x, sample_z);
                let delta_y = sample_y as i64 - sensor_y as i64;
                let offset_x = direction_x as f32 * radius as f32;
                let offset_z = direction_z as f32 * radius as f32;
                let horizontal_distance = offset_x.hypot(offset_z).max(f32::EPSILON);
                let elevation = (delta_y as f32)
                    .atan2(horizontal_distance)
                    .clamp(0.0, std::f32::consts::FRAC_PI_2);
                maximum = maximum.max(elevation);
            }
            max_elevation_rad[direction_index] = maximum;
        }

        // For an isotropic sky dome, 1 - sin(max elevation) is a compact
        // visible-sky estimate used by the source method. Angles remain in
        // radians internally; the result is dimensionless.
        let sky_visibility = max_elevation_rad
            .iter()
            .map(|angle| 1.0 - angle.sin())
            .sum::<f32>()
            / HORIZON_DIRECTIONS.len() as f32;

        Self {
            max_elevation_rad,
            sky_visibility: sky_visibility.clamp(0.0, 1.0),
            surface_y_voxels: surface_y as f32,
        }
    }
}

/// Four shared-corner profiles for one horizontal chunk column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VirtualHorizonField {
    origin_x: i32,
    origin_z: i32,
    // Order: south-west, south-east, north-west, north-east in X/Z space.
    corners: [HorizonSample; 4],
}

impl VirtualHorizonField {
    pub fn build<F>(pos: ChunkPos, surface_height: F) -> Self
    where
        F: Fn(i32, i32) -> i32,
    {
        let origin_x = pos.x.saturating_mul(CHUNK_SIZE_I);
        let origin_z = pos.z.saturating_mul(CHUNK_SIZE_I);
        let east_x = origin_x.saturating_add(CHUNK_SIZE_I);
        let north_z = origin_z.saturating_add(CHUNK_SIZE_I);
        let corners = [
            HorizonSample::build(origin_x, origin_z, &surface_height),
            HorizonSample::build(east_x, origin_z, &surface_height),
            HorizonSample::build(origin_x, north_z, &surface_height),
            HorizonSample::build(east_x, north_z, &surface_height),
        ];
        Self {
            origin_x,
            origin_z,
            corners,
        }
    }

    /// Linear-RGB multiplier for a world-space mesh vertex.
    ///
    /// The height-field horizon is sampled at ground level. A smooth vertical
    /// escape term prevents tall towers, ships, and sky structures from
    /// inheriting valley darkness after they rise above the surrounding relief.
    #[inline]
    pub fn macro_light_multiplier(&self, world_position: [i32; 3]) -> f32 {
        let (tx, tz) = self.local_weights(world_position[0], world_position[2]);
        let sky_visibility = self.bilerp(
            self.corners[0].sky_visibility,
            self.corners[1].sky_visibility,
            self.corners[2].sky_visibility,
            self.corners[3].sky_visibility,
            tx,
            tz,
        );
        let surface_y = self.bilerp(
            self.corners[0].surface_y_voxels,
            self.corners[1].surface_y_voxels,
            self.corners[2].surface_y_voxels,
            self.corners[3].surface_y_voxels,
            tx,
            tz,
        );
        let clearance = (world_position[1] as f32 - surface_y).max(0.0);
        let escape = smoothstep_f32(8.0, FULL_SKY_CLEARANCE_VOXELS, clearance);
        let effective_visibility = sky_visibility + (1.0 - sky_visibility) * escape;
        (MIN_MACRO_LIGHT + (1.0 - MIN_MACRO_LIGHT) * effective_visibility)
            .clamp(MIN_MACRO_LIGHT, 1.0)
    }

    #[inline]
    fn local_weights(&self, world_x: i32, world_z: i32) -> (f32, f32) {
        let dx = world_x as i64 - self.origin_x as i64;
        let dz = world_z as i64 - self.origin_z as i64;
        (
            (dx as f32 / CHUNK_SIZE_I as f32).clamp(0.0, 1.0),
            (dz as f32 / CHUNK_SIZE_I as f32).clamp(0.0, 1.0),
        )
    }

    #[inline]
    fn bilerp(
        &self,
        south_west: f32,
        south_east: f32,
        north_west: f32,
        north_east: f32,
        tx: f32,
        tz: f32,
    ) -> f32 {
        let south = south_west + (south_east - south_west) * tx;
        let north = north_west + (north_east - north_west) * tx;
        south + (north - south) * tz
    }
}

#[derive(Default)]
struct HorizonCacheState {
    fields: AHashMap<(i32, i32), Arc<OnceLock<VirtualHorizonField>>>,
    sensors: AHashMap<(i32, i32), Arc<OnceLock<HorizonSample>>>,
}

/// Thread-safe, seed-lifetime cache. A profile is independent of chunk Y, so
/// every vertical chunk in one X/Z column reuses the same field. Adjacent
/// fields also share their corner sensors. `OnceLock` makes same-column mesh
/// jobs coalesce instead of repeating the expensive height samples.
#[derive(Clone, Default)]
pub struct SharedHorizonCache {
    inner: Arc<RwLock<HorizonCacheState>>,
}

impl SharedHorizonCache {
    pub fn get_or_build<F>(&self, pos: ChunkPos, surface_height: F) -> VirtualHorizonField
    where
        F: Fn(i32, i32) -> i32,
    {
        let key = (pos.x, pos.z);
        let field_slot = self.field_slot(key);
        *field_slot.get_or_init(|| {
            let origin_x = pos.x.saturating_mul(CHUNK_SIZE_I);
            let origin_z = pos.z.saturating_mul(CHUNK_SIZE_I);
            let east_x = origin_x.saturating_add(CHUNK_SIZE_I);
            let north_z = origin_z.saturating_add(CHUNK_SIZE_I);
            let corners = [
                self.sensor_at(origin_x, origin_z, &surface_height),
                self.sensor_at(east_x, origin_z, &surface_height),
                self.sensor_at(origin_x, north_z, &surface_height),
                self.sensor_at(east_x, north_z, &surface_height),
            ];
            VirtualHorizonField {
                origin_x,
                origin_z,
                corners,
            }
        })
    }

    fn field_slot(&self, key: (i32, i32)) -> Arc<OnceLock<VirtualHorizonField>> {
        let cached = {
            let cache = self
                .inner
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.fields.get(&key).cloned()
        };
        if let Some(slot) = cached {
            return slot;
        }
        self.inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fields
            .entry(key)
            .or_insert_with(|| Arc::new(OnceLock::new()))
            .clone()
    }

    fn sensor_at<F>(&self, x: i32, z: i32, surface_height: &F) -> HorizonSample
    where
        F: Fn(i32, i32) -> i32,
    {
        let key = (x, z);
        // Keep the read guard in its own scope. An `if let` temporary can
        // otherwise live through the `else` branch and self-deadlock when
        // that branch acquires the write lock for a cold sensor.
        let cached = {
            let cache = self
                .inner
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.sensors.get(&key).cloned()
        };
        let slot = if let Some(slot) = cached {
            slot
        } else {
            self.inner
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .sensors
                .entry(key)
                .or_insert_with(|| Arc::new(OnceLock::new()))
                .clone()
        };
        *slot.get_or_init(|| HorizonSample::build(x, z, surface_height))
    }

    pub fn clear(&self) {
        let mut cache = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.fields.clear();
        cache.sensors.clear();
    }

    pub fn retain_within(&self, center_x: i32, center_z: i32, radius_chunks: i32) {
        let radius_sq = i64::from(radius_chunks.max(0)).pow(2);
        let mut cache = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.fields.retain(|(x, z), _| {
            let dx = i64::from(*x) - i64::from(center_x);
            let dz = i64::from(*z) - i64::from(center_z);
            dx * dx + dz * dz <= radius_sq
        });

        let mut required_sensors = AHashSet::with_capacity(cache.fields.len() * 4);
        for &(chunk_x, chunk_z) in cache.fields.keys() {
            let origin_x = chunk_x.saturating_mul(CHUNK_SIZE_I);
            let origin_z = chunk_z.saturating_mul(CHUNK_SIZE_I);
            let east_x = origin_x.saturating_add(CHUNK_SIZE_I);
            let north_z = origin_z.saturating_add(CHUNK_SIZE_I);
            required_sensors.extend([
                (origin_x, origin_z),
                (east_x, origin_z),
                (origin_x, north_z),
                (east_x, north_z),
            ]);
        }
        cache
            .sensors
            .retain(|key, _| required_sensors.contains(key));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fields
            .len()
    }

    #[cfg(test)]
    fn sensor_len(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sensors
            .len()
    }
}

#[inline]
fn smoothstep_f32(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn flat_terrain_has_full_sky_and_a_fixed_sample_budget() {
        let calls = AtomicUsize::new(0);
        let field = VirtualHorizonField::build(ChunkPos::new(0, 3, 0), |_, _| {
            calls.fetch_add(1, Ordering::Relaxed);
            42
        });

        assert_eq!(
            calls.load(Ordering::Relaxed),
            HEIGHT_SAMPLES_PER_HORIZON_FIELD
        );
        assert!(field
            .corners
            .iter()
            .all(|corner| corner.sky_visibility == 1.0));
        assert_eq!(field.macro_light_multiplier([8, 43, 8]), 1.0);
    }

    #[test]
    fn surrounding_relief_reduces_macro_light_but_never_crushes_black() {
        let field = VirtualHorizonField::build(ChunkPos::new(0, 0, 0), |x, z| {
            if matches!((x, z), (0, 0) | (16, 0) | (0, 16) | (16, 16)) {
                0
            } else {
                64
            }
        });

        let ground = field.macro_light_multiplier([8, 1, 8]);
        let high_structure = field.macro_light_multiplier([8, 80, 8]);
        assert!((MIN_MACRO_LIGHT..0.96).contains(&ground), "ground={ground}");
        assert!(high_structure > ground);
        assert!((high_structure - 1.0).abs() < 1e-6);
    }

    #[test]
    fn shared_chunk_edges_replay_the_exact_same_horizon_profile() {
        let height = |x: i32, z: i32| {
            40 + (x.div_euclid(11)).rem_euclid(7) - (z.div_euclid(13)).rem_euclid(5)
        };
        let west = VirtualHorizonField::build(ChunkPos::new(-1, 0, 4), height);
        let east = VirtualHorizonField::build(ChunkPos::new(0, 7, 4), height);

        assert_eq!(west.corners[1], east.corners[0]);
        assert_eq!(west.corners[3], east.corners[2]);
        for y in [30, 48, 96] {
            assert_eq!(
                west.macro_light_multiplier([0, y, 70]),
                east.macro_light_multiplier([0, y, 70])
            );
        }
    }

    #[test]
    fn cache_reuses_vertical_columns_and_evicts_distant_profiles() {
        let cache = SharedHorizonCache::default();
        let calls = AtomicUsize::new(0);
        let height = |_: i32, _: i32| {
            calls.fetch_add(1, Ordering::Relaxed);
            12
        };

        let first = cache.get_or_build(ChunkPos::new(5, 0, -3), height);
        let second = cache.get_or_build(ChunkPos::new(5, 9, -3), height);
        assert_eq!(first, second);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.sensor_len(), 4);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            HEIGHT_SAMPLES_PER_HORIZON_FIELD
        );

        cache.retain_within(0, 0, 2);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.sensor_len(), 0);
    }

    #[test]
    fn adjacent_fields_reuse_two_exact_corner_sensors() {
        let cache = SharedHorizonCache::default();
        let calls = AtomicUsize::new(0);
        let height = |_: i32, _: i32| {
            calls.fetch_add(1, Ordering::Relaxed);
            8
        };

        cache.get_or_build(ChunkPos::new(0, 0, 0), height);
        cache.get_or_build(ChunkPos::new(1, 0, 0), height);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.sensor_len(), 6);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            6 * HEIGHT_SAMPLES_PER_HORIZON_SENSOR
        );
    }

    #[test]
    fn concurrent_vertical_jobs_coalesce_to_one_field_build() {
        let cache = SharedHorizonCache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut jobs = Vec::new();
        for y in 0..8 {
            let cache = cache.clone();
            let calls = Arc::clone(&calls);
            jobs.push(std::thread::spawn(move || {
                cache.get_or_build(ChunkPos::new(-7, y, 11), |_, _| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    21
                })
            }));
        }
        let fields: Vec<_> = jobs
            .into_iter()
            .map(|job| job.join().expect("horizon worker"))
            .collect();

        assert!(fields.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.sensor_len(), 4);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            HEIGHT_SAMPLES_PER_HORIZON_FIELD
        );
    }

    #[test]
    fn extreme_chunk_coordinates_fail_closed_without_panicking() {
        let field = VirtualHorizonField::build(ChunkPos::new(i32::MAX, 0, i32::MIN), |x, z| {
            x.saturating_div(1_000_000)
                .saturating_add(z.saturating_div(1_000_000))
        });
        let light = field.macro_light_multiplier([i32::MAX, i32::MAX, i32::MIN]);
        assert!(light.is_finite());
        assert!((MIN_MACRO_LIGHT..=1.0).contains(&light));
    }
}
