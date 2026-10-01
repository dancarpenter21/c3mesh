#![doc = include_str!("../README.md")]

mod config;
mod error;
mod mobility;
mod model;
mod options;
mod packet_queue;
mod simulator;

pub use config::{
    ChannelConfig, ChannelState, DeviceConfig, DeviceKind, DistanceChannel, DistanceRateModel,
    FrequencyBand, InterferenceResponse, NetworkConfig, RadioChannel, ReceiverInterference,
};
pub use error::{ConfigError, ConfigLoadError, SimulationError};
pub use mobility::{MobilityModel, Position3D, Velocity3D, Waypoint};
pub use model::{
    ChannelId, ChannelMetrics, DeviceId, DropReason, NetworkEvent, Packet, PacketId, SimTime,
    TransmissionMetrics,
};
pub use options::{
    ChannelOptions, ChannelQueueMetrics, PacketMetadata, QueueConfig, QueueDiscipline,
    SimulatorOptions,
};
pub use simulator::Simulator;
