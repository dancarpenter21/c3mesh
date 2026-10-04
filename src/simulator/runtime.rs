//! Runtime topology changes preserve the scheduler and unrelated traffic.
use super::*;
use crate::{ConfigError, DeviceConfig};
use std::collections::BTreeSet;

impl Simulator {
    /// Atomically registers additional endpoints and channels at the current time.
    /// Identifiers cannot be reused, even after retirement.
    pub fn register_topology(
        &mut self,
        additions: NetworkConfig,
        options: SimulatorOptions,
    ) -> Result<(), SimulationError> {
        for device in &additions.devices {
            if self.used_devices.contains(&device.id) {
                return Err(ConfigError::DuplicateDevice(device.id.clone()).into());
            }
        }
        for channel in &additions.channels {
            if self.used_channels.contains(&channel.id) {
                return Err(ConfigError::DuplicateChannel(channel.id.clone()).into());
            }
        }
        let mut config = self.configuration.clone();
        config.devices.extend(additions.devices.clone());
        config.channels.extend(additions.channels.clone());
        config.validate()?;
        options.validate(&additions)?;
        for device in additions.devices {
            self.used_devices.insert(device.id.clone());
            self.devices.insert(
                device.id,
                DeviceRuntime {
                    kind: device.kind,
                    mobility_history: BTreeMap::from([(
                        self.now.as_nanos(),
                        device.mobility.clone(),
                    )]),
                    mobility: device.mobility,
                    interference: BTreeMap::from([(self.now.as_nanos(), device.interference)]),
                },
            );
        }
        for channel in additions.channels {
            self.used_channels.insert(channel.id.clone());
            let id = channel.id.clone();
            let mut runtime = ChannelRuntime::from(channel);
            runtime.packet_engine = options.channels.contains_key(&id);
            self.channels.insert(id, runtime);
        }
        self.options.channels.extend(options.channels);
        self.configuration = config;
        Ok(())
    }

    /// Replaces a device trajectory from the current virtual time onward.
    /// Callers advance to the change time before applying it; past events are immutable.
    pub fn set_device_mobility(
        &mut self,
        id: impl Into<DeviceId>,
        mobility: MobilityModel,
    ) -> Result<(), SimulationError> {
        let id = id.into();
        if !self.devices.contains_key(&id) {
            return Err(SimulationError::UnknownDevice(id));
        }
        NetworkConfig {
            devices: vec![DeviceConfig {
                id: id.clone(),
                kind: DeviceKind::Sink,
                mobility: mobility.clone(),
                interference: vec![],
            }],
            channels: vec![],
        }
        .validate()?;
        self.devices
            .get_mut(&id)
            .unwrap()
            .mobility_history
            .insert(self.now.as_nanos(), mobility.clone());
        self.devices.get_mut(&id).unwrap().mobility = mobility.clone();
        self.configuration
            .devices
            .iter_mut()
            .find(|d| d.id == id)
            .unwrap()
            .mobility = mobility;
        Ok(())
    }

    /// Time of the next internal event, useful for synchronizing an external simulation.
    #[must_use]
    pub fn next_event_time(&self) -> Option<SimTime> {
        self.queue
            .first_key_value()
            .map(|((at, _), _)| SimTime::from_nanos(*at))
    }

    /// Retires endpoints and incident channels, including sources whose egress disappears.
    /// Cancels affected queued/in-flight traffic with one explicit drop per packet.
    pub fn retire_devices(&mut self, ids: &[DeviceId]) -> Result<(), SimulationError> {
        for id in ids {
            if !self.devices.contains_key(id) {
                return Err(SimulationError::UnknownDevice(id.clone()));
            }
        }
        let mut devices: BTreeSet<_> = ids.iter().cloned().collect();
        let mut channels = BTreeSet::new();
        loop {
            let before = (devices.len(), channels.len());
            for (id, channel) in &self.channels {
                if channel.endpoints.iter().any(|d| devices.contains(d)) {
                    channels.insert(id.clone());
                }
            }
            for (id, device) in &self.devices {
                if matches!(&device.kind, DeviceKind::Source { egress } if channels.contains(egress))
                {
                    devices.insert(id.clone());
                }
            }
            if before == (devices.len(), channels.len()) {
                break;
            }
        }
        let mut cancelled: BTreeMap<PacketId, Packet> = BTreeMap::new();
        for id in &channels {
            if let Some(channel) = self.channels.remove(id) {
                for queue in channel.waiting {
                    for entry in queue.packets {
                        cancelled.insert(entry.packet.id(), entry.packet);
                    }
                }
            }
            self.options.channels.remove(id);
        }
        for channel in self.channels.values_mut() {
            for queue in &mut channel.waiting {
                let mut index = 0;
                while index < queue.packets.len() {
                    let packet = &queue.packets[index].packet;
                    if devices.contains(packet.source()) || devices.contains(packet.destination()) {
                        let entry = queue.remove(index);
                        cancelled.insert(entry.packet.id(), entry.packet);
                    } else {
                        index += 1;
                    }
                }
            }
        }
        self.queue.retain(|_, event| {
            let (packet, affected) = match event {
                InternalEvent::Inject(p) => (Some(&*p), false),
                InternalEvent::Receive {
                    packet, channel, ..
                } => (Some(&*packet), channels.contains(channel)),
                InternalEvent::LegacyStart { event, channel, .. } => {
                    let packet = match event {
                        NetworkEvent::TransmissionStarted { packet, .. }
                        | NetworkEvent::PacketDelivered { packet, .. }
                        | NetworkEvent::PacketDropped { packet, .. }
                        | NetworkEvent::DataReceived { packet, .. } => packet,
                    };
                    (Some(&*packet), channels.contains(channel))
                }
                InternalEvent::Emit(e) => {
                    let p = match e {
                        NetworkEvent::TransmissionStarted { packet, .. }
                        | NetworkEvent::PacketDelivered { packet, .. }
                        | NetworkEvent::PacketDropped { packet, .. }
                        | NetworkEvent::DataReceived { packet, .. } => packet,
                    };
                    (Some(&*p), false)
                }
                InternalEvent::Drain { channel, .. } => return !channels.contains(channel),
            };
            if let Some(p) = packet {
                if affected || devices.contains(p.source()) || devices.contains(p.destination()) {
                    cancelled.insert(p.id(), p.clone());
                    return false;
                }
            }
            true
        });
        self.queue.retain(|_, event| {
            let packet = match event {
                InternalEvent::Inject(packet) | InternalEvent::Receive { packet, .. } => packet,
                InternalEvent::Emit(event) | InternalEvent::LegacyStart { event, .. } => {
                    match event {
                        NetworkEvent::TransmissionStarted { packet, .. }
                        | NetworkEvent::PacketDelivered { packet, .. }
                        | NetworkEvent::PacketDropped { packet, .. }
                        | NetworkEvent::DataReceived { packet, .. } => packet,
                    }
                }
                InternalEvent::Drain { .. } => return true,
            };
            !cancelled.contains_key(&packet.id())
        });
        self.queue_wakes.retain(|(id, _), _| !channels.contains(id));
        self.devices.retain(|id, _| !devices.contains(id));
        self.configuration
            .devices
            .retain(|d| !devices.contains(&d.id));
        self.configuration
            .channels
            .retain(|c| !channels.contains(&c.id));
        for device in &mut self.configuration.devices {
            match &mut device.kind {
                DeviceKind::Switch { forwarding } => {
                    forwarding.retain(|d, c| !devices.contains(d) && !channels.contains(c))
                }
                DeviceKind::Router {
                    routes,
                    default_route,
                } => {
                    routes.retain(|d, c| !devices.contains(d) && !channels.contains(c));
                    if default_route.as_ref().is_some_and(|c| channels.contains(c)) {
                        *default_route = None;
                    }
                }
                _ => {}
            }
            self.devices.get_mut(&device.id).unwrap().kind = device.kind.clone();
        }
        for packet in cancelled.into_values() {
            self.drop_now(
                packet.clone(),
                packet.destination().clone(),
                DropReason::EndpointRetired,
            )?;
        }
        Ok(())
    }
}
