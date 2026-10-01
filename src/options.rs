use crate::{ChannelId, ConfigError, NetworkConfig, SimTime};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Selection policy for packets waiting on one channel direction.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueDiscipline {
    /// Serve packets in admission order.
    #[default]
    Fifo,
    /// Serve larger priority values first, preserving FIFO within a priority.
    StrictPriority,
    /// Serve flows by integer virtual finish time, weighted by traffic class.
    WeightedFair,
}

/// Bounds and selection policy for a waiting queue; in-flight packets are excluded.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueueConfig {
    /// Maximum waiting packets per direction, or no bound.
    pub max_packets: Option<usize>,
    /// Maximum waiting wire bytes per direction, or no bound.
    pub max_bytes: Option<usize>,
    /// Policy used when a serializer becomes available.
    pub discipline: QueueDiscipline,
}

/// Optional packet-engine behavior on a channel.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelOptions {
    /// Maximum packet wire size including modeled overhead, or no bound.
    pub mtu_bytes: Option<usize>,
    /// Waiting-queue capacity and scheduling policy.
    pub queue: QueueConfig,
    /// Channels with the same nonempty ID share a single serialization resource.
    pub shared_medium: Option<String>,
    /// Bytes added to payload size for serialization, MTU, and queue accounting.
    pub wire_overhead_bytes: usize,
    /// Deterministic loss probability in basis points, from zero through 10,000.
    pub loss_basis_points: u16,
    /// Positive weights by traffic class; unspecified classes have weight one.
    pub traffic_class_weights: BTreeMap<u8, u16>,
}

/// Runtime options separate from the backward-compatible topology schema.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimulatorOptions {
    /// Seed for deterministic channel-loss decisions.
    pub seed: u64,
    /// Channel overrides; absent channels retain their existing scheduling behavior.
    pub channels: BTreeMap<ChannelId, ChannelOptions>,
}

impl SimulatorOptions {
    pub(crate) fn validate(&self, config: &NetworkConfig) -> Result<(), ConfigError> {
        let ids: BTreeSet<_> = config.channels.iter().map(|channel| &channel.id).collect();
        for (id, options) in &self.channels {
            if !ids.contains(id) {
                return Err(ConfigError::UnknownChannel(id.clone()));
            }
            if options.mtu_bytes == Some(0)
                || options.queue.max_packets == Some(0)
                || options.queue.max_bytes == Some(0)
                || options.loss_basis_points > 10_000
                || options
                    .shared_medium
                    .as_ref()
                    .is_some_and(|name| name.trim().is_empty())
                || options
                    .traffic_class_weights
                    .values()
                    .any(|weight| *weight == 0)
            {
                return Err(ConfigError::InvalidSimulatorOptions(id.to_string()));
            }
        }
        Ok(())
    }
}

/// Generic endpoint-supplied metadata; routers never inspect the payload.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PacketMetadata {
    /// Larger values take precedence under strict-priority scheduling.
    pub priority: u8,
    /// Traffic class used to look up a weighted-fair service weight.
    pub traffic_class: u8,
    /// Endpoint-assigned flow identity, scoped by source and traffic class.
    pub flow_id: u64,
    /// Exclusive virtual-time deadline for delivery, or no expiry.
    pub expires_at: Option<SimTime>,
}

impl PacketMetadata {
    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// Instantaneous waiting-queue occupancy, excluding packets already on the wire.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChannelQueueMetrics {
    /// Waiting packets from endpoint zero to endpoint one.
    pub packets_0_to_1: usize,
    /// Waiting packets from endpoint one to endpoint zero.
    pub packets_1_to_0: usize,
    /// Waiting wire bytes from endpoint zero to endpoint one.
    pub bytes_0_to_1: usize,
    /// Waiting wire bytes from endpoint one to endpoint zero.
    pub bytes_1_to_0: usize,
}
