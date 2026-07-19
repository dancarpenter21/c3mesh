//! End-to-end behavioral tests for the simulator.

use c3mesh::{
    ChannelConfig, ChannelState, ConfigError, DeviceConfig, DeviceId, DeviceKind, DropReason,
    NetworkConfig, NetworkEvent, SimTime, SimulationError, Simulator,
};
use std::collections::BTreeMap;

fn direct_config(rate: u64, propagation_delay_ns: u64) -> NetworkConfig {
    NetworkConfig {
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
            endpoints: ["source".into(), "sink".into()],
            bit_rate_bps: rate,
            propagation_delay_ns,
            state: ChannelState::Operational,
            distance: None,
        }],
    }
}

#[test]
fn receive_fires_after_serialization_and_propagation() {
    let mut simulator = Simulator::new(direct_config(1_000_000, 10_000_000)).unwrap();
    simulator.send("source", "sink", vec![0; 1_000]).unwrap();
    let events = simulator.run().unwrap();

    assert!(matches!(
        events.as_slice(),
        [
            NetworkEvent::TransmissionStarted {
                at,
                receive_at,
                ..
            },
            NetworkEvent::DataReceived { at: received, .. },
            NetworkEvent::PacketDelivered { at: delivered, .. }
        ] if *at == SimTime::ZERO
            && *receive_at == SimTime::from_nanos(18_000_000)
            && received == receive_at
            && delivered == receive_at
    ));
}

#[test]
fn serialization_rounds_up_and_same_direction_is_fifo() {
    let mut simulator = Simulator::new(direct_config(3_000_000_000, 7)).unwrap();
    simulator.send("source", "sink", vec![0; 1]).unwrap();
    simulator.send("source", "sink", vec![0; 1]).unwrap();
    let starts: Vec<_> = simulator
        .run()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            NetworkEvent::TransmissionStarted { at, receive_at, .. } => Some((at, receive_at)),
            _ => None,
        })
        .collect();

    assert_eq!(
        starts,
        vec![
            (SimTime::ZERO, SimTime::from_nanos(10)),
            (SimTime::from_nanos(3), SimTime::from_nanos(13)),
        ]
    );
}

#[test]
fn opposite_directions_serialize_independently() {
    let config = NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: "left".into(),
                kind: DeviceKind::Source {
                    egress: "link".into(),
                },
                mobility: Default::default(),
            },
            DeviceConfig {
                id: "right".into(),
                kind: DeviceKind::Source {
                    egress: "link".into(),
                },
                mobility: Default::default(),
            },
        ],
        channels: vec![ChannelConfig {
            id: "link".into(),
            endpoints: ["left".into(), "right".into()],
            bit_rate_bps: 1_000_000,
            propagation_delay_ns: 0,
            state: ChannelState::Operational,
            distance: None,
        }],
    };
    let mut simulator = Simulator::new(config).unwrap();
    simulator.send("left", "right", vec![0; 100]).unwrap();
    simulator.send("right", "left", vec![0; 100]).unwrap();
    let starts: Vec<_> = simulator
        .run()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            NetworkEvent::TransmissionStarted { at, .. } => Some(at),
            _ => None,
        })
        .collect();
    assert_eq!(starts, vec![SimTime::ZERO, SimTime::ZERO]);
}

#[test]
fn switch_forwards_only_after_complete_receive() {
    let mut forwarding = BTreeMap::new();
    forwarding.insert(DeviceId::from("sink"), "out".into());
    let config = NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: "source".into(),
                kind: DeviceKind::Source {
                    egress: "in".into(),
                },
                mobility: Default::default(),
            },
            DeviceConfig {
                id: "switch".into(),
                kind: DeviceKind::Switch { forwarding },
                mobility: Default::default(),
            },
            DeviceConfig {
                id: "sink".into(),
                kind: DeviceKind::Sink,
                mobility: Default::default(),
            },
        ],
        channels: vec![
            ChannelConfig {
                id: "in".into(),
                endpoints: ["source".into(), "switch".into()],
                bit_rate_bps: 8_000_000,
                propagation_delay_ns: 10,
                state: ChannelState::Operational,
                distance: None,
            },
            ChannelConfig {
                id: "out".into(),
                endpoints: ["switch".into(), "sink".into()],
                bit_rate_bps: 8_000_000,
                propagation_delay_ns: 10,
                state: ChannelState::Operational,
                distance: None,
            },
        ],
    };
    let mut simulator = Simulator::new(config).unwrap();
    simulator.send("source", "sink", vec![0; 1]).unwrap();
    let events = simulator.run().unwrap();
    let starts: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            NetworkEvent::TransmissionStarted { at, .. } => Some(*at),
            _ => None,
        })
        .collect();
    assert_eq!(starts, vec![SimTime::ZERO, SimTime::from_nanos(1_010)]);
    assert!(matches!(
        events.last(),
        Some(NetworkEvent::PacketDelivered { at, .. }) if *at == SimTime::from_nanos(2_020)
    ));
}

#[test]
fn router_uses_default_route() {
    let config = routed_config(BTreeMap::new(), Some("out".into()));
    let mut simulator = Simulator::new(config).unwrap();
    simulator.send("source", "sink", b"x".to_vec()).unwrap();
    assert!(
        simulator
            .run()
            .unwrap()
            .iter()
            .any(|event| matches!(event, NetworkEvent::PacketDelivered { .. }))
    );
}

#[test]
fn missing_rule_and_hop_exhaustion_are_drop_events() {
    let mut simulator = Simulator::new(routed_config(BTreeMap::new(), None)).unwrap();
    simulator.send("source", "sink", b"x".to_vec()).unwrap();
    assert!(simulator.run().unwrap().iter().any(|event| matches!(
        event,
        NetworkEvent::PacketDropped {
            reason: DropReason::NoForwardingRule,
            ..
        }
    )));

    let mut routes = BTreeMap::new();
    routes.insert(DeviceId::from("sink"), "out".into());
    let mut simulator = Simulator::new(routed_config(routes, None)).unwrap();
    simulator
        .schedule_send_with_hop_limit(SimTime::ZERO, "source", "sink", b"x".to_vec(), 0)
        .unwrap();
    assert!(simulator.run().unwrap().iter().any(|event| matches!(
        event,
        NetworkEvent::PacketDropped {
            reason: DropReason::HopLimitExceeded,
            ..
        }
    )));
}

#[test]
fn degraded_and_severed_states_affect_new_transmissions() {
    let mut simulator = Simulator::new(direct_config(8_000_000, 0)).unwrap();
    simulator
        .set_channel_state(
            "link",
            ChannelState::Degraded {
                effective_bit_rate_bps: 4_000_000,
            },
        )
        .unwrap();
    simulator.send("source", "sink", vec![0; 1]).unwrap();
    assert!(matches!(
        simulator.run().unwrap().first(),
        Some(NetworkEvent::TransmissionStarted { receive_at, .. })
            if *receive_at == SimTime::from_nanos(2_000)
    ));

    let mut simulator = Simulator::new(direct_config(8_000_000, 0)).unwrap();
    simulator
        .set_channel_state("link", ChannelState::Severed)
        .unwrap();
    simulator.send("source", "sink", vec![0; 1]).unwrap();
    assert!(matches!(
        simulator.run().unwrap().as_slice(),
        [NetworkEvent::PacketDropped {
            reason: DropReason::ChannelSevered { .. },
            ..
        }]
    ));
}

#[test]
fn in_flight_arrival_is_not_rescheduled_by_state_change() {
    let mut simulator = Simulator::new(direct_config(8_000_000, 100)).unwrap();
    simulator.send("source", "sink", vec![0; 1]).unwrap();
    let first = simulator.step().unwrap().unwrap();
    assert!(matches!(
        first,
        NetworkEvent::TransmissionStarted {
            receive_at,
            ..
        } if receive_at == SimTime::from_nanos(1_100)
    ));
    simulator
        .set_channel_state("link", ChannelState::Severed)
        .unwrap();
    assert!(simulator.run().unwrap().iter().any(|event| matches!(
        event,
        NetworkEvent::PacketDelivered { at, .. } if *at == SimTime::from_nanos(1_100)
    )));
}

#[test]
fn callback_and_scheduling_errors_work() {
    let mut simulator = Simulator::new(direct_config(8_000_000, 1)).unwrap();
    simulator.send("source", "sink", Vec::new()).unwrap();
    let mut observed = Vec::new();
    simulator
        .run_with(|event| observed.push(event.time()))
        .unwrap();
    assert_eq!(
        observed,
        vec![
            SimTime::ZERO,
            SimTime::from_nanos(1),
            SimTime::from_nanos(1),
        ]
    );
    assert_eq!(
        simulator.schedule_send(SimTime::ZERO, "source", "sink", Vec::new()),
        Err(SimulationError::TimeInPast)
    );
}

#[test]
fn invalid_topologies_are_rejected() {
    let mut config = direct_config(0, 0);
    assert_eq!(
        config.validate(),
        Err(ConfigError::ZeroBitRate("link".into()))
    );
    assert!(matches!(
        Simulator::new(config),
        Err(SimulationError::InvalidConfig(ConfigError::ZeroBitRate(_)))
    ));

    config = direct_config(1, 0);
    config.channels[0].endpoints = ["source".into(), "source".into()];
    assert!(matches!(
        config.validate(),
        Err(ConfigError::SelfConnectedChannel(_))
    ));

    config = direct_config(10, 0);
    config.channels[0].state = ChannelState::Degraded {
        effective_bit_rate_bps: 11,
    };
    assert!(matches!(
        config.validate(),
        Err(ConfigError::InvalidDegradedRate(_))
    ));

    config = direct_config(10, 0);
    if let DeviceKind::Source { egress } = &mut config.devices[0].kind {
        *egress = "missing".into();
    }
    assert_eq!(
        config.validate(),
        Err(ConfigError::UnknownChannel("missing".into()))
    );
}

#[test]
fn invalid_api_operations_return_errors() {
    let mut simulator = Simulator::new(direct_config(10, 0)).unwrap();
    assert_eq!(
        simulator.send("sink", "source", Vec::new()),
        Err(SimulationError::NotASource("sink".into()))
    );
    assert_eq!(
        simulator.send("missing", "sink", Vec::new()),
        Err(SimulationError::UnknownDevice("missing".into()))
    );
    assert_eq!(
        simulator.set_channel_state(
            "link",
            ChannelState::Degraded {
                effective_bit_rate_bps: 11,
            },
        ),
        Err(SimulationError::InvalidChannelState("link".into()))
    );
}

#[test]
fn checked_timing_reports_overflow() {
    let mut simulator = Simulator::new(direct_config(8, u64::MAX)).unwrap();
    simulator.send("source", "sink", vec![0]).unwrap();
    assert_eq!(simulator.step(), Err(SimulationError::TimeOverflow));
}

fn routed_config(
    routes: BTreeMap<DeviceId, c3mesh::ChannelId>,
    default_route: Option<c3mesh::ChannelId>,
) -> NetworkConfig {
    NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: "source".into(),
                kind: DeviceKind::Source {
                    egress: "in".into(),
                },
                mobility: Default::default(),
            },
            DeviceConfig {
                id: "router".into(),
                kind: DeviceKind::Router {
                    routes,
                    default_route,
                },
                mobility: Default::default(),
            },
            DeviceConfig {
                id: "sink".into(),
                kind: DeviceKind::Sink,
                mobility: Default::default(),
            },
        ],
        channels: vec![
            ChannelConfig {
                id: "in".into(),
                endpoints: ["source".into(), "router".into()],
                bit_rate_bps: 8_000_000,
                propagation_delay_ns: 0,
                state: ChannelState::Operational,
                distance: None,
            },
            ChannelConfig {
                id: "out".into(),
                endpoints: ["router".into(), "sink".into()],
                bit_rate_bps: 8_000_000,
                propagation_delay_ns: 0,
                state: ChannelState::Operational,
                distance: None,
            },
        ],
    }
}
