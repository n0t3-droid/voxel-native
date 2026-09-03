//! Friendly world director facade.
//!
//! Older HUD and shuttle systems ask this module for objective/cockpit
//! lines. The actual living-world behavior now lives in `bots`; this
//! resource relays bot-city state so the game reads as an open friendly
//! world rather than a staged simulation loop.

use bevy::prelude::*;

use crate::bots::FriendlyWorldBrain;
use crate::player::Player;

pub struct DirectorPlugin;

impl Plugin for DirectorPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SimulationDirector::default())
            .init_resource::<UnifiedTelemetry>()
            .add_systems(
                Update,
                update_director.run_if(in_state(crate::menu::GameState::InGame)),
            );
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct UnifiedTelemetry {
    pub ship_kills: u32,
    pub ship_shots: u32,
    pub ground_blocks_broken: u64,
    pub ground_shots: u64,
    pub build_actions: u32,
    pub build_blocks_changed: u64,
    pub city_actions: u32,
    pub invention_actions: u32,
    pub luminite_units: u64,
    pub magnetite_units: u64,
    pub iridium_units: u64,
}

impl UnifiedTelemetry {
    pub fn mission_score(&self) -> f32 {
        self.ship_kills as f32 * 8.0
            + self.ground_blocks_broken as f32 * 0.05
            + self.build_blocks_changed as f32 * 0.03
            + self.city_actions as f32 * 2.0
            + self.invention_actions as f32 * 3.0
            + self.luminite_units as f32 * 1.2
            + self.magnetite_units as f32 * 1.0
            + self.iridium_units as f32 * 2.5
    }
}

#[derive(Resource)]
pub struct SimulationDirector {
    message: String,
    nav_point: Vec3,
    tick: f32,
    enemy_pressure: f32,
}

impl Default for SimulationDirector {
    fn default() -> Self {
        Self {
            message: "COMPANIONS // awaiting your instructions".into(),
            nav_point: Vec3::ZERO,
            tick: 0.0,
            enemy_pressure: 0.35,
        }
    }
}

impl SimulationDirector {
    pub fn cockpit_line(&self) -> String {
        self.message.clone()
    }

    /// Navigation label + world point for shuttle HUD.
    pub fn navigation_dest(&self) -> (&'static str, Vec3) {
        ("COMPANIONS", self.nav_point)
    }

    pub fn enemy_pressure(&self) -> f32 {
        self.enemy_pressure
    }
}

fn update_director(
    time: Res<Time>,
    telemetry: Res<UnifiedTelemetry>,
    brain: Option<Res<FriendlyWorldBrain>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    mut director: ResMut<SimulationDirector>,
    player_q: Query<&Transform, With<Player>>,
) {
    director.tick += time.delta_seconds();
    if director.tick < 0.25 {
        return;
    }
    director.tick = 0.0;

    if let Some(brain) = brain.as_deref() {
        let (label, point) = brain.navigation_dest();
        director.nav_point = point;
        director.message = format!(
            "{} // score {:>4.0} // {}",
            label,
            telemetry.mission_score(),
            brain.cockpit_line()
        );
    } else if let Ok(player_tf) = player_q.get_single() {
        director.nav_point = player_tf.translation;
        director.message = format!(
            "COMPANIONS // score {:>4.0} // waiting for helper drones",
            telemetry.mission_score()
        );
    }

    let mode_pressure: f32 = mode
        .as_deref()
        .map(|m| {
            if m.is_build() {
                0.55
            } else if m.is_ship() {
                1.3
            } else {
                1.0
            }
        })
        .unwrap_or(1.0);
    director.enemy_pressure = (mode_pressure * 0.35).clamp(0.15_f32, 0.75_f32);
}
