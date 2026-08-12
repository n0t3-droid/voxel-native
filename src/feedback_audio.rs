//! Lightweight procedural feedback audio.
//!
//! Voxel Native currently ships without an asset pipeline for sound. These
//! short, deterministic sources keep the first feedback layer self-contained:
//! weapon fire, explosions, and committed construction batches become audible
//! without adding external files or runtime allocation-heavy synthesis.

use std::time::Duration;

use bevy::audio::{AddAudioSource, Decodable, Source, Volume};
use bevy::prelude::*;
use bevy::reflect::TypePath;

use crate::builder::BuilderHistory;
use crate::weapons::DestructionStats;

const SAMPLE_RATE: u32 = 44_100;

pub struct FeedbackAudioPlugin;

impl Plugin for FeedbackAudioPlugin {
    fn build(&self, app: &mut App) {
        app.add_audio_source::<FeedbackTone>()
            .init_resource::<FeedbackAudioState>()
            .add_systems(Startup, setup_feedback_audio)
            .add_systems(Update, play_feedback_audio);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FeedbackToneKind {
    Construction,
    Weapon,
    Explosion,
}

impl FeedbackToneKind {
    const fn duration(self) -> f32 {
        match self {
            Self::Construction => 0.075,
            Self::Weapon => 0.13,
            Self::Explosion => 0.42,
        }
    }
}

#[derive(Asset, TypePath)]
struct FeedbackTone {
    kind: FeedbackToneKind,
}

struct FeedbackToneDecoder {
    kind: FeedbackToneKind,
    sample: u32,
    sample_count: u32,
    noise_state: u32,
}

impl FeedbackToneDecoder {
    fn new(kind: FeedbackToneKind) -> Self {
        Self {
            kind,
            sample: 0,
            sample_count: (kind.duration() * SAMPLE_RATE as f32).round() as u32,
            noise_state: 0xA341_316C ^ (kind as u32).wrapping_mul(0x9E37_79B9),
        }
    }

    fn noise(&mut self) -> f32 {
        self.noise_state ^= self.noise_state << 13;
        self.noise_state ^= self.noise_state >> 17;
        self.noise_state ^= self.noise_state << 5;
        (self.noise_state as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

impl Iterator for FeedbackToneDecoder {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.sample >= self.sample_count {
            return None;
        }
        let t = self.sample as f32 / SAMPLE_RATE as f32;
        let phase = self.sample as f32 / self.sample_count.max(1) as f32;
        let attack = (t / 0.006).clamp(0.0, 1.0);
        let envelope = attack * (1.0 - phase).max(0.0).powf(1.65);
        let noise = self.noise();
        let tau = std::f32::consts::TAU;
        let sample = match self.kind {
            FeedbackToneKind::Construction => {
                let body = (tau * (720.0 - phase * 210.0) * t).sin();
                let tick = (tau * 1_850.0 * t).sin() * (1.0 - phase).powi(4);
                body * 0.42 + tick * 0.28 + noise * 0.08
            }
            FeedbackToneKind::Weapon => {
                let plasma = (tau * (240.0 + phase * 540.0) * t).sin();
                let crack = (tau * 1_240.0 * t).sin() * (1.0 - phase).powi(3);
                plasma * 0.56 + crack * 0.24 + noise * 0.16
            }
            FeedbackToneKind::Explosion => {
                let thump = (tau * (92.0 - phase * 44.0) * t).sin();
                let pressure = (tau * 46.0 * t).sin();
                thump * 0.58 + pressure * 0.22 + noise * (0.30 * (1.0 - phase))
            }
        };
        self.sample += 1;
        Some((sample * envelope).clamp(-1.0, 1.0))
    }
}

impl Source for FeedbackToneDecoder {
    fn current_frame_len(&self) -> Option<usize> {
        Some(self.sample_count.saturating_sub(self.sample) as usize)
    }

    fn channels(&self) -> u16 {
        1
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn total_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs_f32(self.kind.duration()))
    }
}

impl Decodable for FeedbackTone {
    type DecoderItem = f32;
    type Decoder = FeedbackToneDecoder;

    fn decoder(&self) -> Self::Decoder {
        FeedbackToneDecoder::new(self.kind)
    }
}

#[derive(Resource)]
struct FeedbackSounds {
    construction: Handle<FeedbackTone>,
    weapon: Handle<FeedbackTone>,
    explosion: Handle<FeedbackTone>,
}

#[derive(Resource, Default)]
struct FeedbackAudioState {
    initialized: bool,
    shots: u64,
    explosions: u64,
    undo_steps: usize,
    construction_cooldown: f32,
}

fn setup_feedback_audio(mut commands: Commands, mut tones: ResMut<Assets<FeedbackTone>>) {
    commands.insert_resource(FeedbackSounds {
        construction: tones.add(FeedbackTone {
            kind: FeedbackToneKind::Construction,
        }),
        weapon: tones.add(FeedbackTone {
            kind: FeedbackToneKind::Weapon,
        }),
        explosion: tones.add(FeedbackTone {
            kind: FeedbackToneKind::Explosion,
        }),
    });
}

fn play_feedback_audio(
    mut commands: Commands,
    time: Res<Time>,
    sounds: Option<Res<FeedbackSounds>>,
    stats: Option<Res<DestructionStats>>,
    history: Option<Res<BuilderHistory>>,
    mut state: ResMut<FeedbackAudioState>,
) {
    let Some(sounds) = sounds else {
        return;
    };
    let shots = stats.as_ref().map_or(0, |stats| stats.shots_fired);
    let explosions = stats.as_ref().map_or(0, |stats| stats.explosions);
    let undo_steps = history.as_ref().map_or(0, |history| history.undo_len());
    let history_changed = history.as_ref().is_some_and(|history| history.is_changed());
    state.construction_cooldown = (state.construction_cooldown - time.delta_seconds()).max(0.0);

    if !state.initialized {
        state.initialized = true;
        state.shots = shots;
        state.explosions = explosions;
        state.undo_steps = undo_steps;
        return;
    }

    if explosions > state.explosions {
        spawn_feedback(&mut commands, sounds.explosion.clone(), 0.24);
    } else if shots > state.shots {
        spawn_feedback(&mut commands, sounds.weapon.clone(), 0.16);
    } else if history_changed
        && undo_steps >= state.undo_steps
        && state.construction_cooldown <= 0.0
    {
        spawn_feedback(&mut commands, sounds.construction.clone(), 0.075);
        state.construction_cooldown = 0.09;
    }

    state.shots = shots;
    state.explosions = explosions;
    state.undo_steps = undo_steps;
}

fn spawn_feedback(commands: &mut Commands, source: Handle<FeedbackTone>, volume: f32) {
    commands.spawn(AudioSourceBundle {
        source,
        settings: PlaybackSettings::DESPAWN.with_volume(Volume::new(volume)),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn procedural_feedback_is_finite_bounded_and_terminates() {
        for kind in [
            FeedbackToneKind::Construction,
            FeedbackToneKind::Weapon,
            FeedbackToneKind::Explosion,
        ] {
            let expected = (kind.duration() * SAMPLE_RATE as f32).round() as usize;
            let samples = FeedbackToneDecoder::new(kind).collect::<Vec<_>>();
            assert_eq!(samples.len(), expected);
            assert!(samples
                .iter()
                .all(|sample| sample.is_finite() && (-1.0..=1.0).contains(sample)));
            assert!(samples.iter().any(|sample| sample.abs() > 0.01));
        }
    }
}
