# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2026-10-09

### Added

- Opt-in, reception-safe compaction of receiver-interference and mobility history.
- Retained-history boundary and aggregate history/pending-event statistics.
- Differential tests for jamming across long receptions, boundary timestamps,
  same-time updates, future changes, mobility, idle histories and endpoint retirement.

### Changed

- **Breaking:** `SimulationError` adds `HistoryUnavailable { retained_from }`.
  Queries before explicitly compacted history return this error; applications that
  never compact retain their existing historical-query behavior.

## [0.3.0] - 2026-10-04

### Added

- Atomic runtime registration of devices and channels without resetting virtual
  time, packet queues, shared-medium reservations, or deterministic loss state.
- Runtime mobility updates with historical trajectory queries.
- Next-event time inspection for coordinating an external simulation.
- Endpoint retirement that cancels affected queued and in-flight packets while
  preserving unrelated traffic. Retired identifiers cannot be reused.

### Changed

- **Breaking:** `DropReason` adds `EndpointRetired`. Downstream exhaustive
  matches must handle this variant. Serialized configurations remain compatible.

### Validation

- Regression coverage for atomic registration, trajectory changes, queue and
  in-flight cancellation, legacy transmissions, and identifier reuse.

## [0.2.0] - 2026-10-02

### Added

- Opt-in bounded directional queues, FIFO, strict-priority admission and service,
  and weighted-fair scheduling with stable endpoint flow identities.
- Tick-boundary advancement that retains future internal and observable events.
- Endpoint packet metadata with priority, traffic class, flow identity, and expiry.
- MTU and wire-overhead accounting, shared serialization media, and seeded loss.
- Live directional queue metrics, packet-engine validation, and congestion tests.
- Opt-in radio frequency bands and overlap-aware receiver interference.
- Normalized jamming response with directional degradation and severing.
- Initial, live, and scheduled interference snapshots for intermittent effects.
- Directional transmission metrics and in-flight receiver-interference drops.
- An intermittent jamming example and comprehensive usage/testing documentation.

### Changed

- Waiting-buffer byte limits no longer reject packets that can start on an idle
  serializer immediately. MTU checks still apply, and a busy shared medium or
  older waiting traffic requires admission under the waiting limits.
- In-flight packet expiry emits its terminal drop at the exclusive deadline
  without cancelling non-preemptive physical serialization reservations.
- Queue telemetry now reads per-channel counters and indexed legacy wire starts
  instead of scanning the global simulation event queue. Timestamp boundaries,
  full-duplex counts, and existing reservations during engine activation remain
  unchanged.

## [0.1.0] - 2026-07-19

### Added

- Deterministic virtual-time simulation with step, collect, and callback APIs.
- Source, sink, static switch, and static router device roles.
- Full-duplex rate-limited channels with propagation delay and link states.
- Complete-packet receive, delivery, transmission, and drop events.
- Serde configuration schemas with optional JSON and YAML loaders.
- Direct, switched, and routed demonstration topologies.
- Static, constant-velocity, and waypoint device mobility in three dimensions.
- Distance-aware propagation delay, communication range, and bitrate models.
- Time-indexed position and channel-metrics queries for moving networks.
