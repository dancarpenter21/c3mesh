use super::*;
use crate::{ChannelOptions, ChannelQueueMetrics, QueueDiscipline};

impl Simulator {
    /// Returns live waiting-queue occupancy for both channel directions.
    ///
    /// Packets currently serializing or propagating are excluded. Queries use
    /// only the selected channel; they never scan the global event queue.
    pub fn channel_queue_metrics(
        &self,
        channel: impl Into<ChannelId>,
    ) -> Result<ChannelQueueMetrics, SimulationError> {
        let channel = channel.into();
        let runtime = self
            .channels
            .get(&channel)
            .ok_or_else(|| SimulationError::UnknownChannel(channel.clone()))?;
        let forward = runtime.legacy_waiting[0].metrics(self.now.as_nanos());
        let reverse = runtime.legacy_waiting[1].metrics(self.now.as_nanos());
        Ok(ChannelQueueMetrics {
            packets_0_to_1: runtime.waiting[0].packets.len() + forward.0,
            packets_1_to_0: runtime.waiting[1].packets.len() + reverse.0,
            bytes_0_to_1: runtime.waiting[0].bytes + forward.1,
            bytes_1_to_0: runtime.waiting[1].bytes + reverse.1,
        })
    }

    pub(super) fn queue_transmission(
        &mut self,
        packet: Packet,
        from: DeviceId,
        channel: ChannelId,
    ) -> Result<(), SimulationError> {
        let runtime = self
            .channels
            .get(&channel)
            .ok_or_else(|| SimulationError::UnknownChannel(channel.clone()))?;
        let direction = if runtime.endpoints[0] == from {
            0
        } else if runtime.endpoints[1] == from {
            1
        } else {
            return Err(crate::ConfigError::ChannelNotConnected {
                device: from,
                channel,
            }
            .into());
        };
        let options = self
            .options
            .channels
            .get(&channel)
            .cloned()
            .unwrap_or_default();
        let wire_bytes = packet
            .payload()
            .len()
            .checked_add(options.wire_overhead_bytes)
            .ok_or(SimulationError::TimeOverflow)?;
        if let Some(mtu_bytes) = options.mtu_bytes {
            if wire_bytes > mtu_bytes {
                return self.drop_now(
                    packet,
                    from,
                    DropReason::MtuExceeded {
                        channel,
                        mtu_bytes,
                        wire_bytes,
                    },
                );
            }
        }
        self.expire_waiting(&channel, direction)?;
        let waiting = &self.channels[&channel].waiting[direction];
        let can_preempt = options.queue.discipline == QueueDiscipline::StrictPriority
            && preemption_can_fit(waiting, wire_bytes, packet.metadata().priority, &options);
        if !waiting.fits(wire_bytes, &options) && !can_preempt {
            return self.drop_now(packet, from, DropReason::QueueOverflow { channel });
        }
        while !self.channels[&channel].waiting[direction].fits(wire_bytes, &options) {
            let waiting = &mut self
                .channels
                .get_mut(&channel)
                .expect("known channel")
                .waiting[direction];
            let index = waiting
                .lower_priority(packet.metadata().priority)
                .expect("preemption capacity was checked");
            let evicted = waiting.remove(index);
            self.drop_now(
                evicted.packet,
                evicted.from,
                DropReason::QueueOverflow {
                    channel: channel.clone(),
                },
            )?;
        }
        self.channels
            .get_mut(&channel)
            .expect("known channel")
            .waiting[direction]
            .admit(packet, from, wire_bytes, &options)?;
        if self.direction_ready_ns(&channel, direction, &options) <= self.now.as_nanos() {
            self.queue_wakes.remove(&(channel.clone(), direction));
            self.drain_channel(channel, direction)
        } else {
            self.request_drain(&channel, direction, &options)
        }
    }

    fn expire_waiting(
        &mut self,
        channel: &ChannelId,
        direction: usize,
    ) -> Result<(), SimulationError> {
        loop {
            let waiting = &mut self
                .channels
                .get_mut(channel)
                .expect("known channel")
                .waiting[direction];
            let Some(index) = waiting
                .packets
                .iter()
                .position(|entry| entry.packet.expired_at(self.now))
            else {
                return Ok(());
            };
            let expired = waiting.remove(index);
            self.drop_now(expired.packet, expired.from, DropReason::Expired)?;
        }
    }

    fn direction_ready_ns(
        &self,
        channel: &ChannelId,
        direction: usize,
        options: &ChannelOptions,
    ) -> u64 {
        let direction_ready = self.channels[channel].direction_available_ns[direction];
        let medium_ready = options
            .shared_medium
            .as_ref()
            .and_then(|name| self.shared_medium_available.get(name))
            .copied()
            .unwrap_or(0);
        direction_ready.max(medium_ready).max(self.now.as_nanos())
    }

    fn request_drain(
        &mut self,
        channel: &ChannelId,
        direction: usize,
        options: &ChannelOptions,
    ) -> Result<(), SimulationError> {
        let waiting = &self.channels[channel].waiting[direction];
        if waiting.packets.is_empty() {
            return Ok(());
        }
        let ready = self.direction_ready_ns(channel, direction, options);
        let expiry = waiting
            .packets
            .iter()
            .filter_map(|entry| entry.packet.metadata().expires_at)
            .map(SimTime::as_nanos)
            .min()
            .unwrap_or(u64::MAX);
        let wake_at = ready.min(expiry).max(self.now.as_nanos());
        let key = (channel.clone(), direction);
        if self
            .queue_wakes
            .get(&key)
            .is_some_and(|existing| *existing <= wake_at)
        {
            return Ok(());
        }
        self.enqueue(
            wake_at,
            InternalEvent::Drain {
                channel: channel.clone(),
                direction,
            },
        )?;
        self.queue_wakes.insert(key, wake_at);
        Ok(())
    }

    pub(super) fn drain_channel(
        &mut self,
        channel: ChannelId,
        direction: usize,
    ) -> Result<(), SimulationError> {
        let options = self
            .options
            .channels
            .get(&channel)
            .cloned()
            .unwrap_or_default();
        self.expire_waiting(&channel, direction)?;
        if self.direction_ready_ns(&channel, direction, &options) > self.now.as_nanos() {
            return self.request_drain(&channel, direction, &options);
        }
        loop {
            let runtime = self.channels.get_mut(&channel).expect("known channel");
            if runtime.waiting[direction].packets.is_empty() {
                return Ok(());
            }
            let to = runtime.endpoints[1 - direction].clone();
            let state = runtime.state;
            let selected = runtime.waiting[direction].take(options.queue.discipline);
            let metrics =
                self.transmission_metrics_at(channel.clone(), selected.from.clone(), self.now)?;
            let Some(base_rate) = metrics.base_bit_rate_bps else {
                let reason = if state == ChannelState::Severed {
                    DropReason::ChannelSevered {
                        channel: channel.clone(),
                    }
                } else {
                    DropReason::OutOfRange {
                        channel: channel.clone(),
                    }
                };
                self.drop_now(selected.packet, selected.from, reason)?;
                continue;
            };
            let Some(rate) = metrics.effective_bit_rate_bps else {
                self.drop_now(
                    selected.packet,
                    to,
                    DropReason::ReceiverInterference {
                        channel: channel.clone(),
                    },
                )?;
                continue;
            };
            let start_ns = self.now.as_nanos();
            let serialization_ns = serialization_time_ns(selected.wire_bytes, rate)?;
            let end_ns = start_ns
                .checked_add(serialization_ns)
                .ok_or(SimulationError::TimeOverflow)?;
            let receive_ns = end_ns
                .checked_add(metrics.propagation_delay_ns)
                .ok_or(SimulationError::TimeOverflow)?;
            let reception_start_ns = start_ns
                .checked_add(metrics.propagation_delay_ns)
                .ok_or(SimulationError::TimeOverflow)?;
            let lost = packet_lost(
                self.options.seed,
                &selected.packet,
                &channel,
                options.loss_basis_points,
            );
            self.channels
                .get_mut(&channel)
                .expect("known channel")
                .direction_available_ns[direction] = end_ns;
            if let Some(medium) = &options.shared_medium {
                self.shared_medium_available.insert(medium.clone(), end_ns);
            }
            self.enqueue(
                start_ns,
                InternalEvent::Emit(NetworkEvent::TransmissionStarted {
                    at: self.now,
                    packet: selected.packet.clone(),
                    channel: channel.clone(),
                    from: selected.from.clone(),
                    to: to.clone(),
                    receive_at: SimTime::from_nanos(receive_ns),
                    distance_mm: metrics
                        .distance_m
                        .map(distance_to_millimeters)
                        .transpose()?,
                    effective_bit_rate_bps: rate,
                    frequency_band: metrics.frequency_band,
                }),
            )?;
            self.enqueue(
                receive_ns,
                InternalEvent::Receive {
                    packet: selected.packet,
                    channel: channel.clone(),
                    from: selected.from,
                    to,
                    base_bit_rate_bps: base_rate,
                    selected_bit_rate_bps: rate,
                    reception_start_ns,
                    lost,
                },
            )?;
            return self.request_drain(&channel, direction, &options);
        }
    }
}

fn preemption_can_fit(
    waiting: &PacketQueue,
    wire_bytes: usize,
    priority: u8,
    options: &ChannelOptions,
) -> bool {
    let retained: Vec<_> = waiting
        .packets
        .iter()
        .filter(|entry| entry.packet.metadata().priority >= priority)
        .collect();
    let retained_bytes = retained.iter().map(|entry| entry.wire_bytes).sum::<usize>();
    options
        .queue
        .max_packets
        .is_none_or(|limit| retained.len() < limit)
        && retained_bytes
            .checked_add(wire_bytes)
            .is_some_and(|bytes| options.queue.max_bytes.is_none_or(|limit| bytes <= limit))
}

fn packet_lost(seed: u64, packet: &Packet, channel: &ChannelId, basis_points: u16) -> bool {
    if basis_points == 0 {
        return false;
    }
    if basis_points == 10_000 {
        return true;
    }
    // Stable across platforms and tick sizes: no process-seeded hashes or RNG state.
    let mut key = seed
        ^ packet.id().get().wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ u64::from(packet.hops_remaining()).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    for byte in channel.to_string().bytes() {
        key = key.wrapping_mul(0x100_0000_01b3) ^ u64::from(byte);
    }
    key = (key ^ (key >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    key = (key ^ (key >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    key ^= key >> 31;
    ((u128::from(key) * 10_000) >> 64) < u128::from(basis_points)
}
