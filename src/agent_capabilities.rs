//! Stable capability contract shared by every local engine-agent session.
//!
//! Identity, task and authority can differ per session.  Engine powers may not:
//! a future launcher publishes this same profile for every agent and Mission
//! Control can reject or flag a session that advertises another schema.

use serde::{Deserialize, Serialize};

pub(crate) const AGENT_CAPABILITY_SCHEMA_VERSION: u32 = 1;
pub(crate) const SHARED_POWER_PROFILE_ID: &str = "voxel-native/shared-agent-power/v1";
pub(crate) const DIRECT_BRIDGE_READY: bool = false;
pub(crate) const RON_FALLBACK_READY: bool = true;
pub(crate) const VISUAL_CAPTURE_READY: bool = true;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SharedAgentPowerProfile {
    pub schema_version: u32,
    pub profile_id: String,
    pub command_capabilities: Vec<String>,
    pub observation_capabilities: Vec<String>,
    pub authority_modes: Vec<String>,
    pub command_queue_slots: usize,
    pub event_queue_slots: usize,
    pub max_commands_per_frame: usize,
    pub direct_bridge_ready: bool,
    pub ron_fallback_ready: bool,
    pub visual_capture_ready: bool,
}

impl SharedAgentPowerProfile {
    pub(crate) fn current() -> Self {
        Self {
            schema_version: AGENT_CAPABILITY_SCHEMA_VERSION,
            profile_id: SHARED_POWER_PROFILE_ID.into(),
            command_capabilities: [
                "flight.axes",
                "flight.absolute_look",
                "input.key_edges",
                "input.mouse_edges",
                "game.enter_leave",
                "editor.mode_and_tool",
                "editor.human_equivalent_input",
                "bot.preview_approve_execute",
                "bot.pause_resume_cancel",
                "authority.handoff",
                "visual.capture",
                "runtime.exit",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            observation_capabilities: [
                "world.identity_time",
                "player.pose_flight",
                "environment.fields",
                "editor.tool_selection",
                "bot.command_progress",
                "streaming.near_current",
                "performance.frame_stalls",
                "errors.persistent",
                "visual.latest_capture",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            authority_modes: ["CODEX", "USER", "PAUSED"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            // These are the direct-bridge contract limits. The current RON
            // fallback is lower-rate and does not pretend these queues exist.
            command_queue_slots: 256,
            event_queue_slots: 512,
            max_commands_per_frame: 32,
            direct_bridge_ready: DIRECT_BRIDGE_READY,
            ron_fallback_ready: RON_FALLBACK_READY,
            visual_capture_ready: VISUAL_CAPTURE_READY,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentCapabilityManifest {
    pub manifest_schema_version: u32,
    pub agent_id: String,
    pub fleet_id: String,
    pub power: SharedAgentPowerProfile,
}

impl AgentCapabilityManifest {
    pub(crate) fn new(agent_id: impl Into<String>, fleet_id: impl Into<String>) -> Self {
        Self {
            manifest_schema_version: 1,
            agent_id: agent_id.into(),
            fleet_id: fleet_id.into(),
            power: SharedAgentPowerProfile::current(),
        }
    }
}

pub(crate) fn agent_capability_manifest_text(
    agent_id: &str,
    fleet_id: &str,
) -> Result<String, String> {
    let manifest = AgentCapabilityManifest::new(agent_id, fleet_id);
    ron::ser::to_string_pretty(&manifest, ron::ser::PrettyConfig::default())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_identity_receives_byte_equivalent_engine_powers() {
        let explorer = AgentCapabilityManifest::new("explorer", "fleet-a");
        let builder = AgentCapabilityManifest::new("builder", "fleet-a");
        let future = AgentCapabilityManifest::new("future-agent-99", "fleet-b");
        assert_eq!(explorer.power, builder.power);
        assert_eq!(builder.power, future.power);
        assert_eq!(explorer.power.profile_id, SHARED_POWER_PROFILE_ID);
    }

    #[test]
    fn parity_profile_is_bounded_and_honest_about_current_transport() {
        let power = SharedAgentPowerProfile::current();
        assert_eq!(power.schema_version, AGENT_CAPABILITY_SCHEMA_VERSION);
        assert_eq!(power.command_queue_slots, 256);
        assert_eq!(power.event_queue_slots, 512);
        assert_eq!(power.max_commands_per_frame, 32);
        assert!(!power.command_capabilities.is_empty());
        assert!(!power.observation_capabilities.is_empty());
        assert!(!power.direct_bridge_ready);
        assert!(power.ron_fallback_ready);
        assert!(power.visual_capture_ready);
    }

    #[test]
    fn capability_manifest_serialization_contains_exact_identity_and_power_profile() {
        let text = agent_capability_manifest_text("observer-7", "fleet-a")
            .expect("serialize capability contract");
        let decoded: AgentCapabilityManifest =
            ron::from_str(&text).expect("decode serialized capability contract");
        assert_eq!(decoded.agent_id, "observer-7");
        assert_eq!(decoded.fleet_id, "fleet-a");
        assert_eq!(decoded.power.profile_id, SHARED_POWER_PROFILE_ID);
        assert!(!decoded.power.direct_bridge_ready);
    }
}
