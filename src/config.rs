#[cfg(any(feature = "json", feature = "yaml"))]
use crate::ConfigLoadError;
use crate::{ChannelId, ConfigError, DeviceId, MobilityModel};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A serializable description of an entire network topology.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct NetworkConfig {
    /// Devices participating in the network.
    pub devices: Vec<DeviceConfig>,
    /// Channels connecting pairs of devices.
    pub channels: Vec<ChannelConfig>,
}

impl NetworkConfig {
    /// Validates all identifiers, endpoints, rates, and forwarding references.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut device_ids = BTreeSet::new();
        for device in &self.devices {
            if device.id.as_str().is_empty() {
                return Err(ConfigError::EmptyId);
            }
            if !device_ids.insert(device.id.clone()) {
                return Err(ConfigError::DuplicateDevice(device.id.clone()));
            }
            validate_mobility(device)?;
            validate_receiver_interference(device)?;
        }

        let mut channels = BTreeMap::new();
        for channel in &self.channels {
            if channel.id.as_str().is_empty() {
                return Err(ConfigError::EmptyId);
            }
            if channels.insert(channel.id.clone(), channel).is_some() {
                return Err(ConfigError::DuplicateChannel(channel.id.clone()));
            }
            if channel.endpoints[0] == channel.endpoints[1] {
                return Err(ConfigError::SelfConnectedChannel(channel.id.clone()));
            }
            for endpoint in &channel.endpoints {
                if !device_ids.contains(endpoint) {
                    return Err(ConfigError::UnknownDevice(endpoint.clone()));
                }
            }
            if channel.bit_rate_bps == 0 {
                return Err(ConfigError::ZeroBitRate(channel.id.clone()));
            }
            if let ChannelState::Degraded {
                effective_bit_rate_bps,
            } = channel.state
            {
                if effective_bit_rate_bps == 0 || effective_bit_rate_bps > channel.bit_rate_bps {
                    return Err(ConfigError::InvalidDegradedRate(channel.id.clone()));
                }
            }
            if let Some(distance) = &channel.distance {
                validate_distance_model(channel, distance)?;
            }
            if let Some(radio) = &channel.radio {
                validate_radio(channel, radio)?;
            }
        }

        for device in &self.devices {
            match &device.kind {
                DeviceKind::Source { egress } => {
                    validate_egress(&device.id, egress, &channels)?;
                }
                DeviceKind::Sink => {}
                DeviceKind::Switch { forwarding } => {
                    for (destination, channel) in forwarding {
                        if !device_ids.contains(destination) {
                            return Err(ConfigError::UnknownDevice(destination.clone()));
                        }
                        validate_egress(&device.id, channel, &channels)?;
                    }
                }
                DeviceKind::Router {
                    routes,
                    default_route,
                } => {
                    for (destination, channel) in routes {
                        if !device_ids.contains(destination) {
                            return Err(ConfigError::UnknownDevice(destination.clone()));
                        }
                        validate_egress(&device.id, channel, &channels)?;
                    }
                    if let Some(channel) = default_route {
                        validate_egress(&device.id, channel, &channels)?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Parses and validates a JSON topology.
    #[cfg(feature = "json")]
    pub fn from_json_str(input: &str) -> Result<Self, ConfigLoadError> {
        let config: Self = serde_json::from_str(input).map_err(|error| ConfigLoadError::Parse {
            format: "JSON",
            message: error.to_string(),
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Parses and validates a YAML topology.
    #[cfg(feature = "yaml")]
    pub fn from_yaml_str(input: &str) -> Result<Self, ConfigLoadError> {
        let config: Self = yaml_serde::from_str(input).map_err(|error| ConfigLoadError::Parse {
            format: "YAML",
            message: error.to_string(),
        })?;
        config.validate()?;
        Ok(config)
    }
}

fn validate_egress(
    device: &DeviceId,
    channel_id: &ChannelId,
    channels: &BTreeMap<ChannelId, &ChannelConfig>,
) -> Result<(), ConfigError> {
    let channel = channels
        .get(channel_id)
        .ok_or_else(|| ConfigError::UnknownChannel(channel_id.clone()))?;
    if !channel.endpoints.contains(device) {
        return Err(ConfigError::ChannelNotConnected {
            device: device.clone(),
            channel: channel_id.clone(),
        });
    }
    Ok(())
}

/// A serializable device definition.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DeviceConfig {
    /// The unique device identifier.
    pub id: DeviceId,
    /// The device's fundamental network role.
    #[serde(flatten)]
    pub kind: DeviceKind,
    /// The device's position and movement over virtual time.
    #[serde(default)]
    pub mobility: MobilityModel,
    /// Initial receiver-side interference conditions at simulation time zero.
    #[serde(default)]
    pub interference: Vec<ReceiverInterference>,
}

/// The fundamental behavior assigned to a device.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeviceKind {
    /// A packet origin with one attached egress channel.
    Source {
        /// Channel used for all originated packets.
        egress: ChannelId,
    },
    /// A terminal that accepts packets addressed to itself.
    Sink,
    /// A forwarding device with exact destination rules.
    Switch {
        /// Mapping from final destination to egress channel.
        #[serde(default)]
        forwarding: BTreeMap<DeviceId, ChannelId>,
    },
    /// A forwarding device with exact routes and an optional default route.
    Router {
        /// Mapping from final destination to egress channel.
        #[serde(default)]
        routes: BTreeMap<DeviceId, ChannelId>,
        /// Egress used when no exact route exists.
        #[serde(default)]
        default_route: Option<ChannelId>,
    },
}

/// A serializable point-to-point channel definition.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ChannelConfig {
    /// The unique channel identifier.
    pub id: ChannelId,
    /// The two devices connected by the channel.
    pub endpoints: [DeviceId; 2],
    /// The nominal transmission rate in bits per second.
    pub bit_rate_bps: u64,
    /// Fixed one-way delay in nanoseconds.
    ///
    /// Distance-derived propagation delay is added to this value.
    #[serde(default)]
    pub propagation_delay_ns: u64,
    /// The channel's initial availability and effective rate.
    #[serde(default)]
    pub state: ChannelState,
    /// Optional distance-derived propagation, range, and rate behavior.
    #[serde(default)]
    pub distance: Option<DistanceChannel>,
    /// Optional spectrum and interference response for a wireless channel.
    #[serde(default)]
    pub radio: Option<RadioChannel>,
}

/// A half-open radio-frequency interval, `[lower_hz, upper_hz)`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FrequencyBand {
    /// Inclusive lower frequency in hertz.
    pub lower_hz: u64,
    /// Exclusive upper frequency in hertz.
    pub upper_hz: u64,
}

impl FrequencyBand {
    /// Creates a half-open frequency band.
    #[must_use]
    pub const fn new(lower_hz: u64, upper_hz: u64) -> Self {
        Self { lower_hz, upper_hz }
    }

    pub(crate) const fn is_valid(self) -> bool {
        self.lower_hz < self.upper_hz
    }
}

/// Receiver-side interference over a frequency band.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReceiverInterference {
    /// Frequencies occupied by this interference contribution.
    pub band: FrequencyBand,
    /// Normalized severity from `0.0` (none) to `1.0` (maximum).
    pub jammed: f64,
}

/// Wireless behavior attached to a point-to-point channel.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct RadioChannel {
    /// Frequencies occupied by transmissions on this channel.
    pub band: FrequencyBand,
    /// Mapping from aggregate receiver interference to supported bitrate.
    #[serde(default)]
    pub interference_response: InterferenceResponse,
}

/// Linear mapping from normalized interference to radio-link health.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct InterferenceResponse {
    /// Aggregate magnitude at or below which the full base rate is supported.
    pub unaffected_below: f64,
    /// Aggregate magnitude at or above which the link is severed.
    pub severed_at: f64,
}

impl Default for InterferenceResponse {
    fn default() -> Self {
        Self {
            unaffected_below: 0.0,
            severed_at: 1.0,
        }
    }
}

/// The operating state of a channel.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelState {
    /// The channel uses its nominal bit rate.
    #[default]
    Operational,
    /// The channel uses a lower effective bit rate.
    Degraded {
        /// The reduced rate in bits per second.
        effective_bit_rate_bps: u64,
    },
    /// The channel refuses new transmissions.
    Severed,
}

/// Physical behavior for a channel connecting positioned devices.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DistanceChannel {
    /// Signal propagation speed in meters per second.
    ///
    /// For radio in vacuum, use approximately `299_792_458.0`.
    pub propagation_speed_mps: f64,
    /// Maximum endpoint separation at which new transmissions can begin.
    pub max_range_m: f64,
    /// How serialization rate changes as distance increases.
    #[serde(default)]
    pub rate_model: DistanceRateModel,
}

/// A deterministic distance-to-bitrate model.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DistanceRateModel {
    /// Keep the channel's configured rate until it moves out of range.
    #[default]
    Constant,
    /// Linearly reduce bitrate between a full-rate distance and maximum range.
    Linear {
        /// Distance through which the nominal rate remains available.
        full_rate_distance_m: f64,
        /// Bitrate available at maximum range.
        minimum_bit_rate_bps: u64,
    },
}

fn validate_mobility(device: &DeviceConfig) -> Result<(), ConfigError> {
    let valid = match &device.mobility {
        MobilityModel::Static { position } => position.is_finite(),
        MobilityModel::Linear {
            position_at_epoch,
            velocity_mps,
            ..
        } => position_at_epoch.is_finite() && velocity_mps.is_finite(),
        MobilityModel::Waypoints { waypoints } => {
            !waypoints.is_empty()
                && waypoints
                    .iter()
                    .all(|waypoint| waypoint.position.is_finite())
                && waypoints
                    .windows(2)
                    .all(|pair| pair[0].at_ns < pair[1].at_ns)
        }
    };
    if valid {
        Ok(())
    } else {
        Err(ConfigError::InvalidMobility(device.id.clone()))
    }
}

fn validate_receiver_interference(device: &DeviceConfig) -> Result<(), ConfigError> {
    if device.interference.iter().all(|interference| {
        interference.band.is_valid()
            && interference.jammed.is_finite()
            && (0.0..=1.0).contains(&interference.jammed)
    }) {
        Ok(())
    } else {
        Err(ConfigError::InvalidReceiverInterference(device.id.clone()))
    }
}

fn validate_distance_model(
    channel: &ChannelConfig,
    distance: &DistanceChannel,
) -> Result<(), ConfigError> {
    let base_valid = distance.propagation_speed_mps.is_finite()
        && distance.propagation_speed_mps > 0.0
        && distance.max_range_m.is_finite()
        && distance.max_range_m > 0.0;
    let rate_valid = match distance.rate_model {
        DistanceRateModel::Constant => true,
        DistanceRateModel::Linear {
            full_rate_distance_m,
            minimum_bit_rate_bps,
        } => {
            full_rate_distance_m.is_finite()
                && full_rate_distance_m >= 0.0
                && full_rate_distance_m < distance.max_range_m
                && minimum_bit_rate_bps > 0
                && minimum_bit_rate_bps <= channel.bit_rate_bps
        }
    };
    if base_valid && rate_valid {
        Ok(())
    } else {
        Err(ConfigError::InvalidDistanceModel(channel.id.clone()))
    }
}

fn validate_radio(channel: &ChannelConfig, radio: &RadioChannel) -> Result<(), ConfigError> {
    let response = radio.interference_response;
    let valid = radio.band.is_valid()
        && response.unaffected_below.is_finite()
        && response.severed_at.is_finite()
        && response.unaffected_below >= 0.0
        && response.unaffected_below < response.severed_at
        && response.severed_at <= 1.0;
    if valid {
        Ok(())
    } else {
        Err(ConfigError::InvalidRadio(channel.id.clone()))
    }
}
