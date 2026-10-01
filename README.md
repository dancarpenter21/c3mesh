# c3mesh

`c3mesh` is a deterministic, discrete-event Rust library for modeling how
long packets take to move through fixed and mobile communication networks. It
accounts for serialization rate, distance, propagation delay, channel
availability, and store-and-forward behavior without sleeping or depending on
wall-clock time.

The initial model provides four device roles:

- **Sources** originate packets on a configured egress channel.
- **Sinks** accept packets addressed to them.
- **Switches** forward packets using exact destination-to-channel rules.
- **Routers** use exact routes and an optional default route.

Channels are point-to-point and full-duplex. Each direction serializes packets
independently in FIFO order and may be operational, degraded, or severed.

## Quick start

```rust
use c3mesh::{
    ChannelConfig, ChannelState, DeviceConfig, DeviceKind, NetworkConfig,
    NetworkEvent, SimTime, Simulator,
};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let network = NetworkConfig {
    devices: vec![
        DeviceConfig {
            id: "sender".into(),
            kind: DeviceKind::Source { egress: "link".into() },
            mobility: Default::default(),
            interference: vec![],
        },
        DeviceConfig {
            id: "receiver".into(),
            kind: DeviceKind::Sink,
            mobility: Default::default(),
            interference: vec![],
        },
    ],
    channels: vec![ChannelConfig {
        id: "link".into(),
        endpoints: ["sender".into(), "receiver".into()],
        bit_rate_bps: 1_000_000,
        propagation_delay_ns: 10_000_000,
        state: ChannelState::Operational,
        distance: None,
        radio: None,
    }],
};

let mut simulator = Simulator::new(network)?;
simulator.send("sender", "receiver", vec![0; 1_000])?;
let events = simulator.run()?;

assert!(events.iter().any(|event| matches!(
    event,
    NetworkEvent::PacketDelivered { at, .. }
        if *at == SimTime::from_nanos(18_000_000)
)));
# Ok(())
# }
```

The 1,000-byte payload takes 8 ms to serialize at 1 Mbit/s and another
10 ms to propagate, so delivery occurs at 18 ms of virtual time.

## Timing and events

For each transmission, the simulator calculates:

```text
start = max(requested time, channel direction available time)
receive = start + ceil(payload bits / effective bit rate) + propagation delay
```

The channel direction becomes available after serialization finishes, allowing
packets already on the wire to propagate while the next packet serializes.
`NetworkEvent::DataReceived` is emitted only at `receive`, when the complete
packet has arrived. A switch or router can begin forwarding at that same virtual
timestamp. Sink delivery and expected failures are reported as separate events.

Events can be observed in three ways:

- `Simulator::step` advances to one observable event.
- `Simulator::run` returns all remaining events in deterministic order.
- `Simulator::run_with` invokes a callback as each event occurs.
- `Simulator::advance_to` processes every event through an inclusive tick boundary,
  retains future events, and advances idle virtual time without overshooting.

All time is virtual and measured in integer nanoseconds. Runs perform no
wall-clock sleeping and are deterministic for a given topology and send order.


## Packet engine and tick integration

Use `Simulator::new_with_options` to configure a channel's packet engine
without changing the topology schema. An explicit channel entry enables a live
waiting queue. `Simulator::new` and channels without entries preserve the
original full-duplex FIFO reservation behavior. Sending nondefault
`PacketMetadata` also enables live queueing on the traversed channels; existing
wire reservations remain intact.

```rust
use c3mesh::{
    ChannelOptions, NetworkConfig, PacketMetadata, QueueConfig, QueueDiscipline,
    SimTime, Simulator, SimulatorOptions,
};
use std::collections::BTreeMap;

# fn example(network: NetworkConfig) -> Result<(), c3mesh::SimulationError> {
let options = SimulatorOptions {
    seed: 42,
    channels: BTreeMap::from([("link".into(), ChannelOptions {
        mtu_bytes: Some(1_500),
        wire_overhead_bytes: 28,
        queue: QueueConfig {
            max_packets: Some(64),
            max_bytes: Some(96_000),
            discipline: QueueDiscipline::StrictPriority,
        },
        shared_medium: Some("command-radio".into()),
        loss_basis_points: 100, // 1 percent, deterministic for this seed.
        ..Default::default()
    })]),
};
let mut simulator = Simulator::new_with_options(network, options)?;
simulator.schedule_send_with_metadata(
    SimTime::ZERO, "sender", "receiver", b"move".to_vec(),
    PacketMetadata {
        priority: 230,
        traffic_class: 1,
        flow_id: 7,
        expires_at: Some(SimTime::from_nanos(1_000_000_000)),
    },
)?;
let events = simulator.advance_to(SimTime::from_nanos(50_000_000))?;
let waiting = simulator.channel_queue_metrics("link")?;
# let _ = (events, waiting);
# Ok(())
# }
```

Queue telemetry queries inspect only the selected channel: live queues use stored counters, and legacy FIFO reservations use indexed wire-start times. Packets starting exactly at the current virtual time are excluded even when other public events at that timestamp have yet to be consumed.

Queue bounds apply **per direction** to waiting packets and their full wire
size, excluding packets already serializing or propagating. An idle serializer
starts its first admitted packet immediately. FIFO and weighted-fair queues
drop the arriving packet when full. Strict priority can evict lower-priority
waiting packets to admit a higher-priority packet; it preserves older packets
on equal-priority eviction ties and never partially evicts if the arrival still
cannot fit. Packets already on the wire are never preempted.

Scheduling policies select from the queue when serialization becomes available:

- `fifo` preserves admission order.
- `strict_priority` serves larger priority values first, with FIFO ties.
- `weighted_fair` serves by integer virtual finish time. A flow is identified
  by source, traffic class, and endpoint-assigned flow ID. Service cost uses
  wire bytes divided by the positive class weight; unspecified weights are one.
  Each flow retains its packet order. Assign stable flow IDs rather than a
  new ID for each packet when modeling sustained traffic.

A channel's MTU includes modeled overhead. Oversized packets produce
`DropReason::MtuExceeded`; they are not fragmented. Overflow produces
`QueueOverflow`. Channel state, mobility, and receiver interference are
evaluated at the queued packet's actual wire start, so changes during its wait
affect it. Existing in-flight reception checks continue to apply.

Channels with the same `shared_medium` ID share one serialization resource,
including opposite directions. Propagation does not occupy the resource.
Simultaneous contenders resolve deterministically in event order. Channels
with different IDs, and ordinary full-duplex channels, remain independent.

Loss is specified in basis points from 0 through 10,000. Loss decisions use a
stable integer hash of seed, packet identity, channel identity, and hop budget;
they are independent of tick size. Lost packets consume wire time and produce
`ChannelLoss` at the scheduled receive time, without a receive or delivery
event.

An expiry timestamp is an **exclusive** deadline. A packet already expired at
injection is dropped immediately. Waiting packets expire at their deadline and
release capacity, including when a deadline precedes an existing queue wakeup.
An in-flight packet whose arrival is at or after its deadline produces
`Expired` at arrival and does not forward or deliver. Endpoint metadata follows
the packet through routers unchanged; routers never inspect application payloads.

`advance_to(boundary)` returns all events at or before that boundary, including
forwarding and delivery events generated at the boundary itself. It processes
no later internal events, rejects backwards movement, and sets idle time to
the boundary. Keep queued packets pending between game ticks instead of using
`run` to complete future deliveries synchronously.

Run the congestion example and focused tests:

```console
cargo run --example bounded_queues
cargo test --test packet_engine --all-features
```

## Serialized topologies

All configuration types implement serde's `Serialize` and `Deserialize`.
Convenience loaders are available through optional features:

```toml
[dependencies]
c3mesh = { version = "0.1", features = ["json", "yaml"] }
```

Device variants use a `kind` tag. For example:

```yaml
devices:
  - id: client
    kind: source
    egress: access
  - id: router
    kind: router
    routes:
      server: backbone
    default_route: backbone
  - id: server
    kind: sink
channels:
  - id: access
    endpoints: [client, router]
    bit_rate_bps: 10000000
    propagation_delay_ns: 100000
  - id: backbone
    endpoints: [router, server]
    bit_rate_bps: 1000000000
    propagation_delay_ns: 500000
    state:
      kind: degraded
      effective_bit_rate_bps: 500000000
```

Use `NetworkConfig::from_json_str` with the `json` feature and
`NetworkConfig::from_yaml_str` with the `yaml` feature. Both loaders validate
the parsed topology before returning it.

## Channel state

`ChannelState::Operational` uses the nominal bit rate. A degraded channel uses
its lower configured effective rate, while a severed channel rejects new
transmissions with a `PacketDropped` event. Changing a channel's state affects
transmissions requested afterward; already scheduled arrivals are unchanged.

## Examples

```console
cargo run --example direct
cargo run --example yaml_switch --features yaml
cargo run --example json_router --features json
cargo run --example moving_link
cargo run --example intermittent_jamming
```

The examples cover direct delivery, static switch forwarding, routed multi-hop
delivery, a moving aircraft radio link, and intermittent receiver jamming.

## Moving devices and distance-aware links

Every device has a `MobilityModel`. Existing configurations default to a
stationary device at the coordinate origin, while physical simulations can use
a fixed 3D position, constant Cartesian velocity, or a time-ordered waypoint
trajectory. Coordinates are measured in meters but are reference-frame
agnostic, so the same API supports local maps and Earth-centered coordinates.

Add `DistanceChannel` to a channel to derive propagation delay, range, and
bitrate from the endpoint positions:

```rust
use c3mesh::{DistanceChannel, DistanceRateModel};

let radio = DistanceChannel {
    propagation_speed_mps: 299_792_458.0,
    max_range_m: 2_000_000.0,
    rate_model: DistanceRateModel::Linear {
        full_rate_distance_m: 500_000.0,
        minimum_bit_rate_bps: 1_000_000,
    },
};
# let _ = radio;
```

At a packet's actual serialization start time, the simulator calculates the
current endpoint distance. This includes any time the packet spent waiting for
the channel. The selected distance determines whether the link is in range,
its effective bitrate, and the signal propagation delay. Consequently, packets
sent at different virtual times automatically experience the moving topology.

Use `Simulator::device_position_at` to inspect a trajectory and
`Simulator::channel_metrics_at` to query distance, availability, propagation,
and bitrate at any virtual timestamp. `TransmissionStarted` events also include
the sampled distance and effective bitrate. The `moving_link` example models a
moving aircraft and a stationary ground station:

```console
cargo run --example moving_link
```

Distance is sampled at transmission start in version 0.1; acceleration,
orbital mechanics, obstruction, Doppler shift, and changes during the
serialization of one packet should be represented by external trajectory/link
models or shorter packet intervals.

## Wireless spectrum and receiver jamming

Jamming is modeled as a condition at a receiving device. A radio channel says
which frequencies a transmission occupies, while the receiver has one or more
interference entries covering frequency bands. The simulator combines the
entries that overlap the transmission band and converts the result into link
rate or an unavailable link.

The library deliberately does not determine whether a receiver lies inside a
jammed region. A terrain, propagation, electronic-warfare, or game-world model
should make that determination and update the receiver's interference
snapshot. The same API represents deliberate jamming, friendly co-channel
transmissions, faulty electronics, and other unintended interference.

Channels opt into this behavior with `RadioChannel`. A channel whose `radio`
field is `None` is bandless and ignores all receiver interference.

```rust
use c3mesh::{
    FrequencyBand, InterferenceResponse, RadioChannel, ReceiverInterference,
};

let radio = RadioChannel {
    // Half-open interval: 2.400 GHz is included and 2.420 GHz is excluded.
    band: FrequencyBand::new(2_400_000_000, 2_420_000_000),
    interference_response: InterferenceResponse {
        unaffected_below: 0.1,
        severed_at: 0.9,
    },
};

let initial_receiver_interference = vec![ReceiverInterference {
    band: FrequencyBand::new(2_405_000_000, 2_415_000_000),
    jammed: 0.6,
}];
# let _ = (radio, initial_receiver_interference);
```

Assign `Some(radio)` to `ChannelConfig::radio`. Assign the initial interference
vector to the receiving `DeviceConfig::interference`; an empty vector means the
receiver starts clear. Both fields use serde, so the same model can be written
in JSON or YAML:

```yaml
devices:
  - id: sender
    kind: source
    egress: radio
  - id: receiver
    kind: sink
    interference:
      - band: { lower_hz: 2405000000, upper_hz: 2415000000 }
        jammed: 0.6
channels:
  - id: radio
    endpoints: [sender, receiver]
    bit_rate_bps: 1000000
    radio:
      band: { lower_hz: 2400000000, upper_hz: 2420000000 }
      interference_response:
        unaffected_below: 0.1
        severed_at: 0.9
```

### What the 0-1 magnitude means

`jammed` is a normalized receiver-side severity, not dBm, SINR, a percentage,
or a packet-loss probability. Values must be finite and between `0.0` and
`1.0`, inclusive:

- `0.0` contributes no interference.
- `1.0` is maximum severity when the entry covers the entire radio band.
- Intermediate values represent proportional severity before spectral overlap
  and the radio's configured tolerance are applied.

For each entry, the simulator multiplies `jammed` by the fraction of the radio
band it overlaps. Contributions are added and capped at `1.0`. For example, a
`0.6` entry covering half of the victim band contributes `0.3`. A second
full-band `0.2` entry produces an aggregate magnitude of `0.5`. Bands are
half-open, so `[100, 200)` and `[200, 300)` do not overlap.

The aggregate magnitude is mapped through `InterferenceResponse`. At or below
`unaffected_below`, the channel retains the rate already selected from channel
state and distance. At or above `severed_at`, it is unavailable. Between those
thresholds, rate scales linearly. With the default thresholds of `0.0` and
`1.0`, an aggregate magnitude of `0.5` supports half the base bitrate.

### Dynamic and intermittent interference

Interference snapshots are persistent and piecewise constant. Replacing a
snapshot replaces every prior band contribution at that timestamp; pass an
empty vector to clear the receiver. Scheduled changes may be inserted in any
order, but cannot be scheduled before the simulator's current time. The most
recent call for a receiver and timestamp wins.

```rust
use c3mesh::{
    FrequencyBand, ReceiverInterference, SimTime, SimulationError, Simulator,
};

# fn configure_jamming(simulator: &mut Simulator) -> Result<(), SimulationError> {
let pulse = vec![ReceiverInterference {
    band: FrequencyBand::new(2_400_000_000, 2_420_000_000),
    jammed: 0.75,
}];

// Apply a live update at the simulator's current time.
simulator.set_receiver_interference("receiver", pulse.clone())?;

// Replace it with a scheduled pulse, then clear it 10 ms later.
simulator.schedule_receiver_interference(
    SimTime::from_nanos(1_000_000_000),
    "receiver",
    pulse,
)?;
simulator.schedule_receiver_interference(
    SimTime::from_nanos(1_010_000_000),
    "receiver",
    vec![],
)?;
# Ok(())
# }
```

Rate is selected from the receiver condition at the transmission's actual
start, after any FIFO wait. The simulator then checks the entire interval in
which bits reach the receiver:

```text
serialization interval = [start, start + serialization time)
receive window         = [start + propagation delay, receive_at)
receive_at              = start + serialization time + propagation delay
```

If a pulse during that receive window would support a lower bitrate than the
one selected at transmission start, the packet is dropped at its scheduled
receive time with `DropReason::ReceiverInterference`. It produces no
`DataReceived` or `PacketDelivered` event. This catches a pulse that has ended
before packet completion without retroactively moving the channel's FIFO
schedule. Interference before the first bit arrives or beginning exactly when
the complete packet arrives does not affect that packet.

Receiver interference is directional. Jamming device B affects transmissions
from A to B; it does not automatically affect transmissions from B to A. It
also does not automatically interfere with other receivers or turn ordinary
c3mesh transmissions into interference sources. External models should update
each affected receiver explicitly.

### Inspecting and testing jamming

Use `Simulator::receiver_interference_at` to inspect a raw scheduled snapshot.
Use `Simulator::transmission_metrics_at(channel, sender, time)` to see the
resolved receiver, frequency band, overlap-adjusted magnitude, base bitrate,
effective bitrate, and availability for one direction. `TransmissionStarted`
reports the selected bitrate and band; jamming failures appear as
`PacketDropped` events.

The complete degradation, recovery, unintended-interference, and intermittent
pulse workflow is executable:

```console
cargo run --example intermittent_jamming
```

For library development, run the focused jamming tests or the complete suite:

```console
cargo test --test jamming
cargo test --test jamming intermittent_pulse_during_reception_corrupts_in_flight_packet
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo test --doc --all-features
```

Useful expected event sequences are:

- Degraded but stable: `TransmissionStarted`, `DataReceived`, then delivery or
  forwarding at the lower bitrate.
- Severed at start: one `PacketDropped` event at the receiver.
- Off-band interference: the normal unjammed event sequence.
- A worsening in-flight pulse: `TransmissionStarted`, then `PacketDropped` at
  the previously calculated receive time.

Common mistakes are treating `50` as 50% instead of using `0.5`, defining an
empty or reversed frequency interval, expecting a new snapshot to merge with
the previous one, or scheduling a pulse after the relevant receive window.
Invalid bands, thresholds, and magnitudes are rejected during topology or
runtime validation.

This is an abstract deterministic model. It does not calculate received power,
noise floors, SINR, modulation/coding behavior, BER/PER, retransmission,
antenna patterns, jammer propagation, adjacent-channel receiver blocking, or
spectral skirts. Approximate skirts or leakage by supplying additional bands;
use an external RF model when physical link-budget accuracy is required.

This boundary follows the ITU's receiver-effect definition of interference,
which covers accidental and intentional causes, and NIST findings that
interference time scale can materially change link performance. The simple
rectangular overlap calculation replaces the emitter-spectrum and receiver-
filter convolution used in physical RF studies. See the
[ITU radio-interference overview](https://www.itu.int/en/mediacentre/backgrounders/Pages/radio-interference.aspx),
[NIST time-scale study](https://www.nist.gov/publications/assessing-time-scale-dependent-interference-vulnerabilities-wireless-communications),
and [NTIA interference assessment](https://www.ntia.gov/sites/default/files/publications/etdocket03-108appendixa_02152005_0.pdf).

## Model boundaries

Packets remain abstract byte payloads. Optional wire overhead contributes to
serialization, queue capacity, and MTU checks; the engine does not synthesize
protocol headers. Shared media model deterministic serialization contention,
without collisions, carrier sensing, retransmission, or physical RF contention.
There is no automatic fragmentation, dynamic routing, switch learning,
multicast, obstruction, or device processing delay. Device roles are mutually
exclusive built-in variants.

## Minimum supported Rust version

`c3mesh` requires Rust 1.85 or newer.

Continuous integration checks Rust 1.85.0 and current stable on Linux and Windows. The workflow covers formatting, strict lints, all-feature and default-feature tests, the bounded-queue example, and a package dry run. Action revisions are pinned to immutable commits; see [.github/workflows/ci.yml](.github/workflows/ci.yml).

When this checkout is used as a named Docker build context by a consumer, [.dockerignore](.dockerignore) excludes local Cargo output, Git history, tool state, and environment files. Source, manifests, examples, and test fixtures remain available. This prevents accumulated build artifacts from being transferred with a source dependency.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
