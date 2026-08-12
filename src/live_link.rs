//! Local two-engine live link.
//!
//! A CODEX QA instance and a user-facing LIVE SPECTATOR instance run the
//! same deterministic control stream in separate working directories. Their
//! player poses are coupled over a small loopback-only UDP packet:
//!
//! - CODEX leads by default and the user instance follows automatically.
//! - F10 in the user instance enters JOIN mode. The user becomes authoritative,
//!   CODEX follows, and synthetic agent gameplay input is suppressed.
//!
//! The transport refuses non-loopback addresses. It is a local presentation
//! and test bridge, not a network multiplayer protocol.

use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};

use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::input::InputSystem;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiSet};

use crate::agent_control::AgentControlState;
use crate::menu::GameState;
use crate::player::{Player, PlayerMotionSet};

const LIVE_LINK_MAGIC: [u8; 4] = *b"VNLL";
const LIVE_LINK_VERSION: u8 = 2;
const LIVE_LINK_PACKET_BYTES: usize = 64;
const LIVE_LINK_SEND_INTERVAL: f32 = 1.0 / 30.0;
const LIVE_LINK_POSE_TIMEOUT: f32 = 1.0;
const MAX_PACKETS_PER_FRAME: usize = 64;
const MAX_ABS_POSITION: f32 = 10_000_000.0;
const MAX_ABS_VELOCITY: f32 = 10_000.0;
const MAX_ABS_YAW: f32 = std::f32::consts::TAU + 0.01;
const MAX_ABS_PITCH: f32 = std::f32::consts::FRAC_PI_2 + 0.01;
const KNOWN_PACKET_FLAGS: u16 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3);
const INPUT_W: u32 = 1 << 0;
const INPUT_A: u32 = 1 << 1;
const INPUT_S: u32 = 1 << 2;
const INPUT_D: u32 = 1 << 3;
const INPUT_SPACE: u32 = 1 << 4;
const INPUT_CONTROL: u32 = 1 << 5;
const INPUT_Q: u32 = 1 << 6;
const INPUT_E: u32 = 1 << 7;
const INPUT_F3: u32 = 1 << 8;
const INPUT_ESCAPE: u32 = 1 << 9;
const INPUT_MOUSE_LEFT: u32 = 1 << 10;
const INPUT_MOUSE_RIGHT: u32 = 1 << 11;
const INPUT_SHIFT: u32 = 1 << 12;
const KNOWN_INPUT_MASK: u32 = (1 << 13) - 1;
static NEXT_BOOT_NONCE: AtomicU64 = AtomicU64::new(1);

pub(crate) struct LiveLinkPlugin;

impl Plugin for LiveLinkPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(LiveLink::from_env())
            .add_systems(
                PreUpdate,
                (
                    toggle_user_join_mode,
                    poll_live_link_packets,
                    expire_stale_user_authority,
                    sync_agent_control_ownership,
                )
                    .chain()
                    .in_set(LiveLinkControlSet)
                    .after(InputSystem),
            )
            .add_systems(
                PreUpdate,
                gate_and_mirror_live_link_inputs
                    .in_set(LiveLinkInputSet)
                    .after(LiveLinkControlSet),
            )
            .add_systems(
                Update,
                apply_remote_live_link_pose
                    .after(PlayerMotionSet)
                    .run_if(in_state(GameState::InGame)),
            )
            .add_systems(PostUpdate, send_live_link_heartbeat)
            .add_systems(
                Update,
                draw_live_link_overlay
                    .after(EguiSet::InitContexts)
                    .run_if(live_link_overlay_is_visible),
            );
    }
}

/// Ordering boundary used by Agent Control so JOIN ownership is resolved
/// before any synthetic gameplay input is applied.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub(crate) struct LiveLinkControlSet;

/// Final input boundary. Agent Control releases or applies its synthetic
/// buttons before this set; a follower then gates them and reconstructs the
/// authoritative peer's held/edge state.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub(crate) struct LiveLinkInputSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveLinkSide {
    Codex,
    User,
}

impl LiveLinkSide {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "codex" | "qa" | "leader" => Some(Self::Codex),
            "user" | "spectator" | "watch" => Some(Self::User),
            _ => None,
        }
    }

    const fn packet_tag(self) -> u8 {
        match self {
            Self::Codex => 0,
            Self::User => 1,
        }
    }

    const fn from_packet_tag(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Codex),
            1 => Some(Self::User),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LiveLinkConfig {
    side: LiveLinkSide,
    bind: SocketAddr,
    peer: SocketAddr,
}

fn parse_live_link_config(
    side: Option<&str>,
    bind: Option<&str>,
    peer: Option<&str>,
) -> Result<Option<LiveLinkConfig>, String> {
    if side.is_none() && bind.is_none() && peer.is_none() {
        return Ok(None);
    }
    let side = side
        .and_then(LiveLinkSide::parse)
        .ok_or_else(|| "VOXEL_NATIVE_LIVE_LINK_SIDE must be CODEX or USER".to_owned())?;
    let bind = bind
        .ok_or_else(|| "VOXEL_NATIVE_LIVE_LINK_BIND is missing".to_owned())?
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid live-link bind address: {error}"))?;
    let peer = peer
        .ok_or_else(|| "VOXEL_NATIVE_LIVE_LINK_PEER is missing".to_owned())?
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid live-link peer address: {error}"))?;
    if !bind.ip().is_loopback() || !peer.ip().is_loopback() {
        return Err("live link is restricted to loopback addresses".to_owned());
    }
    if bind.port() == 0 || peer.port() == 0 {
        return Err("live-link ports must be explicit and non-zero".to_owned());
    }
    if bind.is_ipv4() != peer.is_ipv4() {
        return Err("live-link bind and peer must use the same address family".to_owned());
    }
    if bind == peer {
        return Err("live-link bind and peer addresses must be different".to_owned());
    }
    Ok(Some(LiveLinkConfig { side, bind, peer }))
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LiveLinkPose {
    boot_id: u64,
    sequence: u64,
    side: LiveLinkSide,
    user_leads: bool,
    pose_valid: bool,
    held_inputs: u32,
    position: Vec3,
    velocity: Vec3,
    yaw: f32,
    pitch: f32,
    flying: bool,
    on_ground: bool,
}

impl LiveLinkPose {
    fn encode(self) -> [u8; LIVE_LINK_PACKET_BYTES] {
        let mut packet = [0_u8; LIVE_LINK_PACKET_BYTES];
        packet[0..4].copy_from_slice(&LIVE_LINK_MAGIC);
        packet[4] = LIVE_LINK_VERSION;
        packet[5] = self.side.packet_tag();
        let mut flags = 0_u16;
        if self.user_leads {
            flags |= 1 << 0;
        }
        if self.flying {
            flags |= 1 << 1;
        }
        if self.on_ground {
            flags |= 1 << 2;
        }
        if self.pose_valid {
            flags |= 1 << 3;
        }
        packet[6..8].copy_from_slice(&flags.to_le_bytes());
        packet[8..16].copy_from_slice(&self.boot_id.to_le_bytes());
        packet[16..24].copy_from_slice(&self.sequence.to_le_bytes());
        write_f32(&mut packet, 24, self.position.x);
        write_f32(&mut packet, 28, self.position.y);
        write_f32(&mut packet, 32, self.position.z);
        write_f32(&mut packet, 36, self.velocity.x);
        write_f32(&mut packet, 40, self.velocity.y);
        write_f32(&mut packet, 44, self.velocity.z);
        write_f32(&mut packet, 48, self.yaw);
        write_f32(&mut packet, 52, self.pitch);
        packet[56..60].copy_from_slice(&self.held_inputs.to_le_bytes());
        packet
    }

    fn decode(packet: &[u8]) -> Option<Self> {
        if packet.len() != LIVE_LINK_PACKET_BYTES
            || packet[0..4] != LIVE_LINK_MAGIC
            || packet[4] != LIVE_LINK_VERSION
        {
            return None;
        }
        let side = LiveLinkSide::from_packet_tag(packet[5])?;
        let flags = u16::from_le_bytes(packet[6..8].try_into().ok()?);
        if flags & !KNOWN_PACKET_FLAGS != 0 {
            return None;
        }
        let pose = Self {
            boot_id: u64::from_le_bytes(packet[8..16].try_into().ok()?),
            sequence: u64::from_le_bytes(packet[16..24].try_into().ok()?),
            side,
            user_leads: flags & (1 << 0) != 0,
            pose_valid: flags & (1 << 3) != 0,
            held_inputs: u32::from_le_bytes(packet[56..60].try_into().ok()?),
            position: Vec3::new(
                read_f32(packet, 24)?,
                read_f32(packet, 28)?,
                read_f32(packet, 32)?,
            ),
            velocity: Vec3::new(
                read_f32(packet, 36)?,
                read_f32(packet, 40)?,
                read_f32(packet, 44)?,
            ),
            yaw: read_f32(packet, 48)?,
            pitch: read_f32(packet, 52)?,
            flying: flags & (1 << 1) != 0,
            on_ground: flags & (1 << 2) != 0,
        };
        let finite = pose.position.is_finite()
            && pose.velocity.is_finite()
            && pose.yaw.is_finite()
            && pose.pitch.is_finite();
        let within_limits = pose.position.abs().max_element() <= MAX_ABS_POSITION
            && pose.velocity.abs().max_element() <= MAX_ABS_VELOCITY
            && pose.yaw.abs() <= MAX_ABS_YAW
            && pose.pitch.abs() <= MAX_ABS_PITCH;
        let reserved_is_zero = packet[60..64].iter().all(|byte| *byte == 0);
        (finite
            && within_limits
            && pose.boot_id != 0
            && pose.sequence != 0
            && pose.held_inputs & !KNOWN_INPUT_MASK == 0
            && reserved_is_zero)
            .then_some(pose)
    }
}

fn write_f32(packet: &mut [u8; LIVE_LINK_PACKET_BYTES], offset: usize, value: f32) {
    packet[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn read_f32(packet: &[u8], offset: usize) -> Option<f32> {
    Some(f32::from_le_bytes(
        packet.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn fresh_boot_id() -> u64 {
    let epoch = crate::platform::now_nanos_seed();
    let nonce = NEXT_BOOT_NONCE.fetch_add(1, Ordering::Relaxed);
    #[cfg(not(target_arch = "wasm32"))]
    let process = u64::from(std::process::id());
    #[cfg(target_arch = "wasm32")]
    let process = 0_u64;

    // SplitMix64's finalizer turns the local time, process and per-process
    // nonce into an opaque non-zero stream identifier without adding a
    // platform RNG requirement to this tiny transport.
    let mut mixed =
        epoch as u64 ^ ((epoch >> 64) as u64).rotate_left(17) ^ process.rotate_left(32) ^ nonce;
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^= mixed >> 31;
    mixed.max(1)
}

#[derive(Resource, Debug)]
pub(crate) struct LiveLink {
    socket: Option<UdpSocket>,
    config: Option<LiveLinkConfig>,
    user_leads: bool,
    boot_id: u64,
    sequence: u64,
    remote_boot_id: Option<u64>,
    retired_remote_boot_id: Option<u64>,
    last_received_sequence: u64,
    remote_pose: Option<LiveLinkPose>,
    last_receive_seconds: f32,
    send_accumulator: f32,
    mirrored_input_mask: u32,
    last_error: Option<String>,
}

impl Default for LiveLink {
    fn default() -> Self {
        Self {
            socket: None,
            config: None,
            user_leads: false,
            boot_id: fresh_boot_id(),
            sequence: 0,
            remote_boot_id: None,
            retired_remote_boot_id: None,
            last_received_sequence: 0,
            remote_pose: None,
            last_receive_seconds: f32::NEG_INFINITY,
            send_accumulator: 0.0,
            mirrored_input_mask: 0,
            last_error: None,
        }
    }
}

impl LiveLink {
    fn from_env() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let side = std::env::var("VOXEL_NATIVE_LIVE_LINK_SIDE").ok();
            let bind = std::env::var("VOXEL_NATIVE_LIVE_LINK_BIND").ok();
            let peer = std::env::var("VOXEL_NATIVE_LIVE_LINK_PEER").ok();
            let config =
                match parse_live_link_config(side.as_deref(), bind.as_deref(), peer.as_deref()) {
                    Ok(Some(config)) => config,
                    Ok(None) => return Self::default(),
                    Err(error) => {
                        warn!("live link disabled: {error}");
                        return Self {
                            last_error: Some(error),
                            ..default()
                        };
                    }
                };
            match UdpSocket::bind(config.bind) {
                Ok(socket) => {
                    if let Err(error) = socket.set_nonblocking(true) {
                        let error = format!("could not make live-link socket nonblocking: {error}");
                        warn!("live link disabled: {error}");
                        return Self {
                            last_error: Some(error),
                            ..default()
                        };
                    }
                    info!(
                        "live link {:?}: {} -> {}",
                        config.side, config.bind, config.peer
                    );
                    let user_leads = config.side == LiveLinkSide::User
                        && std::env::var("VOXEL_NATIVE_LIVE_LINK_START_JOIN")
                            .map(|value| {
                                matches!(
                                    value.trim().to_ascii_lowercase().as_str(),
                                    "1" | "true" | "yes" | "on"
                                )
                            })
                            .unwrap_or(false);
                    Self {
                        socket: Some(socket),
                        config: Some(config),
                        user_leads,
                        ..default()
                    }
                }
                Err(error) => {
                    let error = format!("could not bind live-link socket {}: {error}", config.bind);
                    warn!("live link disabled: {error}");
                    Self {
                        last_error: Some(error),
                        ..default()
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            Self::default()
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.socket.is_some() && self.config.is_some()
    }

    pub(crate) fn suppresses_agent_gameplay(&self, now_seconds: f32) -> bool {
        self.is_active()
            && self.user_leads
            && match self.local_side() {
                Some(LiveLinkSide::Codex) => self.peer_is_live(now_seconds),
                Some(LiveLinkSide::User) => true,
                None => false,
            }
    }

    fn local_side(&self) -> Option<LiveLinkSide> {
        self.config.map(|config| config.side)
    }

    fn is_following(&self) -> bool {
        match self.local_side() {
            Some(LiveLinkSide::Codex) => self.user_leads,
            Some(LiveLinkSide::User) => !self.user_leads,
            None => false,
        }
    }

    fn peer_is_live(&self, now_seconds: f32) -> bool {
        self.remote_pose.is_some()
            && now_seconds >= self.last_receive_seconds
            && now_seconds - self.last_receive_seconds <= LIVE_LINK_POSE_TIMEOUT
    }

    fn accept_remote_pose(&mut self, pose: LiveLinkPose, now_seconds: f32) -> bool {
        let Some(config) = self.config else {
            return false;
        };
        if pose.side == config.side || !now_seconds.is_finite() {
            return false;
        }
        if self.remote_boot_id != Some(pose.boot_id) {
            if self.retired_remote_boot_id == Some(pose.boot_id) {
                return false;
            }
            self.retired_remote_boot_id = self.remote_boot_id;
            self.remote_boot_id = Some(pose.boot_id);
            self.last_received_sequence = 0;
            self.remote_pose = None;
        }
        if pose.sequence <= self.last_received_sequence {
            return false;
        }

        self.last_received_sequence = pose.sequence;
        self.last_receive_seconds = now_seconds;
        if pose.side == LiveLinkSide::User && config.side == LiveLinkSide::Codex {
            self.user_leads = pose.user_leads;
        }
        self.remote_pose = Some(pose);
        self.last_error = None;
        true
    }

    fn expire_stale_user_authority(&mut self, now_seconds: f32) -> bool {
        if self.local_side() == Some(LiveLinkSide::Codex)
            && self.user_leads
            && !self.peer_is_live(now_seconds)
        {
            self.user_leads = false;
            return true;
        }
        false
    }

    fn advance_local_sequence(&mut self) {
        if self.sequence == u64::MAX {
            self.boot_id = fresh_boot_id();
            self.sequence = 1;
        } else {
            self.sequence += 1;
        }
    }

    fn mode_label(&self, now_seconds: f32) -> &'static str {
        if !self.is_active() && self.last_error.is_some() {
            return "LINK ERROR";
        }
        if !self.peer_is_live(now_seconds) {
            return "LINKING";
        }
        match (self.local_side(), self.user_leads) {
            (Some(LiveLinkSide::Codex), false) => "QA LEADER",
            (Some(LiveLinkSide::User), false) => "LIVE SPECTATOR",
            (Some(LiveLinkSide::Codex), true) => "JOIN OBSERVER",
            (Some(LiveLinkSide::User), true) => "JOIN // YOU LEAD",
            (None, _) => "OFFLINE",
        }
    }
}

fn live_link_overlay_is_visible(link: Res<LiveLink>) -> bool {
    link.is_active() || link.last_error.is_some()
}

fn toggle_user_join_mode(keys: Res<ButtonInput<KeyCode>>, mut link: ResMut<LiveLink>) {
    if link.local_side() != Some(LiveLinkSide::User) || !keys.just_pressed(KeyCode::F10) {
        return;
    }
    link.user_leads = !link.user_leads;
    link.send_accumulator = LIVE_LINK_SEND_INTERVAL;
    info!(
        "live link: user {}",
        if link.user_leads {
            "entered JOIN mode"
        } else {
            "returned to spectator mode"
        }
    );
}

fn poll_live_link_packets(time: Res<Time>, mut link: ResMut<LiveLink>) {
    let socket = match link.socket.as_ref().map(UdpSocket::try_clone) {
        Some(Ok(socket)) => socket,
        Some(Err(error)) => {
            link.last_error = Some(format!("could not clone live-link socket: {error}"));
            return;
        }
        None => return,
    };
    let Some(config) = link.config else {
        return;
    };
    // One extra byte is intentional: a receive buffer exactly as large as the
    // protocol could make an oversized UDP datagram look valid after
    // truncation on platforms that do not return MessageTooLong.
    let mut buffer = [0_u8; LIVE_LINK_PACKET_BYTES + 1];
    for _ in 0..MAX_PACKETS_PER_FRAME {
        match socket.recv_from(&mut buffer) {
            Ok((received, source)) => {
                if source != config.peer {
                    continue;
                }
                let Some(pose) = LiveLinkPose::decode(&buffer[..received]) else {
                    continue;
                };
                link.accept_remote_pose(pose, time.elapsed_seconds());
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(error) => {
                link.last_error = Some(format!("live-link receive failed: {error}"));
                break;
            }
        }
    }
}

fn expire_stale_user_authority(time: Res<Time>, mut link: ResMut<LiveLink>) {
    if link.expire_stale_user_authority(time.elapsed_seconds()) {
        info!("live link: USER authority lease expired; CODEX resumed QA control");
    }
}

fn capture_held_inputs(keys: &ButtonInput<KeyCode>, mouse: &ButtonInput<MouseButton>) -> u32 {
    let mut mask = 0_u32;
    for (bit, key) in [
        (INPUT_W, KeyCode::KeyW),
        (INPUT_A, KeyCode::KeyA),
        (INPUT_S, KeyCode::KeyS),
        (INPUT_D, KeyCode::KeyD),
        (INPUT_SPACE, KeyCode::Space),
        (INPUT_Q, KeyCode::KeyQ),
        (INPUT_E, KeyCode::KeyE),
        (INPUT_F3, KeyCode::F3),
        (INPUT_ESCAPE, KeyCode::Escape),
    ] {
        if keys.pressed(key) {
            mask |= bit;
        }
    }
    if keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]) {
        mask |= INPUT_CONTROL;
    }
    if keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]) {
        mask |= INPUT_SHIFT;
    }
    if mouse.pressed(MouseButton::Left) {
        mask |= INPUT_MOUSE_LEFT;
    }
    if mouse.pressed(MouseButton::Right) {
        mask |= INPUT_MOUSE_RIGHT;
    }
    mask
}

fn apply_mirrored_key(
    keys: &mut ButtonInput<KeyCode>,
    key: KeyCode,
    bit: u32,
    previous: u32,
    desired: u32,
) {
    let was_held = previous & bit != 0;
    let is_held = desired & bit != 0;
    keys.reset(key);
    if is_held {
        keys.press(key);
        if was_held {
            keys.clear_just_pressed(key);
        }
    } else if was_held {
        keys.press(key);
        keys.clear_just_pressed(key);
        keys.release(key);
    }
}

fn apply_mirrored_mouse_button(
    mouse: &mut ButtonInput<MouseButton>,
    button: MouseButton,
    bit: u32,
    previous: u32,
    desired: u32,
) {
    let was_held = previous & bit != 0;
    let is_held = desired & bit != 0;
    mouse.reset(button);
    if is_held {
        mouse.press(button);
        if was_held {
            mouse.clear_just_pressed(button);
        }
    } else if was_held {
        mouse.press(button);
        mouse.clear_just_pressed(button);
        mouse.release(button);
    }
}

fn apply_mirrored_inputs(
    keys: &mut ButtonInput<KeyCode>,
    mouse: &mut ButtonInput<MouseButton>,
    previous: u32,
    desired: u32,
) {
    for (bit, key) in [
        (INPUT_W, KeyCode::KeyW),
        (INPUT_A, KeyCode::KeyA),
        (INPUT_S, KeyCode::KeyS),
        (INPUT_D, KeyCode::KeyD),
        (INPUT_SPACE, KeyCode::Space),
        (INPUT_Q, KeyCode::KeyQ),
        (INPUT_E, KeyCode::KeyE),
        (INPUT_F3, KeyCode::F3),
        (INPUT_ESCAPE, KeyCode::Escape),
    ] {
        apply_mirrored_key(keys, key, bit, previous, desired);
    }

    // Normalize either physical modifier to the left-side variant on the
    // follower. Both local variants are reset, so neither can leak through.
    keys.reset(KeyCode::ControlRight);
    keys.reset(KeyCode::ShiftRight);
    apply_mirrored_key(keys, KeyCode::ControlLeft, INPUT_CONTROL, previous, desired);
    apply_mirrored_key(keys, KeyCode::ShiftLeft, INPUT_SHIFT, previous, desired);
    apply_mirrored_mouse_button(
        mouse,
        MouseButton::Left,
        INPUT_MOUSE_LEFT,
        previous,
        desired,
    );
    apply_mirrored_mouse_button(
        mouse,
        MouseButton::Right,
        INPUT_MOUSE_RIGHT,
        previous,
        desired,
    );
}

fn gate_and_mirror_live_link_inputs(
    time: Res<Time>,
    mut link: ResMut<LiveLink>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut mouse_motion: ResMut<Events<MouseMotion>>,
    mut mouse_wheel: ResMut<Events<MouseWheel>>,
) {
    let previous = link.mirrored_input_mask;
    if !link.is_following() {
        if previous != 0 {
            // Flush mirrored held state exactly once when leadership changes.
            // The next native input frame owns these buttons again.
            apply_mirrored_inputs(&mut keys, &mut mouse, previous, 0);
            link.mirrored_input_mask = 0;
        }
        return;
    }

    // The remote pose already carries authoritative look. Discarding local
    // relative motion and wheel gestures prevents an unfocused spectator from
    // briefly steering or changing tools before the pose correction runs.
    mouse_motion.clear();
    mouse_wheel.clear();
    let desired = if link.peer_is_live(time.elapsed_seconds()) {
        link.remote_pose
            .map(|pose| pose.held_inputs)
            .unwrap_or_default()
    } else {
        0
    };
    // Reset every mirrored gameplay button before reconstructing the remote
    // held/edge state. This is the local-input gate for a follower instance.
    apply_mirrored_inputs(&mut keys, &mut mouse, previous, desired);
    link.mirrored_input_mask = desired;
}

fn sync_agent_control_ownership(
    time: Res<Time>,
    link: Res<LiveLink>,
    agent: Option<ResMut<AgentControlState>>,
) {
    if let Some(mut agent) = agent {
        let suppressed = link.suppresses_agent_gameplay(time.elapsed_seconds());
        // Calling a mutating method through ResMut marks the resource changed
        // even if the value is identical. Compare through the shared deref
        // first so downstream Changed<AgentControlState> systems stay quiet.
        if agent.live_link_suppressed() != suppressed {
            agent.set_live_link_suppressed(suppressed);
        }
    }
}

fn apply_remote_live_link_pose(
    time: Res<Time>,
    link: Res<LiveLink>,
    mut player_q: Query<(&mut Transform, &mut Player)>,
) {
    let Ok((mut transform, mut player)) = player_q.get_single_mut() else {
        return;
    };
    let now = time.elapsed_seconds();
    if link.is_following() && link.peer_is_live(now) {
        if let Some(remote) = link.remote_pose.filter(|pose| pose.pose_valid) {
            transform.translation = remote.position;
            transform.rotation = Quat::from_axis_angle(Vec3::Y, remote.yaw)
                * Quat::from_axis_angle(Vec3::X, remote.pitch);
            player.yaw = remote.yaw;
            player.pitch = remote.pitch.clamp(-1.54, 1.54);
            player.velocity = remote.velocity;
            player.flying = remote.flying;
            player.on_ground = remote.on_ground;
            player.placed_on_surface = true;
        }
    }
}

fn send_live_link_heartbeat(
    time: Res<Time>,
    mut link: ResMut<LiveLink>,
    player_q: Query<(&Transform, &Player)>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
) {
    link.send_accumulator += time.delta_seconds().min(0.1);
    if link.send_accumulator < LIVE_LINK_SEND_INTERVAL {
        return;
    }
    link.send_accumulator %= LIVE_LINK_SEND_INTERVAL;
    link.advance_local_sequence();
    let Some(config) = link.config else {
        return;
    };
    let socket = match link.socket.as_ref().map(UdpSocket::try_clone) {
        Some(Ok(socket)) => socket,
        Some(Err(error)) => {
            link.last_error = Some(format!("could not clone live-link socket: {error}"));
            return;
        }
        None => return,
    };
    let local_pose = player_q.get_single().ok();
    let (position, velocity, yaw, pitch, flying, on_ground) = local_pose.map_or(
        (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0, false, false),
        |(transform, player)| {
            (
                transform.translation,
                player.velocity,
                player.yaw,
                player.pitch,
                player.flying,
                player.on_ground,
            )
        },
    );
    let packet = LiveLinkPose {
        boot_id: link.boot_id,
        sequence: link.sequence,
        side: config.side,
        user_leads: link.user_leads,
        pose_valid: local_pose.is_some(),
        held_inputs: capture_held_inputs(&keys, &mouse),
        position,
        velocity,
        yaw,
        pitch,
        flying,
        on_ground,
    }
    .encode();
    if let Err(error) = socket.send_to(&packet, config.peer) {
        link.last_error = Some(format!("live-link send failed: {error}"));
    }
}

fn draw_live_link_overlay(mut contexts: EguiContexts, time: Res<Time>, link: Res<LiveLink>) {
    let ctx = contexts.ctx_mut();
    let now = time.elapsed_seconds();
    let linked = link.peer_is_live(now);
    let (mut accent, detail) = if linked {
        if link.user_leads {
            (
                egui::Color32::from_rgb(255, 197, 84),
                "F10: return to CODEX spectator",
            )
        } else {
            (
                egui::Color32::from_rgb(75, 226, 255),
                "F10: JOIN and lead the test",
            )
        }
    } else {
        (
            egui::Color32::from_rgb(255, 96, 110),
            "waiting for local peer",
        )
    };
    if link.last_error.is_some() {
        accent = egui::Color32::from_rgb(255, 96, 110);
    }
    let show_join_hint = link.local_side() == Some(LiveLinkSide::User);
    egui::Area::new(egui::Id::new("voxel_native_live_link"))
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_rgba_unmultiplied(7, 12, 20, 224))
                .stroke(egui::Stroke::new(1.2, accent))
                .rounding(egui::Rounding::same(7.0))
                .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                .show(ui, |ui| {
                    ui.set_max_width(720.0);
                    ui.horizontal(|ui| {
                        ui.colored_label(accent, "●");
                        ui.label(
                            egui::RichText::new(link.mode_label(now))
                                .strong()
                                .monospace()
                                .color(egui::Color32::WHITE),
                        );
                        if show_join_hint {
                            ui.label(
                                egui::RichText::new(detail)
                                    .monospace()
                                    .small()
                                    .color(egui::Color32::from_gray(190)),
                            );
                        }
                    });
                    if let Some(error) = link.last_error.as_deref() {
                        ui.label(
                            egui::RichText::new(error)
                                .monospace()
                                .small()
                                .color(egui::Color32::from_rgb(255, 170, 176)),
                        );
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pose(boot_id: u64, sequence: u64, side: LiveLinkSide) -> LiveLinkPose {
        LiveLinkPose {
            boot_id,
            sequence,
            side,
            user_leads: false,
            pose_valid: true,
            held_inputs: 0,
            position: Vec3::new(12.0, 34.0, 56.0),
            velocity: Vec3::new(1.0, 2.0, 3.0),
            yaw: 0.5,
            pitch: -0.25,
            flying: false,
            on_ground: true,
        }
    }

    fn test_config(side: LiveLinkSide) -> LiveLinkConfig {
        LiveLinkConfig {
            side,
            bind: "127.0.0.1:47811".parse().unwrap(),
            peer: "127.0.0.1:47812".parse().unwrap(),
        }
    }

    #[test]
    fn live_link_is_disabled_when_no_environment_is_configured() {
        assert_eq!(parse_live_link_config(None, None, None), Ok(None));
    }

    #[test]
    fn live_link_requires_a_complete_loopback_pair() {
        assert!(parse_live_link_config(Some("codex"), None, Some("127.0.0.1:47812")).is_err());
        assert!(parse_live_link_config(
            Some("user"),
            Some("0.0.0.0:47812"),
            Some("127.0.0.1:47811")
        )
        .is_err());
        assert!(parse_live_link_config(
            Some("codex"),
            Some("127.0.0.1:47811"),
            Some("192.168.1.2:47812")
        )
        .is_err());
        assert!(parse_live_link_config(
            Some("codex"),
            Some("127.0.0.1:0"),
            Some("127.0.0.1:47812")
        )
        .is_err());
        assert!(parse_live_link_config(
            Some("codex"),
            Some("127.0.0.1:47811"),
            Some("127.0.0.1:0")
        )
        .is_err());
        assert!(parse_live_link_config(
            Some("codex"),
            Some("127.0.0.1:47811"),
            Some("[::1]:47812")
        )
        .is_err());
    }

    #[test]
    fn live_link_config_accepts_distinct_loopback_endpoints() {
        let config = parse_live_link_config(
            Some("CODEX"),
            Some("127.0.0.1:47811"),
            Some("127.0.0.1:47812"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.side, LiveLinkSide::Codex);
        assert_eq!(config.bind, "127.0.0.1:47811".parse().unwrap());
        assert_eq!(config.peer, "127.0.0.1:47812".parse().unwrap());
    }

    #[test]
    fn pose_packet_round_trips_exactly() {
        let pose = LiveLinkPose {
            boot_id: 0x1234_5678_9abc_def0,
            sequence: 77,
            side: LiveLinkSide::User,
            user_leads: true,
            pose_valid: true,
            held_inputs: INPUT_W | INPUT_MOUSE_LEFT,
            position: Vec3::new(12.5, -4.25, 900.0),
            velocity: Vec3::new(-1.0, 2.0, 3.5),
            yaw: -2.25,
            pitch: 0.75,
            flying: true,
            on_ground: false,
        };
        assert_eq!(LiveLinkPose::decode(&pose.encode()), Some(pose));
    }

    #[test]
    fn pose_packet_rejects_wrong_version_and_non_finite_values() {
        let mut packet = LiveLinkPose {
            boot_id: 42,
            sequence: 1,
            side: LiveLinkSide::Codex,
            user_leads: false,
            pose_valid: true,
            held_inputs: 0,
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            flying: false,
            on_ground: true,
        }
        .encode();
        packet[4] = LIVE_LINK_VERSION + 1;
        assert!(LiveLinkPose::decode(&packet).is_none());

        packet[4] = LIVE_LINK_VERSION;
        write_f32(&mut packet, 24, f32::NAN);
        assert!(LiveLinkPose::decode(&packet).is_none());
    }

    #[test]
    fn pose_packet_rejects_oversized_extreme_and_unknown_data() {
        let packet = test_pose(42, 1, LiveLinkSide::Codex).encode();
        let mut oversized = packet.to_vec();
        oversized.push(0);
        assert!(LiveLinkPose::decode(&oversized).is_none());

        let mut extreme_position = packet;
        write_f32(&mut extreme_position, 24, MAX_ABS_POSITION + 1.0);
        assert!(LiveLinkPose::decode(&extreme_position).is_none());

        let mut extreme_velocity = packet;
        write_f32(&mut extreme_velocity, 36, MAX_ABS_VELOCITY + 1.0);
        assert!(LiveLinkPose::decode(&extreme_velocity).is_none());

        let mut extreme_yaw = packet;
        write_f32(&mut extreme_yaw, 48, MAX_ABS_YAW + 1.0);
        assert!(LiveLinkPose::decode(&extreme_yaw).is_none());

        let mut unknown_flags = packet;
        unknown_flags[6..8].copy_from_slice(&(1_u16 << 15).to_le_bytes());
        assert!(LiveLinkPose::decode(&unknown_flags).is_none());

        let mut zero_boot = packet;
        zero_boot[8..16].copy_from_slice(&0_u64.to_le_bytes());
        assert!(LiveLinkPose::decode(&zero_boot).is_none());

        let mut zero_sequence = packet;
        zero_sequence[16..24].copy_from_slice(&0_u64.to_le_bytes());
        assert!(LiveLinkPose::decode(&zero_sequence).is_none());

        let mut unknown_input = packet;
        unknown_input[56..60].copy_from_slice(&(1_u32 << 31).to_le_bytes());
        assert!(LiveLinkPose::decode(&unknown_input).is_none());

        let mut non_zero_reserved = packet;
        non_zero_reserved[63] = 1;
        assert!(LiveLinkPose::decode(&non_zero_reserved).is_none());
    }

    #[test]
    fn heartbeat_without_a_player_is_valid_but_cannot_teleport_a_follower() {
        let mut heartbeat = test_pose(42, 1, LiveLinkSide::Codex);
        heartbeat.pose_valid = false;
        heartbeat.position = Vec3::ZERO;
        heartbeat.velocity = Vec3::ZERO;

        let decoded = LiveLinkPose::decode(&heartbeat.encode()).expect("valid heartbeat");
        assert!(!decoded.pose_valid);
    }

    #[test]
    fn relevant_user_inputs_are_encoded_as_a_held_mask() {
        let mut keys = ButtonInput::<KeyCode>::default();
        let mut mouse = ButtonInput::<MouseButton>::default();
        for key in [
            KeyCode::KeyW,
            KeyCode::ControlRight,
            KeyCode::KeyQ,
            KeyCode::F3,
            KeyCode::Escape,
            KeyCode::ShiftLeft,
        ] {
            keys.press(key);
        }
        mouse.press(MouseButton::Right);

        assert_eq!(
            capture_held_inputs(&keys, &mouse),
            INPUT_W
                | INPUT_CONTROL
                | INPUT_Q
                | INPUT_F3
                | INPUT_ESCAPE
                | INPUT_SHIFT
                | INPUT_MOUSE_RIGHT
        );
    }

    #[test]
    fn follower_gate_suppresses_local_buttons_and_preserves_remote_edges() {
        let mut keys = ButtonInput::<KeyCode>::default();
        let mut mouse = ButtonInput::<MouseButton>::default();
        keys.press(KeyCode::KeyA);
        mouse.press(MouseButton::Right);
        let mirrored = INPUT_W | INPUT_MOUSE_LEFT;

        apply_mirrored_inputs(&mut keys, &mut mouse, 0, mirrored);
        assert!(!keys.pressed(KeyCode::KeyA));
        assert!(!mouse.pressed(MouseButton::Right));
        assert!(keys.pressed(KeyCode::KeyW));
        assert!(keys.just_pressed(KeyCode::KeyW));
        assert!(mouse.pressed(MouseButton::Left));
        assert!(mouse.just_pressed(MouseButton::Left));

        apply_mirrored_inputs(&mut keys, &mut mouse, mirrored, mirrored);
        assert!(keys.pressed(KeyCode::KeyW));
        assert!(!keys.just_pressed(KeyCode::KeyW));
        assert!(mouse.pressed(MouseButton::Left));
        assert!(!mouse.just_pressed(MouseButton::Left));

        apply_mirrored_inputs(&mut keys, &mut mouse, mirrored, 0);
        assert!(!keys.pressed(KeyCode::KeyW));
        assert!(keys.just_released(KeyCode::KeyW));
        assert!(!mouse.pressed(MouseButton::Left));
        assert!(mouse.just_released(MouseButton::Left));
    }

    #[test]
    fn unchanged_live_link_ownership_does_not_mark_agent_control_changed() {
        let mut world = World::new();
        world.init_resource::<Time>();
        world.insert_resource(LiveLink::default());
        world.insert_resource(AgentControlState::default());
        world.clear_trackers();

        let mut schedule = Schedule::default();
        schedule.add_systems(sync_agent_control_ownership);
        schedule.run(&mut world);

        let agent = world.resource_ref::<AgentControlState>();
        assert!(
            !agent.is_changed(),
            "an identical suppression state must not trigger Changed<AgentControlState>"
        );
    }

    #[test]
    fn a_new_peer_boot_resets_sequence_and_retires_the_old_stream() {
        let mut link = LiveLink {
            config: Some(test_config(LiveLinkSide::Codex)),
            ..default()
        };
        assert!(link.accept_remote_pose(test_pose(10, 900, LiveLinkSide::User), 1.0));
        assert_eq!(link.last_received_sequence, 900);

        let mut restarted = test_pose(11, 1, LiveLinkSide::User);
        restarted.user_leads = true;
        assert!(link.accept_remote_pose(restarted, 1.1));
        assert_eq!(link.remote_boot_id, Some(11));
        assert_eq!(link.last_received_sequence, 1);
        assert!(link.user_leads);

        assert!(!link.accept_remote_pose(test_pose(10, 901, LiveLinkSide::User), 1.2));
        assert_eq!(link.remote_boot_id, Some(11));
        assert_eq!(link.last_received_sequence, 1);
    }

    #[test]
    fn codex_authority_recovers_after_the_user_lease_times_out() {
        let mut link = LiveLink {
            socket: UdpSocket::bind("127.0.0.1:0").ok(),
            config: Some(test_config(LiveLinkSide::Codex)),
            ..default()
        };
        let mut joined = test_pose(10, 1, LiveLinkSide::User);
        joined.user_leads = true;
        assert!(link.accept_remote_pose(joined, 5.0));
        assert!(link.suppresses_agent_gameplay(5.5));
        assert!(!link.expire_stale_user_authority(5.5));

        assert!(link.expire_stale_user_authority(6.01));
        assert!(!link.user_leads);
        assert!(!link.suppresses_agent_gameplay(6.01));
        assert!(!link.expire_stale_user_authority(7.0));
    }

    #[test]
    fn local_sequence_rollover_starts_a_fresh_stream() {
        let mut link = LiveLink {
            sequence: u64::MAX,
            ..default()
        };
        let old_boot = link.boot_id;

        link.advance_local_sequence();

        assert_eq!(link.sequence, 1);
        assert_ne!(link.boot_id, old_boot);
    }

    #[test]
    fn user_leadership_suppresses_agent_gameplay_on_both_sides() {
        let mut link = LiveLink::default();
        link.config = Some(test_config(LiveLinkSide::Codex));
        link.socket = UdpSocket::bind("127.0.0.1:0").ok();
        assert!(!link.suppresses_agent_gameplay(1.0));
        link.user_leads = true;
        link.remote_pose = Some(LiveLinkPose {
            boot_id: 7,
            sequence: 1,
            side: LiveLinkSide::User,
            user_leads: true,
            pose_valid: false,
            held_inputs: 0,
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            flying: false,
            on_ground: false,
        });
        link.last_receive_seconds = 1.0;
        assert!(link.suppresses_agent_gameplay(1.0));
    }
}
