use super::*;
use serde::{Deserialize, Serialize};

/// Versioned checkpoint of a simulator, including queued events, reception
/// history, runtime endpoints and scheduler counters. Restore only trusted
/// application-owned snapshots and pin the application/dependency schema.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimulatorSnapshot {
    schema_version: u32,
    configuration: NetworkConfig,
    used_devices: std::collections::BTreeSet<DeviceId>,
    used_channels: std::collections::BTreeSet<ChannelId>,
    devices: BTreeMap<DeviceId, DeviceRuntime>,
    channels: BTreeMap<ChannelId, ChannelRuntime>,
    #[serde(with = "entries")]
    queue: BTreeMap<(u64, u64), InternalEvent>,
    now: SimTime,
    history_boundary: SimTime,
    next_sequence: u64,
    next_packet_id: u64,
    options: SimulatorOptions,
    #[serde(with = "entries")]
    queue_wakes: BTreeMap<(ChannelId, usize), u64>,
    shared_medium_available: BTreeMap<String, u64>,
}

impl Simulator {
    /// Capture the complete current simulator state without advancing time.
    pub fn snapshot(&self) -> SimulatorSnapshot {
        SimulatorSnapshot {
            schema_version: 1,
            configuration: self.configuration.clone(),
            used_devices: self.used_devices.clone(),
            used_channels: self.used_channels.clone(),
            devices: self.devices.clone(),
            channels: self.channels.clone(),
            queue: self.queue.clone(),
            now: self.now,
            history_boundary: self.history_boundary,
            next_sequence: self.next_sequence,
            next_packet_id: self.next_packet_id,
            options: self.options.clone(),
            queue_wakes: self.queue_wakes.clone(),
            shared_medium_available: self.shared_medium_available.clone(),
        }
    }

    /// Restore a trusted checkpoint. Incompatible versions and invalid topology
    /// or clock boundaries are rejected rather than silently reset.
    pub fn restore(snapshot: SimulatorSnapshot) -> Result<Self, String> {
        if snapshot.schema_version != 1 {
            return Err("incompatible c3mesh snapshot version".into());
        }
        snapshot
            .configuration
            .validate()
            .map_err(|e| e.to_string())?;
        snapshot
            .options
            .validate(&snapshot.configuration)
            .map_err(|e| e.to_string())?;
        if snapshot.history_boundary > snapshot.now
            || snapshot.queue.keys().any(|(at, sequence)| {
                *at < snapshot.now.as_nanos() || *sequence >= snapshot.next_sequence
            })
            || snapshot.channels.values().any(|channel| {
                channel
                    .endpoints
                    .iter()
                    .any(|id| !snapshot.devices.contains_key(id))
            })
            || snapshot
                .devices
                .keys()
                .any(|id| !snapshot.used_devices.contains(id))
            || snapshot
                .channels
                .keys()
                .any(|id| !snapshot.used_channels.contains(id))
            || snapshot
                .queue_wakes
                .keys()
                .any(|(id, direction)| *direction > 1 || !snapshot.channels.contains_key(id))
        {
            return Err("invalid c3mesh snapshot boundaries or topology".into());
        }
        Ok(Self {
            configuration: snapshot.configuration,
            used_devices: snapshot.used_devices,
            used_channels: snapshot.used_channels,
            devices: snapshot.devices,
            channels: snapshot.channels,
            queue: snapshot.queue,
            now: snapshot.now,
            history_boundary: snapshot.history_boundary,
            next_sequence: snapshot.next_sequence,
            next_packet_id: snapshot.next_packet_id,
            options: snapshot.options,
            queue_wakes: snapshot.queue_wakes,
            shared_medium_available: snapshot.shared_medium_available,
        })
    }
}

pub(crate) mod entries {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;
    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(
        map: &BTreeMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        map.iter().collect::<Vec<_>>().serialize(serializer)
    }
    pub fn deserialize<
        'de,
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    >(
        deserializer: D,
    ) -> Result<BTreeMap<K, V>, D::Error> {
        let entries = Vec::<(K, V)>::deserialize(deserializer)?;
        let count = entries.len();
        let map: BTreeMap<_, _> = entries.into_iter().collect();
        if count != map.len() {
            return Err(serde::de::Error::custom("duplicate snapshot map key"));
        }
        Ok(map)
    }
}
