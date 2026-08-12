//! Perceptual voxel budgets for scalable world rendering.
//!
//! The generated voxel topology is intentionally not part of this module.
//! A world seed must produce identical solid cells on every machine. These
//! budgets only decide how much secondary visual work is allowed around the
//! camera: ambient occlusion, micro detail, animated instances, facade
//! modules, and HDR emission.

use crate::chunk::CHUNK_SIZE;
use crate::neurocore::{QualityState, RuntimeProfile};
use crate::settings::{GraphicsMode, SceneryQuality};

/// The three visual scales used by the renderer.
///
/// Macro chunks preserve silhouette and material identity. Structural chunks
/// add lighting and authored facade structure. Micro chunks may spend the
/// remaining frame budget on close-range decoration and motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VoxelDetailTier {
    Macro,
    Structural,
    Micro,
}

/// HDR limits are explicit rather than hidden inside block colors. This keeps
/// emissive materials readable without allowing a large ore field to dominate
/// the exposure of the whole scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmissionBudget {
    Low,
    Balanced,
    Cinematic,
}

impl EmissionBudget {
    pub const fn max_luminance(self) -> f32 {
        match self {
            Self::Low => 0.72,
            Self::Balanced => 1.05,
            Self::Cinematic => 1.45,
        }
    }

    pub const fn max_peak_channel(self) -> f32 {
        match self {
            Self::Low => 1.25,
            Self::Balanced => 2.0,
            Self::Cinematic => 3.2,
        }
    }
}

/// Effective visual work for the current frame-pressure state.
///
/// All counters are hard ceilings. Systems may do less work, but must never
/// exceed them. The limits are deliberately expressed in units that can be
/// measured in telemetry instead of vague labels such as "pretty" or "fast".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldQualityBudget {
    pub structural_radius_chunks: i32,
    pub micro_radius_chunks: i32,
    pub max_micro_faces_per_chunk: u32,
    pub max_animated_instances: u32,
    pub facade_modules_per_building: u16,
    pub emission: EmissionBudget,
}

impl WorldQualityBudget {
    pub fn resolve(
        graphics: GraphicsMode,
        scenery: SceneryQuality,
        profile: RuntimeProfile,
        quality: QualityState,
    ) -> Self {
        let requested = requested_rank(graphics, scenery, profile);
        let pressure_cap = match quality {
            QualityState::Critical => 0,
            QualityState::Throttled => 1,
            QualityState::Nominal => 2,
            QualityState::Expanding | QualityState::Benchmark => 3,
        };
        let rank = requested.min(pressure_cap);

        match rank {
            0 => Self {
                structural_radius_chunks: 3,
                micro_radius_chunks: 0,
                max_micro_faces_per_chunk: 0,
                max_animated_instances: 48,
                facade_modules_per_building: 16,
                emission: EmissionBudget::Low,
            },
            1 => Self {
                structural_radius_chunks: 5,
                micro_radius_chunks: 2,
                max_micro_faces_per_chunk: 8_192,
                max_animated_instances: 96,
                facade_modules_per_building: 28,
                emission: EmissionBudget::Low,
            },
            2 => Self {
                structural_radius_chunks: 9,
                micro_radius_chunks: 4,
                max_micro_faces_per_chunk: 22_000,
                max_animated_instances: 320,
                facade_modules_per_building: 64,
                emission: EmissionBudget::Balanced,
            },
            _ => Self {
                structural_radius_chunks: 15,
                micro_radius_chunks: 8,
                max_micro_faces_per_chunk: 48_000,
                max_animated_instances: 768,
                facade_modules_per_building: 128,
                emission: EmissionBudget::Cinematic,
            },
        }
    }

    /// Chebyshev distance matches the square chunk-streaming footprint and
    /// avoids a square root in every scheduling decision.
    pub fn detail_tier(self, dx: i32, dz: i32) -> VoxelDetailTier {
        let distance = dx.abs().max(dz.abs());
        if distance <= self.micro_radius_chunks {
            VoxelDetailTier::Micro
        } else if distance <= self.structural_radius_chunks {
            VoxelDetailTier::Structural
        } else {
            VoxelDetailTier::Macro
        }
    }
}

fn requested_rank(graphics: GraphicsMode, scenery: SceneryQuality, profile: RuntimeProfile) -> u8 {
    let graphics_rank = match graphics {
        GraphicsMode::Fast => 1,
        GraphicsMode::Balanced => 2,
        GraphicsMode::High => 3,
    };
    let scenery_rank = match scenery {
        SceneryQuality::Off => 0,
        SceneryQuality::Lean => 1,
        SceneryQuality::Balanced => 2,
        SceneryQuality::Lush => 3,
    };
    let profile_cap = match profile {
        RuntimeProfile::LowSpec => 1,
        RuntimeProfile::Auto | RuntimeProfile::Balanced => 2,
        RuntimeProfile::Cinematic | RuntimeProfile::Benchmark => 3,
    };

    graphics_rank.min(scenery_rank.max(1)).min(profile_cap)
}

/// The old terrain loop asked for center + four neighbours for every column.
pub const NAIVE_SURFACE_SAMPLES_PER_CHUNK: usize = CHUNK_SIZE * CHUNK_SIZE * 5;

/// A one-cell border supplies those same values exactly once.
pub const CACHED_SURFACE_SAMPLES_PER_CHUNK: usize = (CHUNK_SIZE + 2) * (CHUNK_SIZE + 2);

pub const fn surface_sample_savings_per_chunk() -> usize {
    NAIVE_SURFACE_SAMPLES_PER_CHUNK - CACHED_SURFACE_SAMPLES_PER_CHUNK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn low() -> WorldQualityBudget {
        WorldQualityBudget::resolve(
            GraphicsMode::Fast,
            SceneryQuality::Lean,
            RuntimeProfile::LowSpec,
            QualityState::Nominal,
        )
    }

    fn balanced() -> WorldQualityBudget {
        WorldQualityBudget::resolve(
            GraphicsMode::Balanced,
            SceneryQuality::Balanced,
            RuntimeProfile::Balanced,
            QualityState::Nominal,
        )
    }

    fn cinematic() -> WorldQualityBudget {
        WorldQualityBudget::resolve(
            GraphicsMode::High,
            SceneryQuality::Lush,
            RuntimeProfile::Cinematic,
            QualityState::Expanding,
        )
    }

    #[test]
    fn budgets_scale_monotonically() {
        let low = low();
        let balanced = balanced();
        let cinematic = cinematic();

        assert!(low.structural_radius_chunks <= balanced.structural_radius_chunks);
        assert!(balanced.structural_radius_chunks <= cinematic.structural_radius_chunks);
        assert!(low.micro_radius_chunks <= balanced.micro_radius_chunks);
        assert!(balanced.micro_radius_chunks <= cinematic.micro_radius_chunks);
        assert!(low.max_animated_instances <= balanced.max_animated_instances);
        assert!(balanced.max_animated_instances <= cinematic.max_animated_instances);
        assert!(low.facade_modules_per_building <= balanced.facade_modules_per_building);
        assert!(balanced.facade_modules_per_building <= cinematic.facade_modules_per_building);
    }

    #[test]
    fn distance_only_removes_detail() {
        let budget = cinematic();
        let mut previous = VoxelDetailTier::Micro;
        for distance in 0..=budget.structural_radius_chunks + 3 {
            let tier = budget.detail_tier(distance, 0);
            assert!(tier <= previous);
            previous = tier;
        }
    }

    #[test]
    fn runtime_pressure_has_a_hard_cap() {
        let critical = WorldQualityBudget::resolve(
            GraphicsMode::High,
            SceneryQuality::Lush,
            RuntimeProfile::Cinematic,
            QualityState::Critical,
        );
        assert_eq!(critical.micro_radius_chunks, 0);
        assert_eq!(critical.max_micro_faces_per_chunk, 0);
        assert_eq!(critical.emission, EmissionBudget::Low);
    }

    #[test]
    fn terrain_surface_cache_removes_at_least_seventy_four_percent() {
        let saved =
            surface_sample_savings_per_chunk() as f64 / NAIVE_SURFACE_SAMPLES_PER_CHUNK as f64;
        assert!(saved >= 0.74, "actual saved ratio: {saved:.4}");
        assert_eq!(NAIVE_SURFACE_SAMPLES_PER_CHUNK, 1_280);
        assert_eq!(CACHED_SURFACE_SAMPLES_PER_CHUNK, 324);
    }

    #[test]
    fn emission_caps_scale_monotonically() {
        assert!(EmissionBudget::Low.max_luminance() < EmissionBudget::Balanced.max_luminance());
        assert!(
            EmissionBudget::Balanced.max_luminance() < EmissionBudget::Cinematic.max_luminance()
        );
        assert!(
            EmissionBudget::Low.max_peak_channel() < EmissionBudget::Balanced.max_peak_channel()
        );
    }
}
