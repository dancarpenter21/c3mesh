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
        },
        DeviceConfig {
            id: "receiver".into(),
            kind: DeviceKind::Sink,
            mobility: Default::default(),
        },
    ],
    channels: vec![ChannelConfig {
        id: "link".into(),
        endpoints: ["sender".into(), "receiver".into()],
        bit_rate_bps: 1_000_000,
        propagation_delay_ns: 10_000_000,
        state: ChannelState::Operational,
        distance: None,
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

All time is virtual and measured in integer nanoseconds. Runs perform no
wall-clock sleeping and are deterministic for a given topology and send order.

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
```

The examples cover direct delivery, static switch forwarding, routed multi-hop
delivery, and a moving aircraft radio link.

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

## Model boundaries

Version 0.1 models point-to-point links and abstract packets whose transmitted
size is their payload length. It does not synthesize protocol headers or model
Ethernet/IP details, dynamic routing, switch learning, multicast, random loss,
wireless contention, obstruction, or device processing delay. Device roles are
mutually exclusive built-in variants.

## Minimum supported Rust version

`c3mesh` requires Rust 1.85 or newer.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
