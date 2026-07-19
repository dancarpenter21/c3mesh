//! Demonstrates timing a direct source-to-sink transmission.

use c3mesh::{
    ChannelConfig, ChannelState, DeviceConfig, DeviceId, DeviceKind, NetworkConfig, NetworkEvent,
    SimTime, Simulator,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: "source".into(),
                kind: DeviceKind::Source {
                    egress: "link".into(),
                },
                mobility: Default::default(),
            },
            DeviceConfig {
                id: "sink".into(),
                kind: DeviceKind::Sink,
                mobility: Default::default(),
            },
        ],
        channels: vec![ChannelConfig {
            id: "link".into(),
            endpoints: [DeviceId::from("source"), DeviceId::from("sink")],
            bit_rate_bps: 1_000_000,
            propagation_delay_ns: 10_000_000,
            state: ChannelState::Operational,
            distance: None,
        }],
    };

    let mut simulator = Simulator::new(config)?;
    simulator.send("source", "sink", vec![0; 1_000])?;
    let events = simulator.run()?;

    let delivered_at = events.iter().find_map(|event| match event {
        NetworkEvent::PacketDelivered { at, .. } => Some(*at),
        _ => None,
    });
    assert_eq!(delivered_at, Some(SimTime::from_nanos(18_000_000)));
    println!("1,000 bytes delivered in {}", delivered_at.unwrap());
    Ok(())
}
