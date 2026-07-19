use crate::{
    ChannelConfig, ChannelId, ChannelMetrics, ChannelState, DeviceId, DeviceKind, DistanceChannel,
    DistanceRateModel, DropReason, MobilityModel, NetworkConfig, NetworkEvent, Packet, PacketId,
    Position3D, SimTime, SimulationError,
};
use std::collections::BTreeMap;

const DEFAULT_HOP_LIMIT: u16 = 64;
const NANOS_PER_SECOND: u128 = 1_000_000_000;

#[derive(Clone, Debug)]
struct ChannelRuntime {
    endpoints: [DeviceId; 2],
    nominal_bit_rate_bps: u64,
    propagation_delay_ns: u64,
    state: ChannelState,
    distance: Option<DistanceChannel>,
    direction_available_ns: [u64; 2],
}

impl From<ChannelConfig> for ChannelRuntime {
    fn from(value: ChannelConfig) -> Self {
        Self {
            endpoints: value.endpoints,
            nominal_bit_rate_bps: value.bit_rate_bps,
            propagation_delay_ns: value.propagation_delay_ns,
            state: value.state,
            distance: value.distance,
            direction_available_ns: [0, 0],
        }
    }
}

#[derive(Clone, Debug)]
struct DeviceRuntime {
    kind: DeviceKind,
    mobility: MobilityModel,
}

#[derive(Clone, Debug)]
enum InternalEvent {
    Inject(Packet),
    Emit(NetworkEvent),
    Receive {
        packet: Packet,
        channel: ChannelId,
        from: DeviceId,
        to: DeviceId,
    },
}

/// A deterministic, discrete-event simulation of a validated network topology.
#[derive(Clone, Debug)]
pub struct Simulator {
    devices: BTreeMap<DeviceId, DeviceRuntime>,
    channels: BTreeMap<ChannelId, ChannelRuntime>,
    queue: BTreeMap<(u64, u64), InternalEvent>,
    now: SimTime,
    next_sequence: u64,
    next_packet_id: u64,
}

impl Simulator {
    /// Creates a simulator after validating the supplied topology.
    pub fn new(config: NetworkConfig) -> Result<Self, SimulationError> {
        config.validate()?;
        let devices = config
            .devices
            .into_iter()
            .map(|device| {
                (
                    device.id,
                    DeviceRuntime {
                        kind: device.kind,
                        mobility: device.mobility,
                    },
                )
            })
            .collect();
        let channels = config
            .channels
            .into_iter()
            .map(|channel| (channel.id.clone(), channel.into()))
            .collect();
        Ok(Self {
            devices,
            channels,
            queue: BTreeMap::new(),
            now: SimTime::ZERO,
            next_sequence: 0,
            next_packet_id: 0,
        })
    }

    /// Returns the simulator's current virtual time.
    #[must_use]
    pub const fn now(&self) -> SimTime {
        self.now
    }

    /// Returns a device's configured or calculated position at a virtual time.
    pub fn device_position_at(
        &self,
        device: impl Into<DeviceId>,
        at: SimTime,
    ) -> Result<Position3D, SimulationError> {
        let device = device.into();
        let position = self
            .devices
            .get(&device)
            .map(|runtime| runtime.mobility.position_at(at))
            .ok_or(SimulationError::UnknownDevice(device))?;
        if position.is_finite() {
            Ok(position)
        } else {
            Err(SimulationError::TimeOverflow)
        }
    }

    /// Evaluates channel distance, propagation, rate, and availability at a time.
    pub fn channel_metrics_at(
        &self,
        channel: impl Into<ChannelId>,
        at: SimTime,
    ) -> Result<ChannelMetrics, SimulationError> {
        let channel_id = channel.into();
        let runtime = self
            .channels
            .get(&channel_id)
            .ok_or_else(|| SimulationError::UnknownChannel(channel_id.clone()))?;
        let state_rate = match runtime.state {
            ChannelState::Operational => Some(runtime.nominal_bit_rate_bps),
            ChannelState::Degraded {
                effective_bit_rate_bps,
            } => Some(effective_bit_rate_bps),
            ChannelState::Severed => None,
        };

        let (distance_m, distance_delay_ns, distance_rate, in_range) =
            if let Some(distance_config) = &runtime.distance {
                let left = self.device_position_at(runtime.endpoints[0].clone(), at)?;
                let right = self.device_position_at(runtime.endpoints[1].clone(), at)?;
                let distance_m = left.distance_to(right);
                if !distance_m.is_finite() {
                    return Err(SimulationError::TimeOverflow);
                }
                let delay_ns =
                    propagation_delay_ns(distance_m, distance_config.propagation_speed_mps)?;
                let in_range = distance_m <= distance_config.max_range_m;
                let rate =
                    distance_rate_bps(runtime.nominal_bit_rate_bps, distance_config, distance_m);
                (Some(distance_m), delay_ns, rate, in_range)
            } else {
                (None, 0, runtime.nominal_bit_rate_bps, true)
            };

        let propagation_delay_ns = runtime
            .propagation_delay_ns
            .checked_add(distance_delay_ns)
            .ok_or(SimulationError::TimeOverflow)?;
        let effective_bit_rate_bps = state_rate
            .filter(|_| in_range)
            .map(|rate| rate.min(distance_rate));
        Ok(ChannelMetrics {
            at,
            distance_m,
            propagation_delay_ns,
            effective_bit_rate_bps,
            available: effective_bit_rate_bps.is_some(),
        })
    }

    /// Originates a packet at the current virtual time with a default hop limit.
    pub fn send(
        &mut self,
        source: impl Into<DeviceId>,
        destination: impl Into<DeviceId>,
        payload: impl Into<Vec<u8>>,
    ) -> Result<PacketId, SimulationError> {
        self.schedule_send(self.now, source, destination, payload)
    }

    /// Schedules a packet at a specific virtual time with a default hop limit.
    pub fn schedule_send(
        &mut self,
        at: SimTime,
        source: impl Into<DeviceId>,
        destination: impl Into<DeviceId>,
        payload: impl Into<Vec<u8>>,
    ) -> Result<PacketId, SimulationError> {
        self.schedule_send_with_hop_limit(at, source, destination, payload, DEFAULT_HOP_LIMIT)
    }

    /// Schedules a packet with an explicit forwarding-hop limit.
    pub fn schedule_send_with_hop_limit(
        &mut self,
        at: SimTime,
        source: impl Into<DeviceId>,
        destination: impl Into<DeviceId>,
        payload: impl Into<Vec<u8>>,
        hop_limit: u16,
    ) -> Result<PacketId, SimulationError> {
        if at < self.now {
            return Err(SimulationError::TimeInPast);
        }
        let source = source.into();
        let destination = destination.into();
        match self.devices.get(&source).map(|device| &device.kind) {
            Some(DeviceKind::Source { .. }) => {}
            Some(_) => return Err(SimulationError::NotASource(source)),
            None => return Err(SimulationError::UnknownDevice(source)),
        }
        if !self.devices.contains_key(&destination) {
            return Err(SimulationError::UnknownDevice(destination));
        }

        let id = PacketId::new(self.next_packet_id);
        self.next_packet_id = self
            .next_packet_id
            .checked_add(1)
            .ok_or(SimulationError::PacketIdOverflow)?;
        let packet = Packet::new(id, source, destination, payload.into(), hop_limit);
        self.enqueue(at.as_nanos(), InternalEvent::Inject(packet))?;
        Ok(id)
    }

    /// Changes a channel's state for transmissions requested from this point onward.
    pub fn set_channel_state(
        &mut self,
        channel: impl Into<ChannelId>,
        state: ChannelState,
    ) -> Result<(), SimulationError> {
        let channel_id = channel.into();
        let runtime = self
            .channels
            .get_mut(&channel_id)
            .ok_or_else(|| SimulationError::UnknownChannel(channel_id.clone()))?;
        if let ChannelState::Degraded {
            effective_bit_rate_bps,
        } = state
        {
            if effective_bit_rate_bps == 0 || effective_bit_rate_bps > runtime.nominal_bit_rate_bps
            {
                return Err(SimulationError::InvalidChannelState(channel_id));
            }
        }
        runtime.state = state;
        Ok(())
    }

    /// Advances until the next observable event, or returns `None` when idle.
    pub fn step(&mut self) -> Result<Option<NetworkEvent>, SimulationError> {
        loop {
            let Some(((time_ns, _), internal)) = self.queue.pop_first() else {
                return Ok(None);
            };
            self.now = SimTime::from_nanos(time_ns);
            match internal {
                InternalEvent::Emit(event) => return Ok(Some(event)),
                InternalEvent::Inject(packet) => {
                    let source = packet.source().clone();
                    let egress = match self.devices.get(&source).map(|device| &device.kind) {
                        Some(DeviceKind::Source { egress }) => egress.clone(),
                        _ => return Err(SimulationError::NotASource(source)),
                    };
                    self.request_transmission(packet, source, egress)?;
                }
                InternalEvent::Receive {
                    packet,
                    channel,
                    from,
                    to,
                } => {
                    self.handle_receive(&packet, &to)?;
                    return Ok(Some(NetworkEvent::DataReceived {
                        at: self.now,
                        packet,
                        channel,
                        from,
                        device: to,
                    }));
                }
            }
        }
    }

    /// Runs until idle and returns every observable event in order.
    pub fn run(&mut self) -> Result<Vec<NetworkEvent>, SimulationError> {
        let mut events = Vec::new();
        while let Some(event) = self.step()? {
            events.push(event);
        }
        Ok(events)
    }

    /// Runs until idle and invokes a callback for every observable event.
    pub fn run_with<F>(&mut self, mut callback: F) -> Result<(), SimulationError>
    where
        F: FnMut(&NetworkEvent),
    {
        while let Some(event) = self.step()? {
            callback(&event);
        }
        Ok(())
    }

    fn handle_receive(
        &mut self,
        packet: &Packet,
        device: &DeviceId,
    ) -> Result<(), SimulationError> {
        let kind = self
            .devices
            .get(device)
            .map(|runtime| runtime.kind.clone())
            .ok_or_else(|| SimulationError::UnknownDevice(device.clone()))?;
        match kind {
            DeviceKind::Sink if packet.destination() == device => {
                self.emit_now(NetworkEvent::PacketDelivered {
                    at: self.now,
                    packet: packet.clone(),
                    sink: device.clone(),
                })?;
            }
            DeviceKind::Sink => {
                self.drop_now(packet.clone(), device.clone(), DropReason::WrongDestination)?;
            }
            DeviceKind::Source { .. } => {
                self.drop_now(
                    packet.clone(),
                    device.clone(),
                    DropReason::UnsupportedDeviceRole,
                )?;
            }
            DeviceKind::Switch { forwarding } => {
                if let Some(channel) = forwarding.get(packet.destination()) {
                    self.forward(packet, device.clone(), channel.clone())?;
                } else {
                    self.drop_now(packet.clone(), device.clone(), DropReason::NoForwardingRule)?;
                }
            }
            DeviceKind::Router {
                routes,
                default_route,
            } => {
                if let Some(channel) = routes.get(packet.destination()).cloned().or(default_route) {
                    self.forward(packet, device.clone(), channel)?;
                } else {
                    self.drop_now(packet.clone(), device.clone(), DropReason::NoForwardingRule)?;
                }
            }
        }
        Ok(())
    }

    fn forward(
        &mut self,
        packet: &Packet,
        device: DeviceId,
        channel: ChannelId,
    ) -> Result<(), SimulationError> {
        if let Some(forwarded) = packet.forwarded() {
            self.request_transmission(forwarded, device, channel)
        } else {
            self.drop_now(packet.clone(), device, DropReason::HopLimitExceeded)
        }
    }

    fn request_transmission(
        &mut self,
        packet: Packet,
        from: DeviceId,
        channel_id: ChannelId,
    ) -> Result<(), SimulationError> {
        let runtime = self
            .channels
            .get(&channel_id)
            .ok_or_else(|| SimulationError::UnknownChannel(channel_id.clone()))?;
        let (direction, to) = if runtime.endpoints[0] == from {
            (0, runtime.endpoints[1].clone())
        } else if runtime.endpoints[1] == from {
            (1, runtime.endpoints[0].clone())
        } else {
            return Err(SimulationError::InvalidConfig(
                crate::ConfigError::ChannelNotConnected {
                    device: from,
                    channel: channel_id,
                },
            ));
        };

        let start_ns = self
            .now
            .as_nanos()
            .max(runtime.direction_available_ns[direction]);
        let start = SimTime::from_nanos(start_ns);
        let state = runtime.state;
        let metrics = self.channel_metrics_at(channel_id.clone(), start)?;
        let Some(rate) = metrics.effective_bit_rate_bps else {
            let reason = if state == ChannelState::Severed {
                DropReason::ChannelSevered {
                    channel: channel_id,
                }
            } else {
                DropReason::OutOfRange {
                    channel: channel_id,
                }
            };
            return self.drop_at(start, packet, from, reason);
        };

        let serialization_ns = serialization_time_ns(packet.payload().len(), rate)?;
        let serialization_end_ns = start_ns
            .checked_add(serialization_ns)
            .ok_or(SimulationError::TimeOverflow)?;
        let receive_ns = serialization_end_ns
            .checked_add(metrics.propagation_delay_ns)
            .ok_or(SimulationError::TimeOverflow)?;
        self.channels
            .get_mut(&channel_id)
            .expect("validated channel must remain present")
            .direction_available_ns[direction] = serialization_end_ns;

        let receive_at = SimTime::from_nanos(receive_ns);
        self.enqueue(
            start_ns,
            InternalEvent::Emit(NetworkEvent::TransmissionStarted {
                at: SimTime::from_nanos(start_ns),
                packet: packet.clone(),
                channel: channel_id.clone(),
                from: from.clone(),
                to: to.clone(),
                receive_at,
                distance_mm: metrics
                    .distance_m
                    .map(distance_to_millimeters)
                    .transpose()?,
                effective_bit_rate_bps: rate,
            }),
        )?;
        self.enqueue(
            receive_ns,
            InternalEvent::Receive {
                packet,
                channel: channel_id,
                from,
                to,
            },
        )
    }

    fn drop_now(
        &mut self,
        packet: Packet,
        device: DeviceId,
        reason: DropReason,
    ) -> Result<(), SimulationError> {
        self.drop_at(self.now, packet, device, reason)
    }

    fn drop_at(
        &mut self,
        at: SimTime,
        packet: Packet,
        device: DeviceId,
        reason: DropReason,
    ) -> Result<(), SimulationError> {
        self.enqueue(
            at.as_nanos(),
            InternalEvent::Emit(NetworkEvent::PacketDropped {
                at,
                packet,
                device,
                reason,
            }),
        )
    }

    fn emit_now(&mut self, event: NetworkEvent) -> Result<(), SimulationError> {
        self.enqueue(self.now.as_nanos(), InternalEvent::Emit(event))
    }

    fn enqueue(&mut self, time_ns: u64, event: InternalEvent) -> Result<(), SimulationError> {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(SimulationError::TimeOverflow)?;
        self.queue.insert((time_ns, sequence), event);
        Ok(())
    }
}

fn distance_rate_bps(nominal_rate: u64, model: &DistanceChannel, distance_m: f64) -> u64 {
    match model.rate_model {
        DistanceRateModel::Constant => nominal_rate,
        DistanceRateModel::Linear {
            full_rate_distance_m,
            minimum_bit_rate_bps,
        } => {
            if distance_m <= full_rate_distance_m {
                return nominal_rate;
            }
            if distance_m >= model.max_range_m {
                return minimum_bit_rate_bps;
            }
            let fraction =
                (distance_m - full_rate_distance_m) / (model.max_range_m - full_rate_distance_m);
            let rate_span = (nominal_rate - minimum_bit_rate_bps) as f64;
            (nominal_rate as f64 - rate_span * fraction)
                .floor()
                .clamp(minimum_bit_rate_bps as f64, nominal_rate as f64) as u64
        }
    }
}

fn propagation_delay_ns(distance_m: f64, speed_mps: f64) -> Result<u64, SimulationError> {
    let delay = (distance_m / speed_mps * NANOS_PER_SECOND as f64).ceil();
    if !delay.is_finite() || delay < 0.0 || delay > u64::MAX as f64 {
        return Err(SimulationError::TimeOverflow);
    }
    Ok(delay as u64)
}

fn distance_to_millimeters(distance_m: f64) -> Result<u64, SimulationError> {
    let millimeters = (distance_m * 1_000.0).round();
    if !millimeters.is_finite() || millimeters < 0.0 || millimeters > u64::MAX as f64 {
        return Err(SimulationError::TimeOverflow);
    }
    Ok(millimeters as u64)
}

fn serialization_time_ns(payload_bytes: usize, bit_rate_bps: u64) -> Result<u64, SimulationError> {
    let bits = (payload_bytes as u128)
        .checked_mul(8)
        .ok_or(SimulationError::TimeOverflow)?;
    let numerator = bits
        .checked_mul(NANOS_PER_SECOND)
        .ok_or(SimulationError::TimeOverflow)?;
    let rate = u128::from(bit_rate_bps);
    let rounded = numerator
        .checked_add(rate - 1)
        .ok_or(SimulationError::TimeOverflow)?
        / rate;
    u64::try_from(rounded).map_err(|_| SimulationError::TimeOverflow)
}
