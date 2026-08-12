//! Fixed-layout core for the future direct agent/engine bridge.
//!
//! This module deliberately stops before operating-system shared memory.  It
//! proves the bounded queue, sequence, corruption-containment, and telemetry
//! rules in-process first.  A mapped transport must validate the byte prefix,
//! mapping length, alignment, atomic capabilities, and process lifetime before
//! it may construct endpoints around these bytes.

use std::cell::UnsafeCell;
use std::fmt;
use std::marker::PhantomData;
use std::mem::{align_of, size_of};
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};

pub const BRIDGE_MAGIC: [u8; 8] = *b"VXAGBRG1";
pub const BRIDGE_ABI_VERSION: u16 = 1;
pub const BRIDGE_ENDIAN_LITTLE: u32 = 0x4c45_0001;

pub const COMMAND_SLOT_COUNT: usize = 256;
pub const EVENT_SLOT_COUNT: usize = 512;
pub const COMMAND_PAYLOAD_CAPACITY: usize = 512;
pub const EVENT_PAYLOAD_CAPACITY: usize = 256;
pub const TELEMETRY_CAPACITY: usize = 64 * 1024;
pub const COMMAND_DRAIN_BUDGET: usize = 32;
pub const UNACKNOWLEDGED_MUTATION_WINDOW: usize = 64;
pub const REGION_HEADER_SIZE_BYTES: usize = 576;
pub const COMMAND_SLOT_SIZE_BYTES: usize = 576;
pub const EVENT_SLOT_SIZE_BYTES: usize = 320;
pub const TELEMETRY_AREA_SIZE_BYTES: usize = 65_600;

const PREFIX_SIZE: usize = 64;
const TELEMETRY_WORD_COUNT: usize = TELEMETRY_CAPACITY / size_of::<u64>();
const HALF_SEQUENCE_SPACE: u64 = 1_u64 << 63;

const _: () = assert!(COMMAND_SLOT_COUNT.is_power_of_two());
const _: () = assert!(EVENT_SLOT_COUNT.is_power_of_two());
const _: () = assert!(TELEMETRY_CAPACITY % size_of::<u64>() == 0);
const _: () = assert!(COMMAND_DRAIN_BUDGET <= COMMAND_SLOT_COUNT);
const _: () = assert!(UNACKNOWLEDGED_MUTATION_WINDOW <= COMMAND_SLOT_COUNT);

fn increment_saturating(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(1))
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionNonce([u8; 16]);

impl SessionNonce {
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                return Some(Self(bytes));
            }
            index += 1;
        }
        None
    }

    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CommandClass {
    Query = 0,
    Continuous = 1,
    Mutation = 2,
    Authority = 3,
}

impl CommandClass {
    pub const fn permits_coalescing(self) -> bool {
        matches!(self, Self::Continuous)
    }

    pub const fn mutates_authoritative_state(self) -> bool {
        matches!(self, Self::Mutation | Self::Authority)
    }

    const fn from_byte(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Query),
            1 => Some(Self::Continuous),
            2 => Some(Self::Mutation),
            3 => Some(Self::Authority),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct CommandMessage {
    pub sequence: u64,
    pub world_epoch: u64,
    pub deadline_ns: u64,
    pub opcode: u16,
    pub class: CommandClass,
    payload_len: u16,
    payload: [u8; COMMAND_PAYLOAD_CAPACITY],
}

impl CommandMessage {
    pub fn new(
        sequence: u64,
        world_epoch: u64,
        deadline_ns: u64,
        opcode: u16,
        class: CommandClass,
        payload: &[u8],
    ) -> Result<Self, BridgeError> {
        if payload.len() > COMMAND_PAYLOAD_CAPACITY {
            return Err(BridgeError::PayloadTooLarge {
                capacity: COMMAND_PAYLOAD_CAPACITY,
                actual: payload.len(),
            });
        }
        let mut storage = [0_u8; COMMAND_PAYLOAD_CAPACITY];
        storage[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            sequence,
            world_epoch,
            deadline_ns,
            opcode,
            class,
            payload_len: payload.len() as u16,
            payload: storage,
        })
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload[..usize::from(self.payload_len)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct EventMessage {
    pub sequence: u64,
    pub world_epoch: u64,
    pub code: u16,
    pub status: u16,
    payload_len: u16,
    payload: [u8; EVENT_PAYLOAD_CAPACITY],
}

impl EventMessage {
    pub fn new(
        sequence: u64,
        world_epoch: u64,
        code: u16,
        status: u16,
        payload: &[u8],
    ) -> Result<Self, BridgeError> {
        if payload.len() > EVENT_PAYLOAD_CAPACITY {
            return Err(BridgeError::PayloadTooLarge {
                capacity: EVENT_PAYLOAD_CAPACITY,
                actual: payload.len(),
            });
        }
        let mut storage = [0_u8; EVENT_PAYLOAD_CAPACITY];
        storage[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            sequence,
            world_epoch,
            code,
            status,
            payload_len: payload.len() as u16,
            payload: storage,
        })
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload[..usize::from(self.payload_len)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeError {
    UnsupportedEndian,
    InvalidNonce,
    TruncatedPrefix { required: usize, actual: usize },
    TruncatedRegion { required: usize, actual: usize },
    CorruptMagic,
    UnsupportedVersion { found: u16 },
    CorruptHeaderSize { found: usize },
    CorruptTotalSize { found: u64 },
    CorruptReservedPrefix,
    SessionMismatch,
    PayloadTooLarge { capacity: usize, actual: usize },
    QueueFull,
    QueueEmpty,
    CorruptCursor,
    CorruptLength { capacity: usize, actual: usize },
    CorruptCommandClass { found: u8 },
    CorruptReservedField,
    TrackerEpochMismatch { tracker: u64, region: u64 },
    TelemetryUnavailable,
    TelemetryBusy,
    TelemetryWriterBusy,
    BufferTooSmall { required: usize, actual: usize },
    NonContinuousCoalescing,
    CoalescingKeyMismatch,
    CoalescingSequenceNotNewer,
}

impl fmt::Display for BridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for BridgeError {}

#[repr(C)]
#[derive(Clone, Copy)]
struct RegionPrefix {
    magic: [u8; 8],
    abi_version_le: u16,
    header_size_le: u16,
    endian_tag_le: u32,
    total_size_le: u64,
    session_nonce: [u8; 16],
    reserved: [u8; 24],
}

const _: () = assert!(size_of::<RegionPrefix>() == PREFIX_SIZE);
const _: () = assert!(align_of::<RegionPrefix>() == align_of::<u64>());
const _: () = assert!(std::mem::offset_of!(RegionPrefix, magic) == 0);
const _: () = assert!(std::mem::offset_of!(RegionPrefix, abi_version_le) == 8);
const _: () = assert!(std::mem::offset_of!(RegionPrefix, header_size_le) == 10);
const _: () = assert!(std::mem::offset_of!(RegionPrefix, endian_tag_le) == 12);
const _: () = assert!(std::mem::offset_of!(RegionPrefix, total_size_le) == 16);
const _: () = assert!(std::mem::offset_of!(RegionPrefix, session_nonce) == 24);

#[repr(C, align(64))]
struct CursorLine {
    value: AtomicU64,
    reserved: [u8; 56],
}

impl CursorLine {
    fn new(value: u64) -> Self {
        Self {
            value: AtomicU64::new(value),
            reserved: [0; 56],
        }
    }
}

const _: () = assert!(size_of::<CursorLine>() == 64);
const _: () = assert!(align_of::<CursorLine>() == 64);

#[repr(C, align(64))]
struct CorruptionLine {
    header: AtomicU64,
    length: AtomicU64,
    cursor: AtomicU64,
    class: AtomicU64,
    stale_epoch: AtomicU64,
    duplicate_sequence: AtomicU64,
    out_of_order_sequence: AtomicU64,
    expired: AtomicU64,
    queue_full: AtomicU64,
    telemetry_retries: AtomicU64,
    command_peak_occupancy: AtomicU64,
    event_peak_occupancy: AtomicU64,
    reserved_field: AtomicU64,
    reserved: [u8; 24],
}

impl CorruptionLine {
    fn new() -> Self {
        Self {
            header: AtomicU64::new(0),
            length: AtomicU64::new(0),
            cursor: AtomicU64::new(0),
            class: AtomicU64::new(0),
            stale_epoch: AtomicU64::new(0),
            duplicate_sequence: AtomicU64::new(0),
            out_of_order_sequence: AtomicU64::new(0),
            expired: AtomicU64::new(0),
            queue_full: AtomicU64::new(0),
            telemetry_retries: AtomicU64::new(0),
            command_peak_occupancy: AtomicU64::new(0),
            event_peak_occupancy: AtomicU64::new(0),
            reserved_field: AtomicU64::new(0),
            reserved: [0; 24],
        }
    }
}

const _: () = assert!(size_of::<CorruptionLine>() == 128);
const _: () = assert!(align_of::<CorruptionLine>() == 64);

#[repr(C, align(64))]
struct RegionHeader {
    prefix: RegionPrefix,
    world_epoch: CursorLine,
    command_producer: CursorLine,
    command_consumer: CursorLine,
    event_producer: CursorLine,
    event_consumer: CursorLine,
    telemetry_generation: CursorLine,
    corruption: CorruptionLine,
}

const EXPECTED_HEADER_SIZE: usize = PREFIX_SIZE + 6 * 64 + 128;
const _: () = assert!(EXPECTED_HEADER_SIZE == REGION_HEADER_SIZE_BYTES);
const _: () = assert!(size_of::<RegionHeader>() == EXPECTED_HEADER_SIZE);
const _: () = assert!(align_of::<RegionHeader>() == 64);
const _: () = assert!(std::mem::offset_of!(RegionHeader, prefix) == 0);
const _: () = assert!(std::mem::offset_of!(RegionHeader, world_epoch) == 64);
const _: () = assert!(std::mem::offset_of!(RegionHeader, command_producer) == 128);
const _: () = assert!(std::mem::offset_of!(RegionHeader, command_consumer) == 192);
const _: () = assert!(std::mem::offset_of!(RegionHeader, event_producer) == 256);
const _: () = assert!(std::mem::offset_of!(RegionHeader, event_consumer) == 320);
const _: () = assert!(std::mem::offset_of!(RegionHeader, telemetry_generation) == 384);
const _: () = assert!(std::mem::offset_of!(RegionHeader, corruption) == 448);

#[derive(Clone, Copy)]
#[repr(C, align(64))]
struct CommandSlot {
    sequence_le: u64,
    world_epoch_le: u64,
    deadline_ns_le: u64,
    opcode_le: u16,
    payload_len_le: u16,
    class: u8,
    reserved: [u8; 3],
    payload: [u8; COMMAND_PAYLOAD_CAPACITY],
}

impl CommandSlot {
    const EMPTY: Self = Self {
        sequence_le: 0,
        world_epoch_le: 0,
        deadline_ns_le: 0,
        opcode_le: 0,
        payload_len_le: 0,
        class: 0,
        reserved: [0; 3],
        payload: [0; COMMAND_PAYLOAD_CAPACITY],
    };

    fn encode(message: &CommandMessage) -> Self {
        let mut slot = Self::EMPTY;
        slot.sequence_le = message.sequence.to_le();
        slot.world_epoch_le = message.world_epoch.to_le();
        slot.deadline_ns_le = message.deadline_ns.to_le();
        slot.opcode_le = message.opcode.to_le();
        slot.payload_len_le = message.payload_len.to_le();
        slot.class = message.class as u8;
        slot.payload[..usize::from(message.payload_len)].copy_from_slice(message.payload());
        slot
    }

    fn decode(self) -> Result<CommandMessage, BridgeError> {
        let payload_len = usize::from(u16::from_le(self.payload_len_le));
        if payload_len > COMMAND_PAYLOAD_CAPACITY {
            return Err(BridgeError::CorruptLength {
                capacity: COMMAND_PAYLOAD_CAPACITY,
                actual: payload_len,
            });
        }
        let class = CommandClass::from_byte(self.class)
            .ok_or(BridgeError::CorruptCommandClass { found: self.class })?;
        if self.reserved.iter().any(|byte| *byte != 0) {
            return Err(BridgeError::CorruptReservedField);
        }
        Ok(CommandMessage {
            sequence: u64::from_le(self.sequence_le),
            world_epoch: u64::from_le(self.world_epoch_le),
            deadline_ns: u64::from_le(self.deadline_ns_le),
            opcode: u16::from_le(self.opcode_le),
            class,
            payload_len: payload_len as u16,
            payload: self.payload,
        })
    }
}

const _: () = assert!(size_of::<CommandSlot>() == COMMAND_SLOT_SIZE_BYTES);
const _: () = assert!(align_of::<CommandSlot>() == 64);
const _: () = assert!(std::mem::offset_of!(CommandSlot, sequence_le) == 0);
const _: () = assert!(std::mem::offset_of!(CommandSlot, world_epoch_le) == 8);
const _: () = assert!(std::mem::offset_of!(CommandSlot, deadline_ns_le) == 16);
const _: () = assert!(std::mem::offset_of!(CommandSlot, payload) == 32);

#[derive(Clone, Copy)]
#[repr(C, align(64))]
struct EventSlot {
    sequence_le: u64,
    world_epoch_le: u64,
    code_le: u16,
    status_le: u16,
    payload_len_le: u16,
    reserved: [u8; 2],
    payload: [u8; EVENT_PAYLOAD_CAPACITY],
}

impl EventSlot {
    const EMPTY: Self = Self {
        sequence_le: 0,
        world_epoch_le: 0,
        code_le: 0,
        status_le: 0,
        payload_len_le: 0,
        reserved: [0; 2],
        payload: [0; EVENT_PAYLOAD_CAPACITY],
    };

    fn encode(message: &EventMessage) -> Self {
        let mut slot = Self::EMPTY;
        slot.sequence_le = message.sequence.to_le();
        slot.world_epoch_le = message.world_epoch.to_le();
        slot.code_le = message.code.to_le();
        slot.status_le = message.status.to_le();
        slot.payload_len_le = message.payload_len.to_le();
        slot.payload[..usize::from(message.payload_len)].copy_from_slice(message.payload());
        slot
    }

    fn decode(self) -> Result<EventMessage, BridgeError> {
        let payload_len = usize::from(u16::from_le(self.payload_len_le));
        if payload_len > EVENT_PAYLOAD_CAPACITY {
            return Err(BridgeError::CorruptLength {
                capacity: EVENT_PAYLOAD_CAPACITY,
                actual: payload_len,
            });
        }
        if self.reserved.iter().any(|byte| *byte != 0) {
            return Err(BridgeError::CorruptReservedField);
        }
        Ok(EventMessage {
            sequence: u64::from_le(self.sequence_le),
            world_epoch: u64::from_le(self.world_epoch_le),
            code: u16::from_le(self.code_le),
            status: u16::from_le(self.status_le),
            payload_len: payload_len as u16,
            payload: self.payload,
        })
    }
}

const _: () = assert!(size_of::<EventSlot>() == EVENT_SLOT_SIZE_BYTES);
const _: () = assert!(align_of::<EventSlot>() == 64);
const _: () = assert!(std::mem::offset_of!(EventSlot, payload) == 24);

#[repr(C, align(64))]
struct TelemetryArea {
    payload_len: AtomicU32,
    reserved: AtomicU32,
    world_epoch: AtomicU64,
    reflected_command_sequence: AtomicU64,
    words: [AtomicU64; TELEMETRY_WORD_COUNT],
}

impl TelemetryArea {
    fn new() -> Self {
        Self {
            payload_len: AtomicU32::new(0),
            reserved: AtomicU32::new(0),
            world_epoch: AtomicU64::new(0),
            reflected_command_sequence: AtomicU64::new(0),
            words: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

const _: () = assert!(size_of::<TelemetryArea>() == TELEMETRY_AREA_SIZE_BYTES);
const _: () = assert!(align_of::<TelemetryArea>() == 64);
const _: () = assert!(std::mem::offset_of!(TelemetryArea, payload_len) == 0);
const _: () = assert!(std::mem::offset_of!(TelemetryArea, world_epoch) == 8);
const _: () = assert!(std::mem::offset_of!(TelemetryArea, reflected_command_sequence) == 16);
const _: () = assert!(std::mem::offset_of!(TelemetryArea, words) == 24);

/// Fixed-layout storage.  Safe callers can only obtain one agent endpoint and
/// one engine endpoint from a mutable borrow, enforcing the SPSC ownership
/// rule for the two rings.
#[repr(C, align(64))]
pub struct AgentDirectRegion {
    header: RegionHeader,
    command_slots: [UnsafeCell<CommandSlot>; COMMAND_SLOT_COUNT],
    event_slots: [UnsafeCell<EventSlot>; EVENT_SLOT_COUNT],
    telemetry: TelemetryArea,
}

/// `UnsafeCell` ring slots are synchronized by the release/acquire cursor
/// protocol.  `split` is the only safe endpoint constructor and its mutable
/// borrow prevents a second producer or consumer from being created while the
/// first pair exists.  Telemetry words are independently atomic.
unsafe impl Sync for AgentDirectRegion {}

pub const REGION_SIZE_BYTES: usize = size_of::<AgentDirectRegion>();
pub const REGION_ALIGNMENT_BYTES: usize = align_of::<AgentDirectRegion>();

const EXPECTED_REGION_SIZE: usize = REGION_HEADER_SIZE_BYTES
    + COMMAND_SLOT_COUNT * COMMAND_SLOT_SIZE_BYTES
    + EVENT_SLOT_COUNT * EVENT_SLOT_SIZE_BYTES
    + TELEMETRY_AREA_SIZE_BYTES;
const _: () = assert!(REGION_SIZE_BYTES == EXPECTED_REGION_SIZE);
const _: () = assert!(REGION_ALIGNMENT_BYTES == 64);
const _: () = assert!(std::mem::offset_of!(AgentDirectRegion, header) == 0);
const _: () = assert!(std::mem::offset_of!(AgentDirectRegion, command_slots) == 576);
const _: () = assert!(std::mem::offset_of!(AgentDirectRegion, event_slots) == 148_032);
const _: () = assert!(std::mem::offset_of!(AgentDirectRegion, telemetry) == 311_872);

impl AgentDirectRegion {
    pub fn new_boxed(
        session_nonce: SessionNonce,
        world_epoch: u64,
    ) -> Result<Box<Self>, BridgeError> {
        if !cfg!(target_endian = "little") {
            return Err(BridgeError::UnsupportedEndian);
        }
        let prefix = RegionPrefix {
            magic: BRIDGE_MAGIC,
            abi_version_le: BRIDGE_ABI_VERSION.to_le(),
            header_size_le: (EXPECTED_HEADER_SIZE as u16).to_le(),
            endian_tag_le: BRIDGE_ENDIAN_LITTLE.to_le(),
            total_size_le: (REGION_SIZE_BYTES as u64).to_le(),
            session_nonce: session_nonce.bytes(),
            reserved: [0; 24],
        };
        Ok(Box::new(Self {
            header: RegionHeader {
                prefix,
                world_epoch: CursorLine::new(world_epoch.to_le()),
                command_producer: CursorLine::new(0),
                command_consumer: CursorLine::new(0),
                event_producer: CursorLine::new(0),
                event_consumer: CursorLine::new(0),
                telemetry_generation: CursorLine::new(0),
                corruption: CorruptionLine::new(),
            },
            command_slots: std::array::from_fn(|_| UnsafeCell::new(CommandSlot::EMPTY)),
            event_slots: std::array::from_fn(|_| UnsafeCell::new(EventSlot::EMPTY)),
            telemetry: TelemetryArea::new(),
        }))
    }

    pub fn split(
        &mut self,
        expected_nonce: SessionNonce,
    ) -> Result<(AgentEndpoint<'_>, EngineEndpoint<'_>), BridgeError> {
        self.validate_header(expected_nonce)?;
        let shared = &*self;
        Ok((
            AgentEndpoint {
                region: shared,
                expected_nonce,
                not_sync: PhantomData,
            },
            EngineEndpoint {
                region: shared,
                expected_nonce,
                not_sync: PhantomData,
            },
        ))
    }

    fn validate_header(&self, expected_nonce: SessionNonce) -> Result<(), BridgeError> {
        let prefix = &self.header.prefix;
        let result = if prefix.magic != BRIDGE_MAGIC {
            Err(BridgeError::CorruptMagic)
        } else if u16::from_le(prefix.abi_version_le) != BRIDGE_ABI_VERSION {
            Err(BridgeError::UnsupportedVersion {
                found: u16::from_le(prefix.abi_version_le),
            })
        } else if u16::from_le(prefix.header_size_le) as usize != EXPECTED_HEADER_SIZE {
            Err(BridgeError::CorruptHeaderSize {
                found: u16::from_le(prefix.header_size_le) as usize,
            })
        } else if u32::from_le(prefix.endian_tag_le) != BRIDGE_ENDIAN_LITTLE {
            Err(BridgeError::UnsupportedEndian)
        } else if u64::from_le(prefix.total_size_le) != REGION_SIZE_BYTES as u64 {
            Err(BridgeError::CorruptTotalSize {
                found: u64::from_le(prefix.total_size_le),
            })
        } else if prefix.reserved.iter().any(|byte| *byte != 0) {
            Err(BridgeError::CorruptReservedPrefix)
        } else if prefix.session_nonce != expected_nonce.bytes() {
            Err(BridgeError::SessionMismatch)
        } else {
            Ok(())
        };
        if result.is_err() {
            increment_saturating(&self.header.corruption.header);
        }
        result
    }

    fn world_epoch(&self) -> u64 {
        u64::from_le(self.header.world_epoch.value.load(Ordering::Acquire))
    }

    fn push_command(&self, message: &CommandMessage) -> Result<(), BridgeError> {
        let producer = self.header.command_producer.value.load(Ordering::Relaxed);
        let consumer = self.header.command_consumer.value.load(Ordering::Acquire);
        let occupancy = producer.wrapping_sub(consumer);
        if occupancy > COMMAND_SLOT_COUNT as u64 {
            increment_saturating(&self.header.corruption.cursor);
            return Err(BridgeError::CorruptCursor);
        }
        if occupancy == COMMAND_SLOT_COUNT as u64 {
            increment_saturating(&self.header.corruption.queue_full);
            return Err(BridgeError::QueueFull);
        }
        let index = producer as usize & (COMMAND_SLOT_COUNT - 1);
        // SAFETY: the unique agent endpoint is the only producer.  Acquire of
        // consumer proves the consumer released this slot before reuse.
        unsafe {
            self.command_slots[index]
                .get()
                .write(CommandSlot::encode(message));
        }
        self.header
            .corruption
            .command_peak_occupancy
            .fetch_max(occupancy + 1, Ordering::Relaxed);
        self.header
            .command_producer
            .value
            .store(producer.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    fn pop_command(&self) -> Result<CommandMessage, BridgeError> {
        let consumer = self.header.command_consumer.value.load(Ordering::Relaxed);
        let producer = self.header.command_producer.value.load(Ordering::Acquire);
        let occupancy = producer.wrapping_sub(consumer);
        if occupancy > COMMAND_SLOT_COUNT as u64 {
            increment_saturating(&self.header.corruption.cursor);
            return Err(BridgeError::CorruptCursor);
        }
        if occupancy == 0 {
            return Err(BridgeError::QueueEmpty);
        }
        let index = consumer as usize & (COMMAND_SLOT_COUNT - 1);
        // SAFETY: acquire of producer proves initialization is visible.  The
        // unique engine endpoint is the only consumer, and the producer cannot
        // reuse this slot until the release store below.
        let slot = unsafe { self.command_slots[index].get().read() };
        self.header
            .command_consumer
            .value
            .store(consumer.wrapping_add(1), Ordering::Release);
        match slot.decode() {
            Ok(message) => Ok(message),
            Err(error @ BridgeError::CorruptLength { .. }) => {
                increment_saturating(&self.header.corruption.length);
                Err(error)
            }
            Err(error @ BridgeError::CorruptCommandClass { .. }) => {
                increment_saturating(&self.header.corruption.class);
                Err(error)
            }
            Err(error @ BridgeError::CorruptReservedField) => {
                increment_saturating(&self.header.corruption.reserved_field);
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn push_event(&self, message: &EventMessage) -> Result<(), BridgeError> {
        let producer = self.header.event_producer.value.load(Ordering::Relaxed);
        let consumer = self.header.event_consumer.value.load(Ordering::Acquire);
        let occupancy = producer.wrapping_sub(consumer);
        if occupancy > EVENT_SLOT_COUNT as u64 {
            increment_saturating(&self.header.corruption.cursor);
            return Err(BridgeError::CorruptCursor);
        }
        if occupancy == EVENT_SLOT_COUNT as u64 {
            increment_saturating(&self.header.corruption.queue_full);
            return Err(BridgeError::QueueFull);
        }
        let index = producer as usize & (EVENT_SLOT_COUNT - 1);
        // SAFETY: the unique engine endpoint is this ring's only producer.
        unsafe {
            self.event_slots[index]
                .get()
                .write(EventSlot::encode(message));
        }
        self.header
            .corruption
            .event_peak_occupancy
            .fetch_max(occupancy + 1, Ordering::Relaxed);
        self.header
            .event_producer
            .value
            .store(producer.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    fn pop_event(&self) -> Result<EventMessage, BridgeError> {
        let consumer = self.header.event_consumer.value.load(Ordering::Relaxed);
        let producer = self.header.event_producer.value.load(Ordering::Acquire);
        let occupancy = producer.wrapping_sub(consumer);
        if occupancy > EVENT_SLOT_COUNT as u64 {
            increment_saturating(&self.header.corruption.cursor);
            return Err(BridgeError::CorruptCursor);
        }
        if occupancy == 0 {
            return Err(BridgeError::QueueEmpty);
        }
        let index = consumer as usize & (EVENT_SLOT_COUNT - 1);
        // SAFETY: acquire of producer publishes this slot to the unique agent
        // consumer; release of consumer prevents premature producer reuse.
        let slot = unsafe { self.event_slots[index].get().read() };
        self.header
            .event_consumer
            .value
            .store(consumer.wrapping_add(1), Ordering::Release);
        match slot.decode() {
            Ok(message) => Ok(message),
            Err(error @ BridgeError::CorruptLength { .. }) => {
                increment_saturating(&self.header.corruption.length);
                Err(error)
            }
            Err(error @ BridgeError::CorruptReservedField) => {
                increment_saturating(&self.header.corruption.reserved_field);
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn publish_telemetry(
        &self,
        reflected_command_sequence: u64,
        payload: &[u8],
    ) -> Result<u64, BridgeError> {
        if payload.len() > TELEMETRY_CAPACITY {
            return Err(BridgeError::PayloadTooLarge {
                capacity: TELEMETRY_CAPACITY,
                actual: payload.len(),
            });
        }
        let generation = self
            .header
            .telemetry_generation
            .value
            .load(Ordering::Acquire);
        if generation & 1 != 0 {
            return Err(BridgeError::TelemetryWriterBusy);
        }
        let writing = generation.wrapping_add(1);
        self.header
            .telemetry_generation
            .value
            .compare_exchange(generation, writing, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| BridgeError::TelemetryWriterBusy)?;

        for (word_index, bytes) in payload.chunks(size_of::<u64>()).enumerate() {
            let mut packed = [0_u8; size_of::<u64>()];
            packed[..bytes.len()].copy_from_slice(bytes);
            self.telemetry.words[word_index].store(u64::from_le_bytes(packed), Ordering::Relaxed);
        }
        self.telemetry
            .world_epoch
            .store(self.world_epoch().to_le(), Ordering::Relaxed);
        self.telemetry
            .reflected_command_sequence
            .store(reflected_command_sequence.to_le(), Ordering::Relaxed);
        self.telemetry
            .payload_len
            .store((payload.len() as u32).to_le(), Ordering::Relaxed);
        self.telemetry.reserved.store(0, Ordering::Relaxed);

        // Generation zero means "never published".  At the theoretical u64
        // wrap boundary, publish generation two instead of briefly making a
        // valid snapshot indistinguishable from an empty region.
        let published = if writing == u64::MAX { 2 } else { writing + 1 };
        self.header
            .telemetry_generation
            .value
            .store(published, Ordering::Release);
        Ok(published)
    }

    fn read_telemetry(
        &self,
        destination: &mut [u8],
        max_retries: usize,
    ) -> Result<TelemetryRead, BridgeError> {
        let attempts = max_retries.max(1);
        for _ in 0..attempts {
            let before = self
                .header
                .telemetry_generation
                .value
                .load(Ordering::Acquire);
            if before == 0 {
                return Err(BridgeError::TelemetryUnavailable);
            }
            if before & 1 != 0 {
                increment_saturating(&self.header.corruption.telemetry_retries);
                std::hint::spin_loop();
                continue;
            }

            let payload_len =
                u32::from_le(self.telemetry.payload_len.load(Ordering::Relaxed)) as usize;
            if payload_len > TELEMETRY_CAPACITY {
                increment_saturating(&self.header.corruption.length);
                return Err(BridgeError::CorruptLength {
                    capacity: TELEMETRY_CAPACITY,
                    actual: payload_len,
                });
            }
            if destination.len() < payload_len {
                return Err(BridgeError::BufferTooSmall {
                    required: payload_len,
                    actual: destination.len(),
                });
            }
            if self.telemetry.reserved.load(Ordering::Relaxed) != 0 {
                increment_saturating(&self.header.corruption.reserved_field);
                return Err(BridgeError::CorruptReservedField);
            }
            let world_epoch = u64::from_le(self.telemetry.world_epoch.load(Ordering::Relaxed));
            let reflected_command_sequence = u64::from_le(
                self.telemetry
                    .reflected_command_sequence
                    .load(Ordering::Relaxed),
            );
            for (word_index, destination_bytes) in destination[..payload_len]
                .chunks_mut(size_of::<u64>())
                .enumerate()
            {
                let packed = self.telemetry.words[word_index]
                    .load(Ordering::Relaxed)
                    .to_le_bytes();
                destination_bytes.copy_from_slice(&packed[..destination_bytes.len()]);
            }

            // The acquire fence keeps every payload/meta load before the
            // validation load below.  That final acquire pairs with the
            // writer's release publication.  Atomic payload words prevent a
            // Rust data race while the generation comparison rejects any
            // mixed snapshot.
            fence(Ordering::Acquire);
            let after = self
                .header
                .telemetry_generation
                .value
                .load(Ordering::Acquire);
            if before == after && after & 1 == 0 {
                return Ok(TelemetryRead {
                    generation: after,
                    world_epoch,
                    reflected_command_sequence,
                    payload_len,
                });
            }
            increment_saturating(&self.header.corruption.telemetry_retries);
            std::hint::spin_loop();
        }
        Err(BridgeError::TelemetryBusy)
    }

    fn counters(&self) -> BridgeCounters {
        let corruption = &self.header.corruption;
        BridgeCounters {
            corrupt_header: corruption.header.load(Ordering::Relaxed),
            corrupt_length: corruption.length.load(Ordering::Relaxed),
            corrupt_cursor: corruption.cursor.load(Ordering::Relaxed),
            corrupt_class: corruption.class.load(Ordering::Relaxed),
            stale_epoch: corruption.stale_epoch.load(Ordering::Relaxed),
            duplicate_sequence: corruption.duplicate_sequence.load(Ordering::Relaxed),
            out_of_order_sequence: corruption.out_of_order_sequence.load(Ordering::Relaxed),
            expired: corruption.expired.load(Ordering::Relaxed),
            queue_full: corruption.queue_full.load(Ordering::Relaxed),
            telemetry_retries: corruption.telemetry_retries.load(Ordering::Relaxed),
            command_peak_occupancy: corruption.command_peak_occupancy.load(Ordering::Relaxed)
                as usize,
            event_peak_occupancy: corruption.event_peak_occupancy.load(Ordering::Relaxed) as usize,
            corrupt_reserved_field: corruption.reserved_field.load(Ordering::Relaxed),
        }
    }

    fn queue_snapshot(&self) -> Result<QueueSnapshot, BridgeError> {
        let command_producer = self.header.command_producer.value.load(Ordering::Acquire);
        let command_consumer = self.header.command_consumer.value.load(Ordering::Acquire);
        let event_producer = self.header.event_producer.value.load(Ordering::Acquire);
        let event_consumer = self.header.event_consumer.value.load(Ordering::Acquire);
        let command_occupancy = command_producer.wrapping_sub(command_consumer);
        let event_occupancy = event_producer.wrapping_sub(event_consumer);
        if command_occupancy > COMMAND_SLOT_COUNT as u64
            || event_occupancy > EVENT_SLOT_COUNT as u64
        {
            increment_saturating(&self.header.corruption.cursor);
            return Err(BridgeError::CorruptCursor);
        }
        Ok(QueueSnapshot {
            command_producer,
            command_consumer,
            command_occupancy: command_occupancy as usize,
            event_producer,
            event_consumer,
            event_occupancy: event_occupancy as usize,
            telemetry_generation: self
                .header
                .telemetry_generation
                .value
                .load(Ordering::Acquire),
        })
    }
}

pub struct AgentEndpoint<'region> {
    region: &'region AgentDirectRegion,
    expected_nonce: SessionNonce,
    // The endpoint may move to its producer thread (`Send`) but cannot be
    // shared by reference across threads (`!Sync`), preserving SPSC safety.
    not_sync: PhantomData<std::cell::Cell<()>>,
}

impl AgentEndpoint<'_> {
    pub fn try_enqueue_command(&self, message: &CommandMessage) -> Result<(), BridgeError> {
        self.region.validate_header(self.expected_nonce)?;
        self.region.push_command(message)
    }

    pub fn try_dequeue_event(&self) -> Result<EventMessage, BridgeError> {
        self.region.validate_header(self.expected_nonce)?;
        self.region.pop_event()
    }

    pub fn read_telemetry(
        &self,
        destination: &mut [u8],
        max_retries: usize,
    ) -> Result<TelemetryRead, BridgeError> {
        self.region.validate_header(self.expected_nonce)?;
        self.region.read_telemetry(destination, max_retries)
    }

    pub fn world_epoch(&self) -> Result<u64, BridgeError> {
        self.region.validate_header(self.expected_nonce)?;
        Ok(self.region.world_epoch())
    }

    pub fn counters(&self) -> BridgeCounters {
        self.region.counters()
    }

    pub fn queue_snapshot(&self) -> Result<QueueSnapshot, BridgeError> {
        self.region.validate_header(self.expected_nonce)?;
        self.region.queue_snapshot()
    }
}

pub struct EngineEndpoint<'region> {
    region: &'region AgentDirectRegion,
    expected_nonce: SessionNonce,
    // See `AgentEndpoint::not_sync`; this protects the unique consumer and the
    // reverse event-ring producer from concurrent safe calls.
    not_sync: PhantomData<std::cell::Cell<()>>,
}

impl EngineEndpoint<'_> {
    pub fn drain_commands<F>(
        &self,
        now_ns: u64,
        tracker: &mut CommandSequenceTracker,
        mut accept: F,
    ) -> Result<DrainReport, BridgeError>
    where
        F: FnMut(&CommandMessage),
    {
        self.region.validate_header(self.expected_nonce)?;
        let region_epoch = self.region.world_epoch();
        if tracker.world_epoch != region_epoch {
            return Err(BridgeError::TrackerEpochMismatch {
                tracker: tracker.world_epoch,
                region: region_epoch,
            });
        }
        let mut report = DrainReport::default();
        while report.removed < COMMAND_DRAIN_BUDGET {
            let message = match self.region.pop_command() {
                Ok(message) => message,
                Err(BridgeError::QueueEmpty) => break,
                Err(BridgeError::CorruptLength { .. }) => {
                    report.removed += 1;
                    report.corrupt_length += 1;
                    continue;
                }
                Err(BridgeError::CorruptCommandClass { .. }) => {
                    report.removed += 1;
                    report.corrupt_class += 1;
                    continue;
                }
                Err(BridgeError::CorruptReservedField) => {
                    report.removed += 1;
                    report.corrupt_reserved += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            report.removed += 1;

            if message.world_epoch != region_epoch {
                increment_saturating(&self.region.header.corruption.stale_epoch);
                report.stale_epoch += 1;
                continue;
            }

            match tracker.observe(message.sequence) {
                SequenceObservation::Duplicate => {
                    increment_saturating(&self.region.header.corruption.duplicate_sequence);
                    report.duplicate += 1;
                    if message.class.mutates_authoritative_state() {
                        report.duplicate_mutation += 1;
                    }
                    continue;
                }
                SequenceObservation::OutOfOrder => {
                    increment_saturating(&self.region.header.corruption.out_of_order_sequence);
                    report.out_of_order += 1;
                    continue;
                }
                SequenceObservation::New => {}
            }

            if message.deadline_ns < now_ns {
                increment_saturating(&self.region.header.corruption.expired);
                report.expired += 1;
                continue;
            }

            accept(&message);
            report.accepted += 1;
        }
        Ok(report)
    }

    pub fn try_enqueue_event(&self, message: &EventMessage) -> Result<(), BridgeError> {
        self.region.validate_header(self.expected_nonce)?;
        self.region.push_event(message)
    }

    pub fn publish_telemetry(
        &self,
        reflected_command_sequence: u64,
        payload: &[u8],
    ) -> Result<u64, BridgeError> {
        self.region.validate_header(self.expected_nonce)?;
        self.region
            .publish_telemetry(reflected_command_sequence, payload)
    }

    pub fn compare_exchange_world_epoch(
        &self,
        expected: u64,
        replacement: u64,
    ) -> Result<u64, BridgeError> {
        self.region.validate_header(self.expected_nonce)?;
        self.region
            .header
            .world_epoch
            .value
            .compare_exchange(
                expected.to_le(),
                replacement.to_le(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(u64::from_le)
            .map_err(|found| BridgeError::TrackerEpochMismatch {
                tracker: expected,
                region: u64::from_le(found),
            })
    }

    pub fn counters(&self) -> BridgeCounters {
        self.region.counters()
    }

    pub fn queue_snapshot(&self) -> Result<QueueSnapshot, BridgeError> {
        self.region.validate_header(self.expected_nonce)?;
        self.region.queue_snapshot()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
    pub removed: usize,
    pub accepted: usize,
    pub corrupt_length: usize,
    pub corrupt_class: usize,
    pub corrupt_reserved: usize,
    pub stale_epoch: usize,
    pub duplicate: usize,
    pub duplicate_mutation: usize,
    pub out_of_order: usize,
    pub expired: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TelemetryRead {
    pub generation: u64,
    pub world_epoch: u64,
    pub reflected_command_sequence: u64,
    pub payload_len: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BridgeCounters {
    pub corrupt_header: u64,
    pub corrupt_length: u64,
    pub corrupt_cursor: u64,
    pub corrupt_class: u64,
    pub stale_epoch: u64,
    pub duplicate_sequence: u64,
    pub out_of_order_sequence: u64,
    pub expired: u64,
    pub queue_full: u64,
    pub telemetry_retries: u64,
    pub command_peak_occupancy: usize,
    pub event_peak_occupancy: usize,
    pub corrupt_reserved_field: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub command_producer: u64,
    pub command_consumer: u64,
    pub command_occupancy: usize,
    pub event_producer: u64,
    pub event_consumer: u64,
    pub event_occupancy: usize,
    pub telemetry_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionPrefixDescriptor {
    pub abi_version: u16,
    pub header_size: usize,
    pub total_size: usize,
    pub session_nonce: [u8; 16],
}

/// Validates only the fixed 64-byte byte prefix before any caller attempts to
/// form a typed reference to a mapped region.  This does not validate mapping
/// alignment, cross-process atomics, ownership, or lifetime.
pub fn validate_region_prefix_bytes(bytes: &[u8]) -> Result<RegionPrefixDescriptor, BridgeError> {
    if bytes.len() < PREFIX_SIZE {
        return Err(BridgeError::TruncatedPrefix {
            required: PREFIX_SIZE,
            actual: bytes.len(),
        });
    }
    if bytes[0..8] != BRIDGE_MAGIC {
        return Err(BridgeError::CorruptMagic);
    }
    let version = u16::from_le_bytes([bytes[8], bytes[9]]);
    if version != BRIDGE_ABI_VERSION {
        return Err(BridgeError::UnsupportedVersion { found: version });
    }
    let header_size = u16::from_le_bytes([bytes[10], bytes[11]]) as usize;
    if header_size != EXPECTED_HEADER_SIZE {
        return Err(BridgeError::CorruptHeaderSize { found: header_size });
    }
    let endian_tag = u32::from_le_bytes(bytes[12..16].try_into().expect("fixed prefix slice"));
    if endian_tag != BRIDGE_ENDIAN_LITTLE {
        return Err(BridgeError::UnsupportedEndian);
    }
    let total_size_u64 = u64::from_le_bytes(bytes[16..24].try_into().expect("fixed prefix slice"));
    if total_size_u64 != REGION_SIZE_BYTES as u64 {
        return Err(BridgeError::CorruptTotalSize {
            found: total_size_u64,
        });
    }
    if bytes.len() < REGION_SIZE_BYTES {
        return Err(BridgeError::TruncatedRegion {
            required: REGION_SIZE_BYTES,
            actual: bytes.len(),
        });
    }
    if bytes[40..64].iter().any(|byte| *byte != 0) {
        return Err(BridgeError::CorruptReservedPrefix);
    }
    let session_nonce = bytes[24..40].try_into().expect("fixed nonce slice");
    if SessionNonce::new(session_nonce).is_none() {
        return Err(BridgeError::InvalidNonce);
    }
    Ok(RegionPrefixDescriptor {
        abi_version: version,
        header_size,
        total_size: REGION_SIZE_BYTES,
        session_nonce,
    })
}

pub fn validate_region_prefix_bytes_for_session(
    bytes: &[u8],
    expected_nonce: SessionNonce,
) -> Result<RegionPrefixDescriptor, BridgeError> {
    let descriptor = validate_region_prefix_bytes(bytes)?;
    if descriptor.session_nonce != expected_nonce.bytes() {
        return Err(BridgeError::SessionMismatch);
    }
    Ok(descriptor)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SequenceObservation {
    New,
    Duplicate,
    OutOfOrder,
}

/// Fixed 64-entry replay window.  Sequence comparison uses serial-number
/// arithmetic: a delta in `(0, 2^63)` is newer, including `u64` wrap.
pub struct CommandSequenceTracker {
    world_epoch: u64,
    has_last: bool,
    last: u64,
    recent: [u64; UNACKNOWLEDGED_MUTATION_WINDOW],
    recent_len: usize,
    recent_cursor: usize,
}

impl CommandSequenceTracker {
    pub const fn new(world_epoch: u64) -> Self {
        Self {
            world_epoch,
            has_last: false,
            last: 0,
            recent: [0; UNACKNOWLEDGED_MUTATION_WINDOW],
            recent_len: 0,
            recent_cursor: 0,
        }
    }

    pub fn reset_world_epoch(&mut self, world_epoch: u64) {
        *self = Self::new(world_epoch);
    }

    pub const fn world_epoch(&self) -> u64 {
        self.world_epoch
    }

    fn observe(&mut self, sequence: u64) -> SequenceObservation {
        if self.recent[..self.recent_len].contains(&sequence) {
            return SequenceObservation::Duplicate;
        }
        if self.has_last {
            let delta = sequence.wrapping_sub(self.last);
            if delta == 0 {
                return SequenceObservation::Duplicate;
            }
            if delta >= HALF_SEQUENCE_SPACE {
                return SequenceObservation::OutOfOrder;
            }
        }
        self.last = sequence;
        self.has_last = true;
        self.recent[self.recent_cursor] = sequence;
        self.recent_cursor = (self.recent_cursor + 1) % UNACKNOWLEDGED_MUTATION_WINDOW;
        self.recent_len = (self.recent_len + 1).min(UNACKNOWLEDGED_MUTATION_WINDOW);
        SequenceObservation::New
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageResult {
    Staged,
    Replaced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlushResult {
    Empty,
    Enqueued,
    StillFull,
}

/// Producer-local one-item staging cell.  It can replace only an explicitly
/// continuous command with the same opcode and epoch; published ring slots are
/// never overwritten while the consumer may be reading them.
pub struct ContinuousCommandStager {
    pending: Option<CommandMessage>,
}

impl ContinuousCommandStager {
    pub const fn new() -> Self {
        Self { pending: None }
    }

    pub fn stage(&mut self, command: CommandMessage) -> Result<StageResult, BridgeError> {
        if !command.class.permits_coalescing() {
            return Err(BridgeError::NonContinuousCoalescing);
        }
        let result = if let Some(previous) = self.pending {
            if previous.opcode != command.opcode || previous.world_epoch != command.world_epoch {
                return Err(BridgeError::CoalescingKeyMismatch);
            }
            let delta = command.sequence.wrapping_sub(previous.sequence);
            if delta == 0 || delta >= HALF_SEQUENCE_SPACE {
                return Err(BridgeError::CoalescingSequenceNotNewer);
            }
            StageResult::Replaced
        } else {
            StageResult::Staged
        };
        self.pending = Some(command);
        Ok(result)
    }

    pub fn flush(&mut self, endpoint: &AgentEndpoint<'_>) -> Result<FlushResult, BridgeError> {
        let Some(command) = self.pending.as_ref() else {
            return Ok(FlushResult::Empty);
        };
        match endpoint.try_enqueue_command(command) {
            Ok(()) => {
                self.pending = None;
                Ok(FlushResult::Enqueued)
            }
            Err(BridgeError::QueueFull) => Ok(FlushResult::StillFull),
            Err(error) => Err(error),
        }
    }

    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

impl Default for ContinuousCommandStager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Barrier;
    use std::thread;
    use std::time::Instant;

    const NONCE: SessionNonce = SessionNonce([0x5a; 16]);
    const EPOCH: u64 = 77;

    fn command(sequence: u64, class: CommandClass) -> CommandMessage {
        CommandMessage::new(sequence, EPOCH, u64::MAX, 9, class, &sequence.to_le_bytes()).unwrap()
    }

    #[test]
    fn abi_layout_prefix_and_hard_budgets_are_pinned() {
        assert_eq!(size_of::<RegionPrefix>(), 64);
        assert_eq!(size_of::<RegionHeader>(), 576);
        assert_eq!(size_of::<CommandSlot>(), 576);
        assert_eq!(size_of::<EventSlot>(), 320);
        assert_eq!(size_of::<TelemetryArea>(), 65_600);
        assert_eq!(REGION_SIZE_BYTES, 377_472);
        assert_eq!(REGION_ALIGNMENT_BYTES, 64);
        assert_eq!(COMMAND_SLOT_COUNT, 256);
        assert_eq!(EVENT_SLOT_COUNT, 512);
        assert_eq!(COMMAND_PAYLOAD_CAPACITY, 512);
        assert_eq!(TELEMETRY_CAPACITY, 65_536);
        assert_eq!(COMMAND_DRAIN_BUDGET, 32);

        let region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&*region as *const AgentDirectRegion).cast::<u8>(),
                REGION_SIZE_BYTES,
            )
        };
        let descriptor = validate_region_prefix_bytes(bytes).unwrap();
        assert_eq!(descriptor.abi_version, BRIDGE_ABI_VERSION);
        assert_eq!(descriptor.header_size, 576);
        assert_eq!(descriptor.total_size, REGION_SIZE_BYTES);
        assert_eq!(descriptor.session_nonce, NONCE.bytes());
    }

    #[test]
    fn byte_prefix_fails_closed_on_truncation_magic_version_size_and_nonce() {
        let region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        let region_bytes = unsafe {
            std::slice::from_raw_parts(
                (&*region as *const AgentDirectRegion).cast::<u8>(),
                REGION_SIZE_BYTES,
            )
        };
        let mut prefix = [0_u8; PREFIX_SIZE];
        prefix.copy_from_slice(&region_bytes[..PREFIX_SIZE]);
        assert!(matches!(
            validate_region_prefix_bytes(&prefix[..20]),
            Err(BridgeError::TruncatedPrefix { .. })
        ));
        assert!(matches!(
            validate_region_prefix_bytes(&prefix),
            Err(BridgeError::TruncatedRegion {
                required: REGION_SIZE_BYTES,
                actual: PREFIX_SIZE
            })
        ));

        let mut bytes = vec![0_u8; REGION_SIZE_BYTES];
        bytes[..PREFIX_SIZE].copy_from_slice(&prefix);
        bytes[0] ^= 0xff;
        assert_eq!(
            validate_region_prefix_bytes(&bytes),
            Err(BridgeError::CorruptMagic)
        );
        bytes[..PREFIX_SIZE].copy_from_slice(&prefix);
        bytes[8..10].copy_from_slice(&2_u16.to_le_bytes());
        assert!(matches!(
            validate_region_prefix_bytes(&bytes),
            Err(BridgeError::UnsupportedVersion { found: 2 })
        ));
        bytes[..PREFIX_SIZE].copy_from_slice(&prefix);
        bytes[16..24].copy_from_slice(&999_u64.to_le_bytes());
        assert!(matches!(
            validate_region_prefix_bytes(&bytes),
            Err(BridgeError::CorruptTotalSize { found: 999 })
        ));
        bytes[..PREFIX_SIZE].copy_from_slice(&prefix);
        bytes[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            validate_region_prefix_bytes(&bytes),
            Err(BridgeError::CorruptTotalSize { found: u64::MAX })
        ));
        bytes[..PREFIX_SIZE].copy_from_slice(&prefix);
        bytes[40] = 1;
        assert_eq!(
            validate_region_prefix_bytes(&bytes),
            Err(BridgeError::CorruptReservedPrefix)
        );
        bytes[..PREFIX_SIZE].copy_from_slice(&prefix);
        bytes[24..40].fill(0);
        assert_eq!(
            validate_region_prefix_bytes(&bytes),
            Err(BridgeError::InvalidNonce)
        );
        bytes[..PREFIX_SIZE].copy_from_slice(&prefix);
        let wrong = SessionNonce::new([0x33; 16]).unwrap();
        assert_eq!(
            validate_region_prefix_bytes_for_session(&bytes, wrong),
            Err(BridgeError::SessionMismatch)
        );
    }

    #[test]
    fn typed_header_rejects_wrong_nonce_and_corrupt_version() {
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        let wrong = SessionNonce::new([0x33; 16]).unwrap();
        assert!(matches!(
            region.split(wrong),
            Err(BridgeError::SessionMismatch)
        ));
        region.header.prefix.abi_version_le = 99_u16.to_le();
        assert!(matches!(
            region.split(NONCE),
            Err(BridgeError::UnsupportedVersion { found: 99 })
        ));
        assert_eq!(region.counters().corrupt_header, 2);
    }

    #[test]
    fn diagnostic_counters_saturate_instead_of_wrapping_to_healthy_zero() {
        let counter = AtomicU64::new(u64::MAX - 1);
        increment_saturating(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
        increment_saturating(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn command_and_event_rings_cover_empty_full_fifo_and_counter_wrap() {
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        region
            .header
            .command_producer
            .value
            .store(u64::MAX - 7, Ordering::Relaxed);
        region
            .header
            .command_consumer
            .value
            .store(u64::MAX - 7, Ordering::Relaxed);
        region
            .header
            .event_producer
            .value
            .store(u64::MAX - 31, Ordering::Relaxed);
        region
            .header
            .event_consumer
            .value
            .store(u64::MAX - 31, Ordering::Relaxed);
        let (agent, engine) = region.split(NONCE).unwrap();
        assert_eq!(engine.region.pop_command(), Err(BridgeError::QueueEmpty));
        for sequence in 0..COMMAND_SLOT_COUNT as u64 {
            agent
                .try_enqueue_command(&command(sequence, CommandClass::Query))
                .unwrap();
        }
        assert_eq!(
            agent.try_enqueue_command(&command(999, CommandClass::Query)),
            Err(BridgeError::QueueFull)
        );
        let mut tracker = CommandSequenceTracker::new(EPOCH);
        let mut accepted = 0;
        while accepted < COMMAND_SLOT_COUNT {
            let report = engine
                .drain_commands(0, &mut tracker, |message| {
                    assert_eq!(message.sequence, accepted as u64);
                    accepted += 1;
                })
                .unwrap();
            assert!(report.removed <= COMMAND_DRAIN_BUDGET);
        }
        assert_eq!(agent.queue_snapshot().unwrap().command_occupancy, 0);

        for sequence in 0..EVENT_SLOT_COUNT as u64 {
            engine
                .try_enqueue_event(&EventMessage::new(sequence, EPOCH, 2, 0, &[]).unwrap())
                .unwrap();
        }
        assert_eq!(
            engine.try_enqueue_event(&EventMessage::new(999, EPOCH, 2, 0, &[]).unwrap()),
            Err(BridgeError::QueueFull)
        );
        for sequence in 0..EVENT_SLOT_COUNT as u64 {
            assert_eq!(agent.try_dequeue_event().unwrap().sequence, sequence);
        }
        assert_eq!(agent.try_dequeue_event(), Err(BridgeError::QueueEmpty));
        let counters = agent.counters();
        assert_eq!(counters.command_peak_occupancy, COMMAND_SLOT_COUNT);
        assert_eq!(counters.event_peak_occupancy, EVENT_SLOT_COUNT);
        engine
            .region
            .header
            .command_producer
            .value
            .store(COMMAND_SLOT_COUNT as u64 + 1, Ordering::Relaxed);
        engine
            .region
            .header
            .command_consumer
            .value
            .store(0, Ordering::Relaxed);
        assert_eq!(agent.queue_snapshot(), Err(BridgeError::CorruptCursor));
        assert_eq!(agent.counters().corrupt_cursor, 1);
    }

    #[test]
    fn corrupt_slot_length_and_class_are_consumed_but_never_delivered() {
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        let (agent, engine) = region.split(NONCE).unwrap();
        agent
            .try_enqueue_command(&command(1, CommandClass::Mutation))
            .unwrap();
        // SAFETY: no consumer is active during this adversarial corruption,
        // and the slot is intentionally stored in UnsafeCell.
        unsafe {
            (*engine.region.command_slots[0].get()).payload_len_le = 513_u16.to_le();
        }
        let mut tracker = CommandSequenceTracker::new(EPOCH);
        let report = engine
            .drain_commands(0, &mut tracker, |_| panic!("corrupt command delivered"))
            .unwrap();
        assert_eq!(report.corrupt_length, 1);

        agent
            .try_enqueue_command(&command(2, CommandClass::Mutation))
            .unwrap();
        unsafe {
            (*engine.region.command_slots[1].get()).class = 255;
        }
        let report = engine
            .drain_commands(0, &mut tracker, |_| panic!("corrupt command delivered"))
            .unwrap();
        assert_eq!(report.corrupt_class, 1);
        agent
            .try_enqueue_command(&command(3, CommandClass::Mutation))
            .unwrap();
        unsafe {
            (*engine.region.command_slots[2].get()).reserved[0] = 1;
        }
        let report = engine
            .drain_commands(0, &mut tracker, |_| panic!("corrupt command delivered"))
            .unwrap();
        assert_eq!(report.corrupt_reserved, 1);
        let counters = engine.counters();
        assert_eq!(counters.corrupt_length, 1);
        assert_eq!(counters.corrupt_class, 1);
        assert_eq!(counters.corrupt_reserved_field, 1);
    }

    #[test]
    fn stale_epoch_duplicate_mutation_out_of_order_and_expiry_never_apply_twice() {
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        let (agent, engine) = region.split(NONCE).unwrap();
        let mut tracker = CommandSequenceTracker::new(EPOCH);
        let applied = AtomicU64::new(0);

        let stale = CommandMessage::new(1, EPOCH - 1, 500, 1, CommandClass::Mutation, &[]).unwrap();
        let valid = CommandMessage::new(10, EPOCH, 500, 1, CommandClass::Mutation, &[]).unwrap();
        let duplicate = valid;
        let older = CommandMessage::new(9, EPOCH, 500, 1, CommandClass::Mutation, &[]).unwrap();
        let expired = CommandMessage::new(11, EPOCH, 99, 1, CommandClass::Mutation, &[]).unwrap();
        for message in [stale, valid, duplicate, older, expired] {
            agent.try_enqueue_command(&message).unwrap();
        }
        let report = engine
            .drain_commands(100, &mut tracker, |_| {
                applied.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.stale_epoch, 1);
        assert_eq!(report.duplicate_mutation, 1);
        assert_eq!(report.out_of_order, 1);
        assert_eq!(report.expired, 1);
        assert_eq!(applied.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn serial_arithmetic_accepts_u64_wrap_but_rejects_old_values() {
        let mut tracker = CommandSequenceTracker::new(EPOCH);
        assert_eq!(tracker.observe(u64::MAX - 1), SequenceObservation::New);
        assert_eq!(tracker.observe(u64::MAX), SequenceObservation::New);
        assert_eq!(tracker.observe(0), SequenceObservation::New);
        assert_eq!(tracker.observe(1), SequenceObservation::New);
        assert_eq!(tracker.observe(u64::MAX), SequenceObservation::Duplicate);
        assert_eq!(
            tracker.observe(u64::MAX - 100),
            SequenceObservation::OutOfOrder
        );
    }

    #[test]
    fn exactly_thirty_two_commands_are_drained_per_call() {
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        let (agent, engine) = region.split(NONCE).unwrap();
        for sequence in 0..40 {
            agent
                .try_enqueue_command(&command(sequence, CommandClass::Query))
                .unwrap();
        }
        let mut tracker = CommandSequenceTracker::new(EPOCH);
        let first = engine.drain_commands(0, &mut tracker, |_| {}).unwrap();
        assert_eq!(first.removed, 32);
        assert_eq!(engine.queue_snapshot().unwrap().command_occupancy, 8);
        let second = engine.drain_commands(0, &mut tracker, |_| {}).unwrap();
        assert_eq!(second.removed, 8);
    }

    #[test]
    fn coalescing_is_producer_local_and_continuous_only() {
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        let (agent, engine) = region.split(NONCE).unwrap();
        let mut stager = ContinuousCommandStager::new();
        assert_eq!(
            stager.stage(command(1, CommandClass::Mutation)),
            Err(BridgeError::NonContinuousCoalescing)
        );
        assert_eq!(
            stager.stage(command(2, CommandClass::Continuous)),
            Ok(StageResult::Staged)
        );
        assert_eq!(
            stager.stage(command(3, CommandClass::Continuous)),
            Ok(StageResult::Replaced)
        );
        assert_eq!(
            stager.stage(command(2, CommandClass::Continuous)),
            Err(BridgeError::CoalescingSequenceNotNewer)
        );
        assert_eq!(stager.flush(&agent), Ok(FlushResult::Enqueued));
        let mut tracker = CommandSequenceTracker::new(EPOCH);
        let mut observed = 0;
        engine
            .drain_commands(0, &mut tracker, |message| observed = message.sequence)
            .unwrap();
        assert_eq!(observed, 3);
    }

    #[test]
    fn telemetry_odd_generation_is_never_accepted() {
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        region
            .header
            .telemetry_generation
            .value
            .store(1, Ordering::Release);
        let (agent, _) = region.split(NONCE).unwrap();
        let mut destination = [0_u8; 8];
        assert_eq!(
            agent.read_telemetry(&mut destination, 4),
            Err(BridgeError::TelemetryBusy)
        );
        assert_eq!(agent.counters().telemetry_retries, 4);
    }

    #[test]
    fn telemetry_generation_wrap_never_reuses_unavailable_zero() {
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        region
            .header
            .telemetry_generation
            .value
            .store(u64::MAX - 1, Ordering::Release);
        let (agent, engine) = region.split(NONCE).unwrap();
        assert_eq!(engine.publish_telemetry(7, b"wrap").unwrap(), 2);
        let mut destination = [0_u8; 8];
        let snapshot = agent.read_telemetry(&mut destination, 8).unwrap();
        assert_eq!(snapshot.generation, 2);
        assert_eq!(&destination[..snapshot.payload_len], b"wrap");
        engine.region.telemetry.reserved.store(1, Ordering::Relaxed);
        assert_eq!(
            agent.read_telemetry(&mut destination, 8),
            Err(BridgeError::CorruptReservedField)
        );
    }

    #[test]
    fn concurrent_telemetry_reader_never_accepts_a_torn_snapshot() {
        const UPDATES: u64 = 100_000;
        const PAYLOAD_BYTES: usize = 64;
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        let (agent, engine) = region.split(NONCE).unwrap();
        let finished = AtomicBool::new(false);
        let published = AtomicU64::new(0);
        let start = Barrier::new(2);
        thread::scope(|scope| {
            let writer_finished = &finished;
            let writer_published = &published;
            let writer_start = &start;
            let writer = scope.spawn(move || {
                writer_start.wait();
                for generation in 1..=UPDATES {
                    let marker = generation.to_le_bytes();
                    let mut payload = [0_u8; PAYLOAD_BYTES];
                    for chunk in payload.chunks_exact_mut(8) {
                        chunk.copy_from_slice(&marker);
                    }
                    engine.publish_telemetry(generation, &payload).unwrap();
                    writer_published.store(generation, Ordering::Release);
                    if generation % 31 == 0 {
                        thread::yield_now();
                    }
                }
                writer_finished.store(true, Ordering::Release);
            });
            let reader_finished = &finished;
            let reader_published = &published;
            let reader_start = &start;
            let reader = scope.spawn(move || {
                let mut reads = 0_u64;
                let mut overlap_reads = 0_u64;
                let mut payload = [0_u8; PAYLOAD_BYTES];
                reader_start.wait();
                while !reader_finished.load(Ordering::Acquire) || reads == 0 {
                    match agent.read_telemetry(&mut payload, 64) {
                        Ok(snapshot) => {
                            let marker = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                            assert_eq!(marker, snapshot.reflected_command_sequence);
                            for chunk in payload.chunks_exact(8) {
                                assert_eq!(u64::from_le_bytes(chunk.try_into().unwrap()), marker);
                            }
                            assert_eq!(snapshot.world_epoch, EPOCH);
                            if reader_published.load(Ordering::Acquire) < UPDATES {
                                overlap_reads += 1;
                            }
                            reads += 1;
                        }
                        Err(BridgeError::TelemetryUnavailable | BridgeError::TelemetryBusy) => {
                            thread::yield_now();
                        }
                        Err(error) => panic!("unexpected telemetry error: {error:?}"),
                    }
                }
                (reads, overlap_reads)
            });
            writer.join().unwrap();
            let (reads, overlap_reads) = reader.join().unwrap();
            assert!(reads > 0);
            assert!(overlap_reads > 0, "reader never overlapped the writer");
        });
        assert_eq!(
            region
                .header
                .telemetry_generation
                .value
                .load(Ordering::Acquire),
            UPDATES * 2
        );
    }

    #[test]
    fn one_hundred_thousand_spsc_updates_keep_storage_and_order_constant() {
        const UPDATES: u64 = 100_000;
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        let address_before = (&*region as *const AgentDirectRegion) as usize;
        let size_before = size_of_val(&*region);
        let (agent, engine) = region.split(NONCE).unwrap();
        let start = Barrier::new(2);
        thread::scope(|scope| {
            let producer_start = &start;
            let producer = scope.spawn(move || {
                producer_start.wait();
                for sequence in 0..UPDATES {
                    let message = command(sequence, CommandClass::Continuous);
                    loop {
                        match agent.try_enqueue_command(&message) {
                            Ok(()) => break,
                            Err(BridgeError::QueueFull) => thread::yield_now(),
                            Err(error) => panic!("unexpected producer error: {error:?}"),
                        }
                    }
                }
            });
            let consumer_start = &start;
            let consumer = scope.spawn(move || {
                let mut tracker = CommandSequenceTracker::new(EPOCH);
                let mut expected = 0_u64;
                consumer_start.wait();
                while expected < UPDATES {
                    engine
                        .drain_commands(0, &mut tracker, |message| {
                            assert_eq!(message.sequence, expected);
                            expected += 1;
                        })
                        .unwrap();
                    if expected < UPDATES {
                        thread::yield_now();
                    }
                }
                expected
            });
            producer.join().unwrap();
            assert_eq!(consumer.join().unwrap(), UPDATES);
        });
        assert_eq!(
            (&*region as *const AgentDirectRegion) as usize,
            address_before
        );
        assert_eq!(size_of_val(&*region), size_before);
        assert_eq!(size_before, REGION_SIZE_BYTES);
    }

    #[test]
    fn epoch_change_requires_explicit_tracker_reset_and_rejects_old_commands() {
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        let (agent, engine) = region.split(NONCE).unwrap();
        let mut tracker = CommandSequenceTracker::new(EPOCH);
        assert_eq!(
            engine.compare_exchange_world_epoch(EPOCH, EPOCH + 1),
            Ok(EPOCH)
        );
        assert!(matches!(
            engine.drain_commands(0, &mut tracker, |_| {}),
            Err(BridgeError::TrackerEpochMismatch { .. })
        ));
        tracker.reset_world_epoch(EPOCH + 1);
        agent
            .try_enqueue_command(&command(1, CommandClass::Mutation))
            .unwrap();
        let report = engine.drain_commands(0, &mut tracker, |_| {}).unwrap();
        assert_eq!(report.stale_epoch, 1);
        assert_eq!(report.accepted, 0);
    }

    #[test]
    #[ignore = "manual latency distribution microbenchmark"]
    fn benchmark_command_ingress_and_telemetry_p50_p95_p99() {
        const SAMPLES: usize = 20_000;
        const FULL_TELEMETRY_SAMPLES: usize = 2_000;
        let mut region = AgentDirectRegion::new_boxed(NONCE, EPOCH).unwrap();
        let (agent, engine) = region.split(NONCE).unwrap();
        let mut tracker = CommandSequenceTracker::new(EPOCH);
        let mut command_latencies = vec![0_u64; SAMPLES];
        let mut telemetry_latencies = vec![0_u64; SAMPLES];
        let mut full_telemetry_publish_latencies = vec![0_u64; FULL_TELEMETRY_SAMPLES];
        let mut telemetry_output = [0_u8; 64];
        let telemetry_payload = [0x5a_u8; 64];
        let full_telemetry_payload = vec![0xa5_u8; TELEMETRY_CAPACITY];

        for (index, latency) in command_latencies.iter_mut().enumerate() {
            let message = command(index as u64, CommandClass::Continuous);
            let start = Instant::now();
            agent.try_enqueue_command(black_box(&message)).unwrap();
            engine
                .drain_commands(0, &mut tracker, |value| {
                    black_box(value);
                })
                .unwrap();
            *latency = start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        }
        for (index, latency) in telemetry_latencies.iter_mut().enumerate() {
            let start = Instant::now();
            engine
                .publish_telemetry(index as u64, black_box(&telemetry_payload))
                .unwrap();
            black_box(agent.read_telemetry(&mut telemetry_output, 8).unwrap());
            *latency = start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        }
        for (index, latency) in full_telemetry_publish_latencies.iter_mut().enumerate() {
            let start = Instant::now();
            engine
                .publish_telemetry(index as u64, black_box(&full_telemetry_payload))
                .unwrap();
            *latency = start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        }
        command_latencies.sort_unstable();
        telemetry_latencies.sort_unstable();
        full_telemetry_publish_latencies.sort_unstable();
        let percentiles = |samples: &[u64]| {
            (
                samples[samples.len() * 50 / 100],
                samples[samples.len() * 95 / 100],
                samples[samples.len() * 99 / 100],
            )
        };
        let command = percentiles(&command_latencies);
        let telemetry = percentiles(&telemetry_latencies);
        let full_telemetry_publish = percentiles(&full_telemetry_publish_latencies);
        eprintln!(
            "direct bridge profile={} command enqueue+drain ns p50/p95/p99={}/{}/{}; telemetry publish+read 64B ns p50/p95/p99={}/{}/{}; telemetry publish 64KiB ns p50/p95/p99={}/{}/{}",
            if cfg!(debug_assertions) { "debug" } else { "optimized" },
            command.0,
            command.1,
            command.2,
            telemetry.0,
            telemetry.1,
            telemetry.2,
            full_telemetry_publish.0,
            full_telemetry_publish.1,
            full_telemetry_publish.2,
        );
        // The contract's latency gates apply to the runtime build.  Debug
        // numbers are still printed as a useful instrumentation baseline, but
        // per-word checked atomics are intentionally much slower there.
        if !cfg!(debug_assertions) {
            assert!(
                command.2 < 250_000,
                "command p99 target exceeded: {command:?}"
            );
            assert!(
                telemetry.2 < 200_000,
                "telemetry p99 target exceeded: {telemetry:?}"
            );
            assert!(
                full_telemetry_publish.2 < 200_000,
                "64KiB telemetry publication p99 target exceeded: {full_telemetry_publish:?}"
            );
        }
    }
}
