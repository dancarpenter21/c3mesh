//! Demonstrates bounded command-radio queues without advancing beyond a game tick.
use c3mesh::{
    ChannelConfig, ChannelOptions, ChannelState, DeviceConfig, DeviceKind, DropReason,
    NetworkConfig, NetworkEvent, PacketMetadata, QueueConfig, QueueDiscipline, SimTime, Simulator,
    SimulatorOptions,
};
use std::collections::BTreeMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let network = NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: "sender".into(),
                kind: DeviceKind::Source {
                    egress: "radio".into(),
                },
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
            id: "radio".into(),
            endpoints: ["sender".into(), "receiver".into()],
            bit_rate_bps: 8_000,
            propagation_delay_ns: 1_000_000,
            state: ChannelState::Operational,
            distance: None,
            radio: None,
        }],
    };
    let mut simulator = Simulator::new_with_options(
        network,
        SimulatorOptions {
            seed: 42,
            channels: BTreeMap::from([(
                "radio".into(),
                ChannelOptions {
                    mtu_bytes: Some(128),
                    wire_overhead_bytes: 20,
                    queue: QueueConfig {
                        max_packets: Some(2),
                        max_bytes: Some(256),
                        discipline: QueueDiscipline::StrictPriority,
                    },
                    ..Default::default()
                },
            )]),
        },
    )?;
    simulator.send("sender", "receiver", vec![0; 100])?;
    simulator.schedule_send_with_metadata(
        SimTime::from_nanos(1),
        "sender",
        "receiver",
        b"old contact".to_vec(),
        PacketMetadata {
            priority: 1,
            expires_at: Some(SimTime::from_nanos(50_000_000)),
            ..Default::default()
        },
    )?;
    simulator.schedule_send_with_metadata(
        SimTime::from_nanos(2),
        "sender",
        "receiver",
        b"background".to_vec(),
        PacketMetadata {
            priority: 1,
            ..Default::default()
        },
    )?;
    let urgent = simulator.schedule_send_with_metadata(
        SimTime::from_nanos(3),
        "sender",
        "receiver",
        b"move".to_vec(),
        PacketMetadata {
            priority: 230,
            ..Default::default()
        },
    )?;
    let mut events = simulator.advance_to(SimTime::from_nanos(10_000_000))?;
    let metrics = simulator.channel_queue_metrics("radio")?;
    assert_eq!(metrics.packets_0_to_1, 2);
    println!(
        "At 10 ms: {} waiting packets, {} wire bytes",
        metrics.packets_0_to_1, metrics.bytes_0_to_1
    );
    events.extend(simulator.run()?);
    assert!(events.iter().any(|event| matches!(
        event,
        NetworkEvent::PacketDropped {
            reason: DropReason::Expired,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(event,
        NetworkEvent::PacketDelivered { packet, .. } if packet.id() == urgent
    )));
    for event in events {
        println!("{event:?}");
    }
    Ok(())
}
