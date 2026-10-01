//! Packet-engine behavior under congestion, physical limits, and virtual-time boundaries.
use c3mesh::{
    ChannelConfig, ChannelOptions, ChannelState, ConfigError, DeviceConfig, DeviceKind, DropReason,
    FrequencyBand, InterferenceResponse, NetworkConfig, NetworkEvent, PacketMetadata, QueueConfig,
    QueueDiscipline, RadioChannel, ReceiverInterference, SimTime, SimulationError, Simulator,
    SimulatorOptions,
};
use std::collections::BTreeMap;

fn time(ns: u64) -> SimTime {
    SimTime::from_nanos(ns)
}

fn topology() -> NetworkConfig {
    NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: "source".into(),
                kind: DeviceKind::Source {
                    egress: "link".into(),
                },
                mobility: Default::default(),
                interference: vec![],
            },
            DeviceConfig {
                id: "sink".into(),
                kind: DeviceKind::Sink,
                mobility: Default::default(),
                interference: vec![],
            },
        ],
        channels: vec![ChannelConfig {
            id: "link".into(),
            endpoints: ["source".into(), "sink".into()],
            bit_rate_bps: 8_000_000_000,
            propagation_delay_ns: 0,
            state: ChannelState::Operational,
            distance: None,
            radio: None,
        }],
    }
}

fn simulator(options: ChannelOptions) -> Simulator {
    Simulator::new_with_options(
        topology(),
        SimulatorOptions {
            seed: 42,
            channels: BTreeMap::from([("link".into(), options)]),
        },
    )
    .unwrap()
}

fn send(simulator: &mut Simulator, at: u64, bytes: usize, priority: u8, flow_id: u64) -> u64 {
    simulator
        .schedule_send_with_metadata(
            time(at),
            "source",
            "sink",
            vec![0; bytes],
            PacketMetadata {
                priority,
                flow_id,
                ..Default::default()
            },
        )
        .unwrap()
        .get()
}

fn starts(events: &[NetworkEvent]) -> Vec<(u64, u64)> {
    events
        .iter()
        .filter_map(|event| match event {
            NetworkEvent::TransmissionStarted { at, packet, .. } => {
                Some((packet.id().get(), at.as_nanos()))
            }
            _ => None,
        })
        .collect()
}

fn drops(events: &[NetworkEvent]) -> Vec<(u64, u64, DropReason)> {
    events
        .iter()
        .filter_map(|event| match event {
            NetworkEvent::PacketDropped {
                at, packet, reason, ..
            } => Some((packet.id().get(), at.as_nanos(), reason.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn advance_stops_before_future_internal_injection_and_includes_equal_timestamps() {
    for mut simulator in [
        Simulator::new(topology()).unwrap(),
        simulator(Default::default()),
    ] {
        simulator
            .schedule_send(time(100), "source", "sink", vec![0; 10])
            .unwrap();
        assert!(simulator.advance_to(time(99)).unwrap().is_empty());
        assert_eq!(simulator.now(), time(99));
        assert_eq!(
            starts(&simulator.advance_to(time(100)).unwrap()),
            vec![(0, 100)]
        );
        assert!(simulator.advance_to(time(109)).unwrap().is_empty());
        let arrivals = simulator.advance_to(time(110)).unwrap();
        assert!(matches!(arrivals.as_slice(), [
            NetworkEvent::DataReceived { at, .. }, NetworkEvent::PacketDelivered { .. }
        ] if *at == time(110)));
        assert_eq!(simulator.now(), time(110));
        assert_eq!(
            simulator.advance_to(time(109)),
            Err(SimulationError::TimeInPast)
        );
    }
}

#[test]
fn zero_length_packets_drain_every_event_at_the_boundary() {
    let mut simulator = simulator(Default::default());
    for _ in 0..4 {
        simulator.send("source", "sink", vec![]).unwrap();
    }
    let events = simulator.advance_to(SimTime::ZERO).unwrap();
    assert_eq!(events.len(), 12);
    assert!(simulator.step().unwrap().is_none());
    assert_eq!(simulator.now(), SimTime::ZERO);
}

#[test]
fn queue_packet_bound_excludes_in_flight_and_releases_after_dequeue() {
    let mut simulator = simulator(ChannelOptions {
        queue: QueueConfig {
            max_packets: Some(1),
            ..Default::default()
        },
        ..Default::default()
    });
    let active = send(&mut simulator, 0, 100, 0, 0);
    let waiting = send(&mut simulator, 1, 10, 0, 0);
    let overflow = send(&mut simulator, 1, 10, 0, 0);
    let first = simulator.advance_to(time(1)).unwrap();
    assert_eq!(starts(&first), vec![(active, 0)]);
    assert_eq!(
        drops(&first),
        vec![(
            overflow,
            1,
            DropReason::QueueOverflow {
                channel: "link".into()
            }
        )]
    );
    let metrics = simulator.channel_queue_metrics("link").unwrap();
    assert_eq!((metrics.packets_0_to_1, metrics.bytes_0_to_1), (1, 10));
    assert_eq!((metrics.packets_1_to_0, metrics.bytes_1_to_0), (0, 0));
    let rest = simulator.run().unwrap();
    assert_eq!(starts(&rest), vec![(waiting, 100)]);
    assert_eq!(
        simulator
            .channel_queue_metrics("link")
            .unwrap()
            .packets_0_to_1,
        0
    );
    assert!(matches!(
        simulator.channel_queue_metrics("missing"),
        Err(SimulationError::UnknownChannel(_))
    ));
}

#[test]
fn byte_capacity_and_mtu_include_wire_overhead() {
    let mut simulator = simulator(ChannelOptions {
        wire_overhead_bytes: 5,
        mtu_bytes: Some(20),
        queue: QueueConfig {
            max_bytes: Some(20),
            ..Default::default()
        },
        ..Default::default()
    });
    send(&mut simulator, 0, 15, 0, 0);
    let fits = send(&mut simulator, 1, 15, 0, 0);
    let too_full = send(&mut simulator, 1, 1, 0, 0);
    let too_large = send(&mut simulator, 1, 16, 0, 0);
    let events = simulator.advance_to(time(1)).unwrap();
    assert_eq!(
        simulator
            .channel_queue_metrics("link")
            .unwrap()
            .bytes_0_to_1,
        20
    );
    assert_eq!(
        drops(&events),
        vec![
            (
                too_full,
                1,
                DropReason::QueueOverflow {
                    channel: "link".into()
                }
            ),
            (
                too_large,
                1,
                DropReason::MtuExceeded {
                    channel: "link".into(),
                    mtu_bytes: 20,
                    wire_bytes: 21
                }
            ),
        ]
    );
    assert_eq!(starts(&simulator.run().unwrap()), vec![(fits, 20)]);
    assert_eq!(simulator.now(), time(40));
}

#[test]
fn strict_priority_preserves_active_packet_and_fifo_within_priority() {
    let mut simulator = simulator(ChannelOptions {
        queue: QueueConfig {
            discipline: QueueDiscipline::StrictPriority,
            ..Default::default()
        },
        ..Default::default()
    });
    let active = send(&mut simulator, 0, 100, 1, 0);
    let low = send(&mut simulator, 1, 10, 1, 0);
    let high = send(&mut simulator, 2, 10, 240, 0);
    let equal = send(&mut simulator, 3, 10, 240, 0);
    assert_eq!(
        starts(&simulator.run().unwrap()),
        vec![(active, 0), (high, 100), (equal, 110), (low, 120),]
    );
}

#[test]
fn priority_eviction_targets_lowest_priority_and_newest_tie() {
    let mut simulator = simulator(ChannelOptions {
        queue: QueueConfig {
            max_packets: Some(2),
            discipline: QueueDiscipline::StrictPriority,
            ..Default::default()
        },
        ..Default::default()
    });
    send(&mut simulator, 0, 100, 0, 0);
    let older = send(&mut simulator, 1, 10, 1, 0);
    let newer = send(&mut simulator, 2, 10, 1, 0);
    let urgent = send(&mut simulator, 3, 10, 230, 0);
    let events = simulator.run().unwrap();
    assert_eq!(
        drops(&events),
        vec![(
            newer,
            3,
            DropReason::QueueOverflow {
                channel: "link".into()
            }
        )]
    );
    assert_eq!(&starts(&events)[1..], &[(urgent, 100), (older, 110)]);
}

#[test]
fn an_unadmittable_priority_packet_does_not_evict_others() {
    let mut simulator = simulator(ChannelOptions {
        queue: QueueConfig {
            max_bytes: Some(10),
            discipline: QueueDiscipline::StrictPriority,
            ..Default::default()
        },
        ..Default::default()
    });
    send(&mut simulator, 0, 10, 0, 0);
    let retained = send(&mut simulator, 1, 8, 240, 0);
    let low = send(&mut simulator, 1, 2, 1, 0);
    let rejected = send(&mut simulator, 2, 9, 230, 0);
    let events = simulator.run().unwrap();
    assert_eq!(
        drops(&events),
        vec![(
            rejected,
            2,
            DropReason::QueueOverflow {
                channel: "link".into()
            }
        )]
    );
    assert_eq!(&starts(&events)[1..], &[(retained, 10), (low, 18)]);
}

#[test]
fn weighted_fair_serves_backlogged_classes_in_proportion_to_weights() {
    let mut simulator = simulator(ChannelOptions {
        queue: QueueConfig {
            discipline: QueueDiscipline::WeightedFair,
            ..Default::default()
        },
        traffic_class_weights: BTreeMap::from([(2, 4)]),
        ..Default::default()
    });
    send(&mut simulator, 0, 100, 0, 99);
    for _ in 0..16 {
        simulator
            .schedule_send_with_metadata(
                time(1),
                "source",
                "sink",
                vec![0; 10],
                PacketMetadata {
                    traffic_class: 0,
                    flow_id: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        simulator
            .schedule_send_with_metadata(
                time(1),
                "source",
                "sink",
                vec![0; 10],
                PacketMetadata {
                    traffic_class: 2,
                    flow_id: 2,
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let events = simulator.run().unwrap();
    let classes: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            NetworkEvent::TransmissionStarted { packet, .. } => {
                Some(packet.metadata().traffic_class)
            }
            _ => None,
        })
        .skip(1)
        .take(10)
        .collect();
    assert_eq!(classes.iter().filter(|class| **class == 2).count(), 8);
    assert_eq!(classes.iter().filter(|class| **class == 0).count(), 2);
    assert!(drops(&events).is_empty());
    assert_eq!(starts(&events).len(), 33);
}

#[test]
fn weighted_fair_uses_wire_size_and_keeps_each_flow_in_order() {
    let mut simulator = simulator(ChannelOptions {
        queue: QueueConfig {
            discipline: QueueDiscipline::WeightedFair,
            ..Default::default()
        },
        ..Default::default()
    });
    send(&mut simulator, 0, 100, 0, 99);
    let large = send(&mut simulator, 1, 20, 0, 1);
    let small_same_flow = send(&mut simulator, 1, 1, 0, 1);
    let small_other_flow = send(&mut simulator, 1, 1, 0, 2);
    assert_eq!(
        &starts(&simulator.run().unwrap())[1..],
        &[
            (small_other_flow, 100),
            (large, 101),
            (small_same_flow, 121),
        ]
    );
}

fn two_links() -> NetworkConfig {
    let mut config = topology();
    let mut second = topology();
    second.devices[0].id = "source2".into();
    second.devices[0].kind = DeviceKind::Source {
        egress: "link2".into(),
    };
    second.devices[1].id = "sink2".into();
    second.channels[0].id = "link2".into();
    second.channels[0].endpoints = ["source2".into(), "sink2".into()];
    config.devices.extend(second.devices);
    config.channels.extend(second.channels);
    config
}

#[test]
fn shared_medium_serializes_links_but_does_not_block_for_propagation() {
    let mut config = two_links();
    for channel in &mut config.channels {
        channel.propagation_delay_ns = 1_000;
    }
    let options = ChannelOptions {
        shared_medium: Some("rf".into()),
        ..Default::default()
    };
    let mut simulator = Simulator::new_with_options(
        config,
        SimulatorOptions {
            channels: BTreeMap::from([("link".into(), options.clone()), ("link2".into(), options)]),
            ..Default::default()
        },
    )
    .unwrap();
    simulator.send("source", "sink", vec![0; 100]).unwrap();
    simulator.send("source2", "sink2", vec![0; 100]).unwrap();
    assert_eq!(starts(&simulator.run().unwrap()), vec![(0, 0), (1, 100)]);
    assert_eq!(simulator.now(), time(1_200));
}

#[test]
fn different_media_serialize_independently() {
    let mut simulator = Simulator::new_with_options(
        two_links(),
        SimulatorOptions {
            channels: BTreeMap::from([
                (
                    "link".into(),
                    ChannelOptions {
                        shared_medium: Some("a".into()),
                        ..Default::default()
                    },
                ),
                (
                    "link2".into(),
                    ChannelOptions {
                        shared_medium: Some("b".into()),
                        ..Default::default()
                    },
                ),
            ]),
            ..Default::default()
        },
    )
    .unwrap();
    simulator.send("source", "sink", vec![0; 100]).unwrap();
    simulator.send("source2", "sink2", vec![0; 100]).unwrap();
    assert_eq!(starts(&simulator.run().unwrap()), vec![(0, 0), (1, 0)]);
}

#[test]
fn shared_medium_also_serializes_opposite_directions() {
    let mut config = topology();
    config.devices[1].kind = DeviceKind::Source {
        egress: "link".into(),
    };
    let mut simulator = Simulator::new_with_options(
        config,
        SimulatorOptions {
            channels: BTreeMap::from([(
                "link".into(),
                ChannelOptions {
                    shared_medium: Some("rf".into()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    )
    .unwrap();
    simulator.send("source", "sink", vec![0; 100]).unwrap();
    simulator.send("sink", "source", vec![0; 100]).unwrap();
    simulator.advance_to(time(1)).unwrap();
    assert_eq!(
        simulator
            .channel_queue_metrics("link")
            .unwrap()
            .packets_1_to_0,
        1
    );
    assert_eq!(starts(&simulator.run().unwrap()), vec![(1, 100)]);
}

#[test]
fn expired_queued_packet_frees_capacity_at_its_deadline() {
    let mut simulator = simulator(ChannelOptions {
        queue: QueueConfig {
            max_packets: Some(1),
            ..Default::default()
        },
        ..Default::default()
    });
    send(&mut simulator, 0, 100, 0, 0);
    let expired = simulator
        .schedule_send_with_metadata(
            time(1),
            "source",
            "sink",
            vec![0; 10],
            PacketMetadata {
                expires_at: Some(time(20)),
                ..Default::default()
            },
        )
        .unwrap()
        .get();
    simulator.advance_to(time(1)).unwrap();
    assert_eq!(
        simulator
            .channel_queue_metrics("link")
            .unwrap()
            .packets_0_to_1,
        1
    );
    assert_eq!(
        drops(&simulator.advance_to(time(20)).unwrap()),
        vec![(expired, 20, DropReason::Expired)]
    );
    assert_eq!(
        simulator
            .channel_queue_metrics("link")
            .unwrap()
            .packets_0_to_1,
        0
    );
    let admitted = send(&mut simulator, 21, 10, 0, 0);
    assert_eq!(starts(&simulator.run().unwrap()), vec![(admitted, 100)]);
}

#[test]
fn newly_admitted_earlier_expiry_reschedules_a_pending_drain() {
    let mut simulator = simulator(Default::default());
    send(&mut simulator, 0, 100, 0, 0);
    simulator
        .schedule_send_with_metadata(
            time(1),
            "source",
            "sink",
            vec![0; 10],
            PacketMetadata {
                expires_at: Some(time(80)),
                ..Default::default()
            },
        )
        .unwrap();
    let earlier = simulator
        .schedule_send_with_metadata(
            time(2),
            "source",
            "sink",
            vec![0; 10],
            PacketMetadata {
                expires_at: Some(time(20)),
                ..Default::default()
            },
        )
        .unwrap()
        .get();
    assert_eq!(
        drops(&simulator.advance_to(time(20)).unwrap()),
        vec![(earlier, 20, DropReason::Expired)]
    );
    assert_eq!(
        simulator
            .channel_queue_metrics("link")
            .unwrap()
            .packets_0_to_1,
        1
    );
}

#[test]
fn exclusive_expiry_prevents_in_flight_delivery_and_already_expired_injection() {
    let mut simulator = simulator(Default::default());
    let in_flight = simulator
        .schedule_send_with_metadata(
            time(0),
            "source",
            "sink",
            vec![0; 10],
            PacketMetadata {
                expires_at: Some(time(10)),
                ..Default::default()
            },
        )
        .unwrap()
        .get();
    let already_expired = simulator
        .schedule_send_with_metadata(
            time(20),
            "source",
            "sink",
            vec![0; 10],
            PacketMetadata {
                expires_at: Some(time(15)),
                ..Default::default()
            },
        )
        .unwrap()
        .get();
    let events = simulator.run().unwrap();
    assert_eq!(starts(&events), vec![(in_flight, 0)]);
    assert_eq!(
        drops(&events),
        vec![
            (in_flight, 10, DropReason::Expired),
            (already_expired, 20, DropReason::Expired)
        ]
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, NetworkEvent::PacketDelivered { .. }))
    );
}

#[test]
fn queued_packet_observes_state_changes_before_actual_start() {
    let mut simulator = simulator(Default::default());
    send(&mut simulator, 0, 100, 0, 0);
    let waiting = send(&mut simulator, 1, 10, 0, 0);
    simulator.advance_to(time(1)).unwrap();
    simulator
        .set_channel_state("link", ChannelState::Severed)
        .unwrap();
    let events = simulator.run().unwrap();
    assert!(starts(&events).is_empty());
    assert_eq!(
        drops(&events),
        vec![(
            waiting,
            100,
            DropReason::ChannelSevered {
                channel: "link".into()
            }
        )]
    );
    assert!(events.iter().any(|event| matches!(event, NetworkEvent::PacketDelivered { packet, .. } if packet.id().get() == 0)));
}

#[test]
fn queued_packet_observes_runtime_receiver_jamming() {
    let mut config = topology();
    let band = FrequencyBand::new(100, 200);
    config.channels[0].radio = Some(RadioChannel {
        band,
        interference_response: InterferenceResponse::default(),
    });
    let mut simulator = Simulator::new_with_options(
        config,
        SimulatorOptions {
            channels: BTreeMap::from([("link".into(), ChannelOptions::default())]),
            ..Default::default()
        },
    )
    .unwrap();
    send(&mut simulator, 0, 100, 0, 0);
    let waiting = send(&mut simulator, 1, 10, 0, 0);
    simulator.advance_to(time(1)).unwrap();
    simulator
        .set_receiver_interference("sink", vec![ReceiverInterference { band, jammed: 1.0 }])
        .unwrap();
    let events = simulator.run().unwrap();
    assert!(starts(&events).is_empty());
    assert_eq!(drops(&events).len(), 2);
    assert!(drops(&events).iter().any(|(id, at, reason)| *id == waiting
        && *at == 100
        && matches!(reason, DropReason::ReceiverInterference { .. })));
}

fn lossy_run(seed: u64, loss: u16, incremental: bool) -> Vec<NetworkEvent> {
    let mut simulator = Simulator::new_with_options(
        topology(),
        SimulatorOptions {
            seed,
            channels: BTreeMap::from([(
                "link".into(),
                ChannelOptions {
                    loss_basis_points: loss,
                    ..Default::default()
                },
            )]),
        },
    )
    .unwrap();
    for _ in 0..100 {
        simulator.send("source", "sink", vec![0; 10]).unwrap();
    }
    if incremental {
        let mut events = Vec::new();
        for ns in 0..=1_000 {
            events.extend(simulator.advance_to(time(ns)).unwrap());
        }
        events
    } else {
        simulator.run().unwrap()
    }
}

#[test]
fn seeded_loss_is_reproducible_across_tick_sizes() {
    let first = lossy_run(42, 5_000, false);
    assert_eq!(first, lossy_run(42, 5_000, true));
    assert_eq!(first, lossy_run(42, 5_000, false));
    assert_ne!(drops(&first), drops(&lossy_run(43, 5_000, false)));
    assert!((25..=75).contains(&drops(&first).len()));
    assert_eq!(starts(&first).len(), 100);
}

#[test]
fn loss_extremes_are_exact_and_still_consume_wire_time() {
    let none = lossy_run(0, 0, false);
    assert!(drops(&none).is_empty());
    let all = lossy_run(0, 10_000, false);
    assert_eq!(drops(&all).len(), 100);
    assert_eq!(starts(&all).last(), Some(&(99, 990)));
    assert_eq!(
        drops(&all).last(),
        Some(&(
            99,
            1_000,
            DropReason::ChannelLoss {
                channel: "link".into()
            }
        ))
    );
}

#[test]
fn invalid_packet_engine_options_fail_at_construction() {
    let invalid = [
        ChannelOptions {
            mtu_bytes: Some(0),
            ..Default::default()
        },
        ChannelOptions {
            loss_basis_points: 10_001,
            ..Default::default()
        },
        ChannelOptions {
            shared_medium: Some("  ".into()),
            ..Default::default()
        },
        ChannelOptions {
            traffic_class_weights: BTreeMap::from([(0, 0)]),
            ..Default::default()
        },
        ChannelOptions {
            queue: QueueConfig {
                max_packets: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
        ChannelOptions {
            queue: QueueConfig {
                max_bytes: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
    ];
    for options in invalid {
        assert!(matches!(
            Simulator::new_with_options(
                topology(),
                SimulatorOptions {
                    channels: BTreeMap::from([("link".into(), options)]),
                    ..Default::default()
                }
            ),
            Err(SimulationError::InvalidConfig(
                ConfigError::InvalidSimulatorOptions(_)
            ))
        ));
    }
    assert!(matches!(
        Simulator::new_with_options(
            topology(),
            SimulatorOptions {
                channels: BTreeMap::from([("missing".into(), Default::default())]),
                ..Default::default()
            }
        ),
        Err(SimulationError::InvalidConfig(ConfigError::UnknownChannel(
            _
        )))
    ));
}

#[cfg(feature = "json")]
#[test]
fn options_and_metadata_round_trip_without_changing_old_packet_json() {
    let options = SimulatorOptions {
        channels: BTreeMap::from([(
            "link".into(),
            ChannelOptions {
                mtu_bytes: Some(1_500),
                queue: QueueConfig {
                    discipline: QueueDiscipline::WeightedFair,
                    ..Default::default()
                },
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let json = serde_json::to_string(&options).unwrap();
    assert!(json.contains("weighted_fair"));
    assert_eq!(
        serde_json::from_str::<SimulatorOptions>(&json).unwrap(),
        options
    );
    let mut simulator = Simulator::new(topology()).unwrap();
    simulator.send("source", "sink", vec![0; 1]).unwrap();
    let event = simulator.step().unwrap().unwrap();
    if let NetworkEvent::TransmissionStarted { packet, .. } = event {
        let json = serde_json::to_string(&packet).unwrap();
        assert!(!json.contains("metadata"));
        let old: c3mesh::Packet = serde_json::from_str(&json).unwrap();
        assert_eq!(old.metadata(), &PacketMetadata::default());
    } else {
        panic!("expected transmission start");
    }
}

#[test]
fn legacy_queue_metrics_exclude_wire_starts_at_the_current_time_between_events() {
    let mut simulator = Simulator::new(topology()).unwrap();
    for bytes in [100, 10, 0, 20] {
        simulator.send("source", "sink", vec![0; bytes]).unwrap();
    }
    assert_eq!(
        simulator
            .channel_queue_metrics("link")
            .unwrap()
            .packets_0_to_1,
        0
    );
    let mut events = simulator.advance_to(time(0)).unwrap();
    let metrics = simulator.channel_queue_metrics("link").unwrap();
    assert_eq!((metrics.packets_0_to_1, metrics.bytes_0_to_1), (3, 30));
    events.extend(simulator.advance_to(time(99)).unwrap());
    assert_eq!(simulator.channel_queue_metrics("link").unwrap(), metrics);

    // A receive event precedes the next wire-start event at this timestamp.
    let event = simulator.step().unwrap().unwrap();
    assert!(matches!(event, NetworkEvent::DataReceived { at, .. } if at == time(100)));
    events.push(event);
    let metrics = simulator.channel_queue_metrics("link").unwrap();
    assert_eq!((metrics.packets_0_to_1, metrics.bytes_0_to_1), (2, 20));
    while simulator.now() < time(110) {
        events.push(simulator.step().unwrap().unwrap());
    }
    let metrics = simulator.channel_queue_metrics("link").unwrap();
    assert_eq!((metrics.packets_0_to_1, metrics.bytes_0_to_1), (0, 0));
    events.extend(simulator.run().unwrap());
    assert_eq!(starts(&events), vec![(0, 0), (1, 100), (2, 110), (3, 110)]);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, NetworkEvent::PacketDelivered { .. }))
            .count(),
        4
    );
}

#[test]
fn legacy_queue_metrics_track_full_duplex_directions_independently() {
    let mut config = topology();
    config.devices[1].kind = DeviceKind::Source {
        egress: "link".into(),
    };
    let mut simulator = Simulator::new(config).unwrap();
    simulator.send("source", "sink", vec![0; 100]).unwrap();
    simulator.send("source", "sink", vec![0; 10]).unwrap();
    simulator.send("sink", "source", vec![0; 200]).unwrap();
    simulator.send("sink", "source", vec![0; 20]).unwrap();
    simulator.advance_to(time(0)).unwrap();
    let metrics = simulator.channel_queue_metrics("link").unwrap();
    assert_eq!((metrics.packets_0_to_1, metrics.bytes_0_to_1), (1, 10));
    assert_eq!((metrics.packets_1_to_0, metrics.bytes_1_to_0), (1, 20));
    simulator.advance_to(time(100)).unwrap();
    let metrics = simulator.channel_queue_metrics("link").unwrap();
    assert_eq!((metrics.packets_0_to_1, metrics.bytes_0_to_1), (0, 0));
    assert_eq!((metrics.packets_1_to_0, metrics.bytes_1_to_0), (1, 20));
    simulator.advance_to(time(200)).unwrap();
    let metrics = simulator.channel_queue_metrics("link").unwrap();
    assert_eq!(
        (
            metrics.packets_0_to_1,
            metrics.bytes_0_to_1,
            metrics.packets_1_to_0,
            metrics.bytes_1_to_0
        ),
        (0, 0, 0, 0)
    );
    simulator.run().unwrap();
    // A new busy period must not carry retired bytes into its queue count.
    simulator.send("source", "sink", vec![0; 20]).unwrap();
    simulator.send("source", "sink", vec![0; 30]).unwrap();
    simulator.advance_to(time(220)).unwrap();
    let metrics = simulator.channel_queue_metrics("link").unwrap();
    assert_eq!((metrics.packets_0_to_1, metrics.bytes_0_to_1), (1, 30));
}

#[test]
fn metadata_activation_preserves_legacy_reservations_and_clone_telemetry() {
    let mut simulator = Simulator::new(topology()).unwrap();
    simulator
        .schedule_send(time(0), "source", "sink", vec![0; 100])
        .unwrap();
    simulator
        .schedule_send(time(1), "source", "sink", vec![0; 20])
        .unwrap();
    let queued = send(&mut simulator, 2, 10, 9, 7);
    let mut events = simulator.advance_to(time(2)).unwrap();
    let metrics = simulator.channel_queue_metrics("link").unwrap();
    assert_eq!((metrics.packets_0_to_1, metrics.bytes_0_to_1), (2, 30));
    let consumed = events.len();
    let mut clone = simulator.clone();
    assert_eq!(clone.channel_queue_metrics("link").unwrap(), metrics);
    events.extend(simulator.advance_to(time(100)).unwrap());
    let metrics = simulator.channel_queue_metrics("link").unwrap();
    assert_eq!((metrics.packets_0_to_1, metrics.bytes_0_to_1), (1, 10));
    events.extend(simulator.advance_to(time(120)).unwrap());
    let metrics = simulator.channel_queue_metrics("link").unwrap();
    assert_eq!((metrics.packets_0_to_1, metrics.bytes_0_to_1), (0, 0));
    events.extend(simulator.run().unwrap());
    assert_eq!(starts(&events), vec![(0, 0), (1, 100), (queued, 120)]);
    let remaining = clone.run().unwrap();
    assert_eq!(remaining, events[consumed..]);
    assert_eq!(clone.channel_queue_metrics("link").unwrap().bytes_0_to_1, 0);
}

#[test]
fn in_flight_expiry_is_observable_at_the_deadline_before_serialization_finishes() {
    let mut simulator = simulator(Default::default());
    let expired = simulator
        .schedule_send_with_metadata(
            time(0),
            "source",
            "sink",
            vec![0; 100],
            PacketMetadata {
                expires_at: Some(time(5)),
                ..Default::default()
            },
        )
        .unwrap()
        .get();
    let following = send(&mut simulator, 0, 10, 0, 1);
    assert_eq!(
        starts(&simulator.advance_to(time(4)).unwrap()),
        vec![(expired, 0)]
    );
    let expiry = simulator.advance_to(time(5)).unwrap();
    assert_eq!(drops(&expiry), vec![(expired, 5, DropReason::Expired)]);
    assert_eq!(simulator.now(), time(5));
    assert_eq!(
        simulator
            .channel_queue_metrics("link")
            .unwrap()
            .packets_0_to_1,
        1
    );
    let remainder = simulator.run().unwrap();
    assert_eq!(starts(&remainder), vec![(following, 100)]);
    assert!(!remainder.iter().any(|event| matches!(event, NetworkEvent::DataReceived { packet, .. } | NetworkEvent::PacketDelivered { packet, .. } if packet.id().get() == expired)));
    assert!(drops(&remainder).is_empty());
}

#[test]
fn propagation_expiry_emits_once_at_the_deadline_without_reserving_the_medium() {
    let mut network = topology();
    network.channels[0].propagation_delay_ns = 100;
    let mut simulator = Simulator::new_with_options(network, SimulatorOptions::default()).unwrap();
    let expired = simulator
        .schedule_send_with_metadata(
            time(0),
            "source",
            "sink",
            vec![0; 10],
            PacketMetadata {
                expires_at: Some(time(40)),
                ..Default::default()
            },
        )
        .unwrap()
        .get();
    let following = send(&mut simulator, 0, 10, 0, 1);
    let before = simulator.advance_to(time(39)).unwrap();
    assert_eq!(starts(&before), vec![(expired, 0), (following, 10)]);
    assert!(drops(&before).is_empty());
    assert_eq!(
        drops(&simulator.advance_to(time(40)).unwrap()),
        vec![(expired, 40, DropReason::Expired)]
    );
    let remainder = simulator.run().unwrap();
    assert!(drops(&remainder).is_empty());
    assert!(remainder.iter().any(|event| matches!(event, NetworkEvent::PacketDelivered { at, packet, .. } if packet.id().get() == following && *at == time(120))));
    assert!(!remainder.iter().any(|event| matches!(event, NetworkEvent::PacketDelivered { packet, .. } if packet.id().get() == expired)));
}

#[test]
fn packet_can_arrive_just_before_its_deadline_without_a_later_expiry_event() {
    let mut simulator = simulator(Default::default());
    let delivered = simulator
        .schedule_send_with_metadata(
            time(0),
            "source",
            "sink",
            vec![0; 10],
            PacketMetadata {
                expires_at: Some(time(11)),
                ..Default::default()
            },
        )
        .unwrap()
        .get();
    let events = simulator.run().unwrap();
    assert!(drops(&events).is_empty());
    assert!(events.iter().any(|event| matches!(event, NetworkEvent::PacketDelivered { at, packet, .. } if packet.id().get() == delivered && *at == time(10))));
    assert_eq!(simulator.now(), time(10));
}
