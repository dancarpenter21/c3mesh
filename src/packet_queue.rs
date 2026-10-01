use crate::{ChannelOptions, DeviceId, Packet, QueueDiscipline, SimulationError};
use std::cmp::Reverse;
use std::collections::{BTreeMap, VecDeque};

type Flow = (DeviceId, u8, u64);

#[derive(Clone, Debug)]
pub(crate) struct WaitingPacket {
    pub packet: Packet,
    pub from: DeviceId,
    pub wire_bytes: usize,
    finish: u128,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PacketQueue {
    pub packets: VecDeque<WaitingPacket>,
    pub bytes: usize,
    pub virtual_time: u128,
    finishes: BTreeMap<Flow, u128>,
}

fn flow(packet: &Packet) -> Flow {
    (
        packet.source().clone(),
        packet.metadata().traffic_class,
        packet.metadata().flow_id,
    )
}

impl PacketQueue {
    pub fn fits(&self, wire_bytes: usize, options: &ChannelOptions) -> bool {
        options
            .queue
            .max_packets
            .is_none_or(|limit| self.packets.len() < limit)
            && self
                .bytes
                .checked_add(wire_bytes)
                .is_some_and(|bytes| options.queue.max_bytes.is_none_or(|limit| bytes <= limit))
    }

    pub fn admit(
        &mut self,
        packet: Packet,
        from: DeviceId,
        wire_bytes: usize,
        options: &ChannelOptions,
    ) -> Result<(), SimulationError> {
        let key = flow(&packet);
        let weight = u128::from(
            *options
                .traffic_class_weights
                .get(&packet.metadata().traffic_class)
                .unwrap_or(&1),
        );
        let start = self
            .finishes
            .get(&key)
            .copied()
            .unwrap_or(0)
            .max(self.virtual_time);
        let cost = ((wire_bytes as u128) << 32).div_ceil(weight);
        let finish = start
            .checked_add(cost)
            .ok_or(SimulationError::TimeOverflow)?;
        self.bytes = self
            .bytes
            .checked_add(wire_bytes)
            .ok_or(SimulationError::TimeOverflow)?;
        self.finishes.insert(key, finish);
        self.packets.push_back(WaitingPacket {
            packet,
            from,
            wire_bytes,
            finish,
        });
        Ok(())
    }

    pub fn remove(&mut self, index: usize) -> WaitingPacket {
        let selected = self
            .packets
            .remove(index)
            .expect("selected queue entry must exist");
        self.bytes -= selected.wire_bytes;
        let key = flow(&selected.packet);
        if !self.packets.iter().any(|item| flow(&item.packet) == key) {
            self.finishes.remove(&key);
        }
        selected
    }

    pub fn select(&self, discipline: QueueDiscipline) -> usize {
        match discipline {
            QueueDiscipline::Fifo => 0,
            QueueDiscipline::StrictPriority => self
                .packets
                .iter()
                .enumerate()
                .max_by_key(|(index, item)| (item.packet.metadata().priority, Reverse(*index)))
                .map_or(0, |(index, _)| index),
            QueueDiscipline::WeightedFair => self
                .packets
                .iter()
                .enumerate()
                .min_by_key(|(index, item)| (item.finish, *index))
                .map_or(0, |(index, _)| index),
        }
    }

    pub fn take(&mut self, discipline: QueueDiscipline) -> WaitingPacket {
        let selected = self.remove(self.select(discipline));
        self.virtual_time = self.virtual_time.max(selected.finish);
        selected
    }

    pub fn lower_priority(&self, priority: u8) -> Option<usize> {
        self.packets
            .iter()
            .enumerate()
            .filter(|(_, item)| item.packet.metadata().priority < priority)
            .min_by_key(|(index, item)| (item.packet.metadata().priority, Reverse(*index)))
            .map(|(index, _)| index)
    }
}

// Legacy wire starts are appended in timestamp order. Prefix byte totals let
// telemetry exclude every start at or before now without scanning the simulator
// event queue, even between public events at the same timestamp.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReservedQueue {
    starts: VecDeque<(u64, u128)>,
    retired_bytes: u128,
}

impl ReservedQueue {
    pub fn reserve(&mut self, starts_at: u64, bytes: usize) -> Result<(), SimulationError> {
        debug_assert!(self.starts.back().is_none_or(|(at, _)| *at <= starts_at));
        let cumulative = self
            .starts
            .back()
            .map_or(self.retired_bytes, |(_, bytes)| *bytes)
            .checked_add(bytes as u128)
            .ok_or(SimulationError::TimeOverflow)?;
        self.starts.push_back((starts_at, cumulative));
        Ok(())
    }

    pub fn release_started(&mut self, now: u64) {
        while self.starts.front().is_some_and(|(at, _)| *at <= now) {
            self.retired_bytes = self
                .starts
                .pop_front()
                .expect("checked front reservation")
                .1;
        }
        if self.starts.is_empty() {
            self.retired_bytes = 0;
        }
    }

    pub fn metrics(&self, now: u64) -> (usize, usize) {
        let first_waiting = self.starts.partition_point(|(at, _)| *at <= now);
        let Some((_, total)) = self.starts.back() else {
            return (0, 0);
        };
        let preceding = if first_waiting == 0 {
            self.retired_bytes
        } else {
            self.starts[first_waiting - 1].1
        };
        let bytes = usize::try_from(total - preceding)
            .expect("queued payloads fit into process address space");
        (self.starts.len() - first_waiting, bytes)
    }
}
