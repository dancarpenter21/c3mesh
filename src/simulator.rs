use crate::packet_queue::{PacketQueue, ReservedQueue};
use crate::{
    ChannelConfig, ChannelId, ChannelMetrics, ChannelState, DeviceId, DeviceKind, DistanceChannel,
    DistanceRateModel, DropReason, FrequencyBand, InterferenceResponse, MobilityModel,
    NetworkConfig, NetworkEvent, Packet, PacketId, PacketMetadata, Position3D, RadioChannel,
    ReceiverInterference, SimTime, SimulationError, SimulatorOptions, TransmissionMetrics,
};
use std::collections::BTreeMap;

mod scheduler;

const DEFAULT_HOP_LIMIT: u16 = 64;
const NANOS_PER_SECOND: u128 = 1_000_000_000;

#[derive(Clone, Debug)]
struct ChannelRuntime {
    endpoints: [DeviceId; 2],
    nominal_bit_rate_bps: u64,
    propagation_delay_ns: u64,
    state: ChannelState,
    distance: Option<DistanceChannel>,
    radio: Option<RadioChannel>,
    direction_available_ns: [u64; 2],
    waiting: [PacketQueue; 2],
    legacy_waiting: [ReservedQueue; 2],
    packet_engine: bool,
}

impl From<ChannelConfig> for ChannelRuntime {
    fn from(value: ChannelConfig) -> Self {
        Self {
            endpoints: value.endpoints,
            nominal_bit_rate_bps: value.bit_rate_bps,
            propagation_delay_ns: value.propagation_delay_ns,
            state: value.state,
            distance: value.distance,
            radio: value.radio,
            direction_available_ns: [0, 0],
            waiting: Default::default(),
            legacy_waiting: Default::default(),
            packet_engine: false,
        }
    }
}

#[derive(Clone, Debug)]
struct DeviceRuntime {
    kind: DeviceKind,
    mobility: MobilityModel,
    interference: BTreeMap<u64, Vec<ReceiverInterference>>,
}

#[derive(Clone, Debug)]
enum InternalEvent {
    Inject(Packet),
    Drain {
        channel: ChannelId,
        direction: usize,
    },
    Emit(NetworkEvent),
    LegacyStart {
        event: NetworkEvent,
        channel: ChannelId,
        direction: usize,
    },
    Receive {
        packet: Packet,
        channel: ChannelId,
        from: DeviceId,
        to: DeviceId,
        base_bit_rate_bps: u64,
        selected_bit_rate_bps: u64,
        reception_start_ns: u64,
        lost: bool,
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
    options: SimulatorOptions,
    queue_wakes: BTreeMap<(ChannelId, usize), u64>,
    shared_medium_available: BTreeMap<String, u64>,
}

impl Simulator {
    /// Creates a simulator after validating the supplied topology.
    pub fn new(config: NetworkConfig) -> Result<Self, SimulationError> {
        Self::new_with_options(config, SimulatorOptions::default())
    }

    /// Creates a simulator with validated packet-engine channel options.
    pub fn new_with_options(
        config: NetworkConfig,
        options: SimulatorOptions,
    ) -> Result<Self, SimulationError> {
        config.validate()?;
        options.validate(&config)?;
        let devices = config
            .devices
            .into_iter()
            .map(|device| {
                (
                    device.id,
                    DeviceRuntime {
                        kind: device.kind,
                        mobility: device.mobility,
                        interference: BTreeMap::from([(0, device.interference)]),
                    },
                )
            })
            .collect();
        let channels = config
            .channels
            .into_iter()
            .map(|channel| {
                let id = channel.id.clone();
                let mut runtime = ChannelRuntime::from(channel);
                runtime.packet_engine = options.channels.contains_key(&id);
                (id, runtime)
            })
            .collect();
        Ok(Self {
            devices,
            channels,
            queue: BTreeMap::new(),
            now: SimTime::ZERO,
            next_sequence: 0,
            next_packet_id: 0,
            options,
            queue_wakes: BTreeMap::new(),
            shared_medium_available: BTreeMap::new(),
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

    /// Evaluates directional channel and receiver-interference conditions.
    pub fn transmission_metrics_at(
        &self,
        channel: impl Into<ChannelId>,
        from: impl Into<DeviceId>,
        at: SimTime,
    ) -> Result<TransmissionMetrics, SimulationError> {
        let channel_id = channel.into();
        let from = from.into();
        let runtime = self
            .channels
            .get(&channel_id)
            .ok_or_else(|| SimulationError::UnknownChannel(channel_id.clone()))?;
        let receiver = if runtime.endpoints[0] == from {
            runtime.endpoints[1].clone()
        } else if runtime.endpoints[1] == from {
            runtime.endpoints[0].clone()
        } else {
            return Err(SimulationError::InvalidConfig(
                crate::ConfigError::ChannelNotConnected {
                    device: from,
                    channel: channel_id,
                },
            ));
        };
        let base = self.channel_metrics_at(channel_id, at)?;
        let (frequency_band, jammed, effective_bit_rate_bps) = if let Some(radio) = runtime.radio {
            let interference = self.receiver_interference_at(receiver.clone(), at)?;
            let jammed = aggregate_jammed(&interference, radio.band);
            let effective = base
                .effective_bit_rate_bps
                .and_then(|rate| interference_rate_bps(rate, radio.interference_response, jammed));
            (Some(radio.band), jammed, effective)
        } else {
            (None, 0.0, base.effective_bit_rate_bps)
        };
        Ok(TransmissionMetrics {
            at,
            receiver,
            distance_m: base.distance_m,
            propagation_delay_ns: base.propagation_delay_ns,
            frequency_band,
            jammed,
            base_bit_rate_bps: base.effective_bit_rate_bps,
            effective_bit_rate_bps,
            available: effective_bit_rate_bps.is_some(),
        })
    }

    /// Returns a receiver's interference snapshot at a virtual time.
    pub fn receiver_interference_at(
        &self,
        receiver: impl Into<DeviceId>,
        at: SimTime,
    ) -> Result<Vec<ReceiverInterference>, SimulationError> {
        let receiver = receiver.into();
        let runtime = self
            .devices
            .get(&receiver)
            .ok_or_else(|| SimulationError::UnknownDevice(receiver.clone()))?;
        Ok(runtime
            .interference
            .range(..=at.as_nanos())
            .next_back()
            .map_or_else(Vec::new, |(_, snapshot)| snapshot.clone()))
    }

    /// Replaces a receiver's interference snapshot at the current virtual time.
    pub fn set_receiver_interference(
        &mut self,
        receiver: impl Into<DeviceId>,
        snapshot: Vec<ReceiverInterference>,
    ) -> Result<(), SimulationError> {
        self.schedule_receiver_interference(self.now, receiver, snapshot)
    }

    /// Schedules a persistent receiver-interference snapshot.
    ///
    /// A later call for the same receiver and timestamp replaces the earlier
    /// snapshot. An empty snapshot clears all receiver interference.
    pub fn schedule_receiver_interference(
        &mut self,
        at: SimTime,
        receiver: impl Into<DeviceId>,
        snapshot: Vec<ReceiverInterference>,
    ) -> Result<(), SimulationError> {
        if at < self.now {
            return Err(SimulationError::TimeInPast);
        }
        let receiver = receiver.into();
        if !self.devices.contains_key(&receiver) {
            return Err(SimulationError::UnknownDevice(receiver));
        }
        if !valid_receiver_interference(&snapshot) {
            return Err(SimulationError::InvalidReceiverInterference(receiver));
        }
        let runtime = self
            .devices
            .get_mut(&receiver)
            .expect("checked receiver must remain present");
        runtime.interference.insert(at.as_nanos(), snapshot);
        Ok(())
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
        self.schedule_send_with_hop_limit_and_metadata(
            at,
            source,
            destination,
            payload,
            hop_limit,
            PacketMetadata::default(),
        )
    }

    /// Schedules a packet with explicit endpoint-supplied scheduling metadata.
    pub fn schedule_send_with_metadata(
        &mut self,
        at: SimTime,
        source: impl Into<DeviceId>,
        destination: impl Into<DeviceId>,
        payload: impl Into<Vec<u8>>,
        metadata: PacketMetadata,
    ) -> Result<PacketId, SimulationError> {
        self.schedule_send_with_hop_limit_and_metadata(
            at,
            source,
            destination,
            payload,
            DEFAULT_HOP_LIMIT,
            metadata,
        )
    }

    /// Schedules a packet with both an explicit hop limit and scheduling metadata.
    pub fn schedule_send_with_hop_limit_and_metadata(
        &mut self,
        at: SimTime,
        source: impl Into<DeviceId>,
        destination: impl Into<DeviceId>,
        payload: impl Into<Vec<u8>>,
        hop_limit: u16,
        metadata: PacketMetadata,
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
        let packet = Packet::new(id, source, destination, payload.into(), hop_limit, metadata);
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
        self.step_until(None)
    }

    /// Processes all events through an inclusive boundary and sets the current time.
    ///
    /// Future internal and observable events remain pending. Moving backwards fails.
    pub fn advance_to(&mut self, boundary: SimTime) -> Result<Vec<NetworkEvent>, SimulationError> {
        if boundary < self.now {
            return Err(SimulationError::TimeInPast);
        }
        let mut events = Vec::new();
        while let Some(event) = self.step_until(Some(boundary))? {
            events.push(event);
        }
        self.now = boundary;
        Ok(events)
    }

    fn step_until(
        &mut self,
        boundary: Option<SimTime>,
    ) -> Result<Option<NetworkEvent>, SimulationError> {
        loop {
            if let Some(limit) = boundary {
                if self
                    .queue
                    .first_key_value()
                    .is_none_or(|((time, _), _)| *time > limit.as_nanos())
                {
                    return Ok(None);
                }
            }
            let Some(((time_ns, _), internal)) = self.queue.pop_first() else {
                return Ok(None);
            };
            self.now = SimTime::from_nanos(time_ns);
            match internal {
                InternalEvent::Emit(event) => return Ok(Some(event)),
                InternalEvent::LegacyStart {
                    event,
                    channel,
                    direction,
                } => {
                    self.channels
                        .get_mut(&channel)
                        .expect("validated channel remains present")
                        .legacy_waiting[direction]
                        .release_started(time_ns);
                    return Ok(Some(event));
                }
                InternalEvent::Drain { channel, direction } => {
                    let key = (channel.clone(), direction);
                    if self.queue_wakes.get(&key) == Some(&time_ns) {
                        self.queue_wakes.remove(&key);
                        self.drain_channel(channel, direction)?;
                    }
                }
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
                    base_bit_rate_bps,
                    selected_bit_rate_bps,
                    reception_start_ns,
                    lost,
                } => {
                    let failure = if packet.expired_at(self.now) {
                        Some(DropReason::Expired)
                    } else if lost {
                        Some(DropReason::ChannelLoss {
                            channel: channel.clone(),
                        })
                    } else {
                        None
                    };
                    if let Some(reason) = failure {
                        return Ok(Some(NetworkEvent::PacketDropped {
                            at: self.now,
                            packet,
                            device: to,
                            reason,
                        }));
                    }
                    if !self.reception_supported(
                        &to,
                        &channel,
                        reception_start_ns,
                        time_ns,
                        base_bit_rate_bps,
                        selected_bit_rate_bps,
                    )? {
                        return Ok(Some(NetworkEvent::PacketDropped {
                            at: self.now,
                            packet,
                            device: to,
                            reason: DropReason::ReceiverInterference { channel },
                        }));
                    }
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

    fn reception_supported(
        &self,
        receiver: &DeviceId,
        channel: &ChannelId,
        reception_start_ns: u64,
        receive_ns: u64,
        base_bit_rate_bps: u64,
        selected_bit_rate_bps: u64,
    ) -> Result<bool, SimulationError> {
        let Some(radio) = self
            .channels
            .get(channel)
            .ok_or_else(|| SimulationError::UnknownChannel(channel.clone()))?
            .radio
        else {
            return Ok(true);
        };
        let runtime = self
            .devices
            .get(receiver)
            .ok_or_else(|| SimulationError::UnknownDevice(receiver.clone()))?;
        let peak = peak_jammed(
            &runtime.interference,
            radio.band,
            reception_start_ns,
            receive_ns,
        );
        Ok(
            interference_rate_bps(base_bit_rate_bps, radio.interference_response, peak)
                .is_some_and(|supported| supported >= selected_bit_rate_bps),
        )
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
        if packet.expired_at(self.now) {
            return self.drop_now(packet, from, DropReason::Expired);
        }
        let runtime = self
            .channels
            .get_mut(&channel_id)
            .ok_or_else(|| SimulationError::UnknownChannel(channel_id.clone()))?;
        runtime.packet_engine |= !packet.metadata().is_default();
        if runtime.packet_engine {
            self.queue_transmission(packet, from, channel_id)
        } else {
            self.request_legacy_transmission(packet, from, channel_id)
        }
    }

    fn request_legacy_transmission(
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
        let metrics = self.transmission_metrics_at(channel_id.clone(), from.clone(), start)?;
        let Some(base_rate) = metrics.base_bit_rate_bps else {
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
        let Some(rate) = metrics.effective_bit_rate_bps else {
            return self.drop_at(
                start,
                packet,
                to,
                DropReason::ReceiverInterference {
                    channel: channel_id,
                },
            );
        };

        let serialization_ns = serialization_time_ns(packet.payload().len(), rate)?;
        let serialization_end_ns = start_ns
            .checked_add(serialization_ns)
            .ok_or(SimulationError::TimeOverflow)?;
        let receive_ns = serialization_end_ns
            .checked_add(metrics.propagation_delay_ns)
            .ok_or(SimulationError::TimeOverflow)?;
        let reception_start_ns = start_ns
            .checked_add(metrics.propagation_delay_ns)
            .ok_or(SimulationError::TimeOverflow)?;
        self.channels
            .get_mut(&channel_id)
            .expect("validated channel must remain present")
            .direction_available_ns[direction] = serialization_end_ns;

        if start_ns > self.now.as_nanos() {
            self.channels
                .get_mut(&channel_id)
                .expect("validated channel remains present")
                .legacy_waiting[direction]
                .reserve(start_ns, packet.payload().len())?;
        }
        let receive_at = SimTime::from_nanos(receive_ns);
        self.enqueue(
            start_ns,
            InternalEvent::LegacyStart {
                channel: channel_id.clone(),
                direction,
                event: NetworkEvent::TransmissionStarted {
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
                    frequency_band: metrics.frequency_band,
                },
            },
        )?;
        self.enqueue(
            receive_ns,
            InternalEvent::Receive {
                packet,
                channel: channel_id,
                from,
                to,
                base_bit_rate_bps: base_rate,
                selected_bit_rate_bps: rate,
                reception_start_ns,
                lost: false,
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

fn valid_receiver_interference(snapshot: &[ReceiverInterference]) -> bool {
    snapshot.iter().all(|interference| {
        interference.band.is_valid()
            && interference.jammed.is_finite()
            && (0.0..=1.0).contains(&interference.jammed)
    })
}

fn aggregate_jammed(snapshot: &[ReceiverInterference], victim: FrequencyBand) -> f64 {
    let victim_width = (victim.upper_hz - victim.lower_hz) as f64;
    snapshot
        .iter()
        .map(|interference| {
            let lower = victim.lower_hz.max(interference.band.lower_hz);
            let upper = victim.upper_hz.min(interference.band.upper_hz);
            if lower >= upper {
                0.0
            } else {
                interference.jammed * (upper - lower) as f64 / victim_width
            }
        })
        .sum::<f64>()
        .min(1.0)
}

fn interference_rate_bps(
    base_rate: u64,
    response: InterferenceResponse,
    jammed: f64,
) -> Option<u64> {
    if jammed <= response.unaffected_below {
        return Some(base_rate);
    }
    if jammed >= response.severed_at {
        return None;
    }
    let fraction =
        (response.severed_at - jammed) / (response.severed_at - response.unaffected_below);
    Some(((base_rate as f64 * fraction).floor() as u64).clamp(1, base_rate))
}

fn peak_jammed(
    timeline: &BTreeMap<u64, Vec<ReceiverInterference>>,
    victim: FrequencyBand,
    start_ns: u64,
    end_ns: u64,
) -> f64 {
    let mut peak = timeline
        .range(..=start_ns)
        .next_back()
        .map_or(0.0, |(_, snapshot)| aggregate_jammed(snapshot, victim));
    if start_ns == end_ns {
        return peak;
    }
    for (_, snapshot) in timeline.range((
        std::ops::Bound::Excluded(start_ns),
        std::ops::Bound::Excluded(end_ns),
    )) {
        peak = peak.max(aggregate_jammed(snapshot, victim));
    }
    peak
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
