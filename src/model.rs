use crate::{FrequencyBand, PacketMetadata};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A stable identifier for a device in a topology.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct DeviceId(String);

impl DeviceId {
    /// Creates a device identifier.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for DeviceId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for DeviceId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A stable identifier for a channel in a topology.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ChannelId(String);

impl ChannelId {
    /// Creates a channel identifier.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ChannelId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ChannelId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Virtual simulation time measured in nanoseconds from the start of a run.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SimTime(u64);

impl SimTime {
    /// The start of simulation time.
    pub const ZERO: Self = Self(0);

    /// Creates a time value from nanoseconds.
    #[must_use]
    pub const fn from_nanos(nanoseconds: u64) -> Self {
        Self(nanoseconds)
    }

    /// Returns this time in nanoseconds.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SimTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ns", self.0)
    }
}

/// A simulator-assigned packet identifier.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct PacketId(u64);

impl PacketId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric packet identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for PacketId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A unit of data moving through the simulated network.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Packet {
    id: PacketId,
    source: DeviceId,
    destination: DeviceId,
    payload: Vec<u8>,
    hops_remaining: u16,
    #[serde(default, skip_serializing_if = "PacketMetadata::is_default")]
    metadata: PacketMetadata,
}

impl Packet {
    pub(crate) fn new(
        id: PacketId,
        source: DeviceId,
        destination: DeviceId,
        payload: Vec<u8>,
        hops_remaining: u16,
        metadata: PacketMetadata,
    ) -> Self {
        Self {
            id,
            source,
            destination,
            payload,
            hops_remaining,
            metadata,
        }
    }

    /// Returns the simulator-assigned identifier.
    #[must_use]
    pub const fn id(&self) -> PacketId {
        self.id
    }

    /// Returns the originating device.
    #[must_use]
    pub const fn source(&self) -> &DeviceId {
        &self.source
    }

    /// Returns the addressed destination device.
    #[must_use]
    pub const fn destination(&self) -> &DeviceId {
        &self.destination
    }

    /// Returns the packet payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Returns the remaining forwarding-hop budget.
    #[must_use]
    pub const fn hops_remaining(&self) -> u16 {
        self.hops_remaining
    }

    /// Returns endpoint-supplied scheduling metadata.
    #[must_use]
    pub const fn metadata(&self) -> &PacketMetadata {
        &self.metadata
    }

    pub(crate) fn expired_at(&self, at: SimTime) -> bool {
        self.metadata
            .expires_at
            .is_some_and(|expires| at >= expires)
    }

    pub(crate) fn forwarded(&self) -> Option<Self> {
        self.hops_remaining.checked_sub(1).map(|remaining| {
            let mut packet = self.clone();
            packet.hops_remaining = remaining;
            packet
        })
    }
}

/// The reason a packet could not continue through the network.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DropReason {
    /// The selected channel was severed.
    ChannelSevered {
        /// The unavailable channel.
        channel: ChannelId,
    },
    /// Moving endpoints were beyond a distance-aware channel's range.
    OutOfRange {
        /// The channel whose endpoints could not communicate.
        channel: ChannelId,
    },
    /// Receiver-side interference prevented successful reception.
    ReceiverInterference {
        /// The affected radio channel.
        channel: ChannelId,
    },
    /// A bounded waiting queue could not admit the packet.
    QueueOverflow {
        /// The saturated channel.
        channel: ChannelId,
    },
    /// The complete wire packet exceeded a channel's MTU.
    MtuExceeded {
        /// The constrained channel.
        channel: ChannelId,
        /// The configured maximum wire size.
        mtu_bytes: usize,
        /// The attempted wire size.
        wire_bytes: usize,
    },
    /// A seeded channel-loss decision discarded the packet after serialization.
    ChannelLoss {
        /// The lossy channel.
        channel: ChannelId,
    },
    /// The packet reached its exclusive virtual-time deadline.
    Expired,
    /// A forwarding device had no matching rule.
    NoForwardingRule,
    /// The packet reached a sink other than its destination.
    WrongDestination,
    /// The receiving device role cannot forward packets.
    UnsupportedDeviceRole,
    /// The packet exhausted its forwarding-hop budget.
    HopLimitExceeded,
}

/// An observable event produced by the simulator.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetworkEvent {
    /// A channel began serializing a packet.
    TransmissionStarted {
        /// The transmission start time.
        at: SimTime,
        /// The packet being transmitted.
        packet: Packet,
        /// The channel carrying the packet.
        channel: ChannelId,
        /// The transmitting device.
        from: DeviceId,
        /// The next device on the channel.
        to: DeviceId,
        /// The time at which the complete packet will arrive.
        receive_at: SimTime,
        /// Endpoint separation rounded to millimeters for a distance-aware link.
        distance_mm: Option<u64>,
        /// Effective serialization rate selected for this transmission.
        effective_bit_rate_bps: u64,
        /// Frequencies occupied by a wireless transmission.
        frequency_band: Option<FrequencyBand>,
    },
    /// The complete packet arrived at the next device.
    DataReceived {
        /// The packet arrival time.
        at: SimTime,
        /// The received packet.
        packet: Packet,
        /// The channel on which the packet arrived.
        channel: ChannelId,
        /// The transmitting device.
        from: DeviceId,
        /// The receiving device.
        device: DeviceId,
    },
    /// A destination sink accepted a packet.
    PacketDelivered {
        /// The delivery time.
        at: SimTime,
        /// The delivered packet.
        packet: Packet,
        /// The destination sink.
        sink: DeviceId,
    },
    /// A device or channel dropped a packet.
    PacketDropped {
        /// The drop time.
        at: SimTime,
        /// The dropped packet.
        packet: Packet,
        /// The device attempting the operation.
        device: DeviceId,
        /// Why the packet was dropped.
        reason: DropReason,
    },
}

/// Distance-derived channel conditions at a specific virtual time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelMetrics {
    /// Time at which the metrics were evaluated.
    pub at: SimTime,
    /// Endpoint distance for a distance-aware channel, in meters.
    pub distance_m: Option<f64>,
    /// Total one-way propagation delay in nanoseconds.
    pub propagation_delay_ns: u64,
    /// Effective bit rate, or `None` when the channel is unavailable.
    pub effective_bit_rate_bps: Option<u64>,
    /// Whether the channel can accept a transmission at this time.
    pub available: bool,
}

/// Directional link conditions for a prospective transmission.
#[derive(Clone, Debug, PartialEq)]
pub struct TransmissionMetrics {
    /// Time at which the metrics were evaluated.
    pub at: SimTime,
    /// Device that would receive the transmission.
    pub receiver: DeviceId,
    /// Endpoint distance for a distance-aware channel, in meters.
    pub distance_m: Option<f64>,
    /// Total one-way propagation delay in nanoseconds.
    pub propagation_delay_ns: u64,
    /// Frequencies used by the channel, or `None` for a bandless channel.
    pub frequency_band: Option<FrequencyBand>,
    /// Aggregate normalized receiver interference after spectral overlap.
    pub jammed: f64,
    /// Effective rate before receiver interference is applied.
    pub base_bit_rate_bps: Option<u64>,
    /// Effective rate including receiver interference, or `None` if unavailable.
    pub effective_bit_rate_bps: Option<u64>,
    /// Whether this direction can begin a transmission at this time.
    pub available: bool,
}

impl NetworkEvent {
    /// Returns the virtual time at which the event occurred.
    #[must_use]
    pub const fn time(&self) -> SimTime {
        match self {
            Self::TransmissionStarted { at, .. }
            | Self::DataReceived { at, .. }
            | Self::PacketDelivered { at, .. }
            | Self::PacketDropped { at, .. } => *at,
        }
    }
}
