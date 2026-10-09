use crate::{ChannelId, DeviceId, SimTime};
use std::error::Error;
use std::fmt;

/// An invalid network configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    /// A device or channel identifier was empty.
    EmptyId,
    /// More than one device used an identifier.
    DuplicateDevice(DeviceId),
    /// More than one channel used an identifier.
    DuplicateChannel(ChannelId),
    /// A referenced device does not exist.
    UnknownDevice(DeviceId),
    /// A referenced channel does not exist.
    UnknownChannel(ChannelId),
    /// Both ends of a channel reference the same device.
    SelfConnectedChannel(ChannelId),
    /// A channel's nominal rate is zero.
    ZeroBitRate(ChannelId),
    /// A degraded rate is zero or greater than the nominal rate.
    InvalidDegradedRate(ChannelId),
    /// A device has non-finite coordinates or an invalid waypoint sequence.
    InvalidMobility(DeviceId),
    /// A device has an invalid receiver-interference entry.
    InvalidReceiverInterference(DeviceId),
    /// A distance-aware channel has invalid physical or rate parameters.
    InvalidDistanceModel(ChannelId),
    /// A channel has an invalid radio band or interference response.
    InvalidRadio(ChannelId),
    /// A channel override has invalid queue, MTU, loss, medium, or weight values.
    InvalidSimulatorOptions(String),
    /// A device selected a channel to which it is not connected.
    ChannelNotConnected {
        /// The device selecting the channel.
        device: DeviceId,
        /// The disconnected channel.
        channel: ChannelId,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId => write!(formatter, "device and channel identifiers cannot be empty"),
            Self::DuplicateDevice(id) => write!(formatter, "duplicate device identifier `{id}`"),
            Self::DuplicateChannel(id) => write!(formatter, "duplicate channel identifier `{id}`"),
            Self::UnknownDevice(id) => write!(formatter, "unknown device `{id}`"),
            Self::UnknownChannel(id) => write!(formatter, "unknown channel `{id}`"),
            Self::SelfConnectedChannel(id) => {
                write!(
                    formatter,
                    "channel `{id}` must connect two different devices"
                )
            }
            Self::ZeroBitRate(id) => write!(formatter, "channel `{id}` has a zero bit rate"),
            Self::InvalidDegradedRate(id) => {
                write!(formatter, "channel `{id}` has an invalid degraded bit rate")
            }
            Self::InvalidMobility(id) => write!(formatter, "device `{id}` has invalid mobility"),
            Self::InvalidReceiverInterference(id) => {
                write!(formatter, "device `{id}` has invalid receiver interference")
            }
            Self::InvalidDistanceModel(id) => {
                write!(formatter, "channel `{id}` has an invalid distance model")
            }
            Self::InvalidRadio(id) => {
                write!(formatter, "channel `{id}` has an invalid radio model")
            }
            Self::InvalidSimulatorOptions(id) => write!(
                formatter,
                "channel `{id}` has invalid packet-engine options"
            ),
            Self::ChannelNotConnected { device, channel } => write!(
                formatter,
                "channel `{channel}` is not connected to device `{device}`"
            ),
        }
    }
}

impl Error for ConfigError {}

/// An error loading a serialized network configuration.
#[derive(Debug)]
pub enum ConfigLoadError {
    /// The document could not be parsed.
    Parse {
        /// The input format being parsed.
        format: &'static str,
        /// The parser's diagnostic.
        message: String,
    },
    /// The parsed topology was not valid.
    Invalid(ConfigError),
}

impl fmt::Display for ConfigLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse { format, message } => {
                write!(
                    formatter,
                    "could not parse {format} network configuration: {message}"
                )
            }
            Self::Invalid(error) => write!(formatter, "invalid network configuration: {error}"),
        }
    }
}

impl Error for ConfigLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse { .. } => None,
            Self::Invalid(error) => Some(error),
        }
    }
}

impl From<ConfigError> for ConfigLoadError {
    fn from(value: ConfigError) -> Self {
        Self::Invalid(value)
    }
}

/// An operation that could not be performed by the simulator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SimulationError {
    /// The supplied topology is invalid.
    InvalidConfig(ConfigError),
    /// An operation referenced an unknown device.
    UnknownDevice(DeviceId),
    /// An operation referenced an unknown channel.
    UnknownChannel(ChannelId),
    /// Only source devices may originate packets.
    NotASource(DeviceId),
    /// An event was requested before the current virtual time.
    TimeInPast,
    /// The requested time precedes the explicitly compacted history boundary.
    HistoryUnavailable {
        /// Earliest time still available for historical queries.
        retained_from: SimTime,
    },
    /// A degraded channel state supplied an invalid rate.
    InvalidChannelState(ChannelId),
    /// A runtime receiver-interference snapshot was invalid.
    InvalidReceiverInterference(DeviceId),
    /// A simulated time calculation exceeded the supported range.
    TimeOverflow,
    /// No more packet identifiers are available.
    PacketIdOverflow,
}

impl fmt::Display for SimulationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(error) => error.fmt(formatter),
            Self::UnknownDevice(id) => write!(formatter, "unknown device `{id}`"),
            Self::UnknownChannel(id) => write!(formatter, "unknown channel `{id}`"),
            Self::NotASource(id) => write!(formatter, "device `{id}` is not a source"),
            Self::HistoryUnavailable { retained_from } => write!(
                formatter,
                "history before {} ns has been compacted",
                retained_from.as_nanos()
            ),
            Self::TimeInPast => write!(formatter, "cannot schedule an event in the past"),
            Self::InvalidChannelState(id) => {
                write!(formatter, "invalid state for channel `{id}`")
            }
            Self::InvalidReceiverInterference(id) => {
                write!(formatter, "invalid receiver interference for device `{id}`")
            }
            Self::TimeOverflow => write!(formatter, "simulated time overflowed"),
            Self::PacketIdOverflow => write!(formatter, "packet identifier space exhausted"),
        }
    }
}

impl Error for SimulationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidConfig(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ConfigError> for SimulationError {
    fn from(value: ConfigError) -> Self {
        Self::InvalidConfig(value)
    }
}
