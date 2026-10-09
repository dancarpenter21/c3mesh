use super::*;

/// Observational counts of retained network state, not an estimate of heap usage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct HistoryStatistics {
    /// Earliest queryable time following explicit compaction, in nanoseconds.
    pub retained_from_ns: u64,
    /// Number of active devices.
    pub devices: usize,
    /// Retained mobility snapshots.
    pub mobility_entries: usize,
    /// Retained receiver-interference snapshots.
    pub interference_entries: usize,
    /// Individual interference contributions across retained snapshots.
    pub interference_contributions: usize,
    /// Internal events waiting to be processed.
    pub pending_events: usize,
    /// Pending receptions that may require historical interference.
    pub pending_receptions: usize,
}

impl Simulator {
    /// Returns aggregate counts without changing simulation state.
    #[must_use]
    pub fn history_statistics(&self) -> HistoryStatistics {
        HistoryStatistics {
            retained_from_ns: self.history_boundary.as_nanos(),
            devices: self.devices.len(),
            mobility_entries: self
                .devices
                .values()
                .map(|d| d.mobility_history.len())
                .sum(),
            interference_entries: self.devices.values().map(|d| d.interference.len()).sum(),
            interference_contributions: self
                .devices
                .values()
                .flat_map(|d| d.interference.values())
                .map(Vec::len)
                .sum(),
            pending_events: self.queue.len(),
            pending_receptions: self
                .queue
                .values()
                .filter(|e| matches!(e, InternalEvent::Receive { .. }))
                .count(),
        }
    }

    /// Earliest time accepted by historical queries after explicit compaction.
    /// Initially zero; ordinary advancement never discards history.
    #[must_use]
    pub const fn retained_history_from(&self) -> SimTime {
        self.history_boundary
    }

    /// Discards history no longer needed by current or pending receptions.
    ///
    /// Keeps the latest snapshot at or before the earlier of `now()` and every
    /// pending reception's start, plus all later snapshots (including future
    /// scheduled changes). This preserves reception-interval interference peaks.
    /// Historical queries before the returned boundary subsequently fail with
    /// `SimulationError::HistoryUnavailable`. Compaction is strictly opt-in.
    /// State retained for long active receptions is intentionally not capped.
    pub fn compact_history(&mut self) -> SimTime {
        let boundary = self
            .queue
            .values()
            .filter_map(|event| match event {
                InternalEvent::Receive {
                    reception_start_ns, ..
                } => Some(*reception_start_ns),
                _ => None,
            })
            .fold(self.now.as_nanos(), u64::min);
        debug_assert!(boundary >= self.history_boundary.as_nanos());
        for device in self.devices.values_mut() {
            compact_timeline(&mut device.mobility_history, boundary);
            compact_timeline(&mut device.interference, boundary);
        }
        self.history_boundary = SimTime::from_nanos(boundary);
        self.history_boundary
    }

    pub(super) fn check_history_time(&self, at: SimTime) -> Result<(), SimulationError> {
        if at < self.history_boundary {
            Err(SimulationError::HistoryUnavailable {
                retained_from: self.history_boundary,
            })
        } else {
            Ok(())
        }
    }
}

fn compact_timeline<T>(timeline: &mut BTreeMap<u64, T>, boundary: u64) {
    if let Some((&anchor, _)) = timeline.range(..=boundary).next_back() {
        // Pop only obsolete entries; do not rebuild or scan retained histories.
        while timeline
            .first_key_value()
            .is_some_and(|(&time, _)| time < anchor)
        {
            timeline.pop_first();
        }
    }
}
