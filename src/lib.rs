#![doc = include_str!("../README.md")]

mod config;
mod error;
mod mobility;
mod model;
mod simulator;

pub use config::{
    ChannelConfig, ChannelState, DeviceConfig, DeviceKind, DistanceChannel, DistanceRateModel,
    NetworkConfig,
};
pub use error::{ConfigError, ConfigLoadError, SimulationError};
pub use mobility::{MobilityModel, Position3D, Velocity3D, Waypoint};
pub use model::{
    ChannelId, ChannelMetrics, DeviceId, DropReason, NetworkEvent, Packet, PacketId, SimTime,
};
pub use simulator::Simulator;
