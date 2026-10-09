//! Compaction must preserve observable network behavior, including reception peaks.
use c3mesh::*;
use std::collections::BTreeMap;

fn time(ns: u64) -> SimTime {
    SimTime::from_nanos(ns)
}
fn jam() -> Vec<ReceiverInterference> {
    vec![ReceiverInterference {
        band: FrequencyBand::new(100, 200),
        jammed: 1.0,
    }]
}
fn simulator(packet_engine: bool) -> Simulator {
    let config = NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: "tx".into(),
                kind: DeviceKind::Source {
                    egress: "radio".into(),
                },
                mobility: Default::default(),
                interference: vec![],
            },
            DeviceConfig {
                id: "rx".into(),
                kind: DeviceKind::Sink,
                mobility: Default::default(),
                interference: vec![],
            },
        ],
        channels: vec![ChannelConfig {
            id: "radio".into(),
            endpoints: ["tx".into(), "rx".into()],
            bit_rate_bps: 8_000_000,
            propagation_delay_ns: 100,
            state: Default::default(),
            distance: None,
            radio: Some(RadioChannel {
                band: FrequencyBand::new(100, 200),
                interference_response: Default::default(),
            }),
        }],
    };
    Simulator::new_with_options(
        config,
        SimulatorOptions {
            seed: 7,
            channels: if packet_engine {
                BTreeMap::from([("radio".into(), ChannelOptions::default())])
            } else {
                BTreeMap::new()
            },
        },
    )
    .unwrap()
}

#[test]
fn compaction_preserves_long_receptions_and_same_time_changes() {
    for packet_engine in [false, true] {
        // Reception is [100, 10100); interference at its end must not affect it.
        for jam_at in [100, 101, 5000, 10099, 10100] {
            let mut full = simulator(packet_engine);
            full.send("tx", "rx", vec![0; 10]).unwrap();
            full.schedule_receiver_interference(time(jam_at), "rx", jam())
                .unwrap();
            full.schedule_receiver_interference(time(jam_at + 1), "rx", vec![])
                .unwrap();
            let mut compact = full.clone();
            let mut events = vec![];
            for ns in [0, 100, 101, 5000, 6000, 10100, 10101, 20000] {
                let a = full.advance_to(time(ns)).unwrap();
                let b = compact.advance_to(time(ns)).unwrap();
                assert_eq!(
                    a, b,
                    "packet_engine={packet_engine}, jam_at={jam_at}, ns={ns}"
                );
                events.extend(b);
                let previous = compact.retained_history_from();
                assert!(compact.compact_history() >= previous);
                assert_eq!(
                    full.transmission_metrics_at("radio", "tx", time(ns))
                        .unwrap(),
                    compact
                        .transmission_metrics_at("radio", "tx", time(ns))
                        .unwrap()
                );
            }
            assert!(
                events.iter().any(|e| matches!(
                    e,
                    NetworkEvent::PacketDropped {
                        reason: DropReason::ReceiverInterference { .. },
                        ..
                    }
                )) == (jam_at < 10100)
            );
            assert_eq!(compact.history_statistics().interference_entries, 2);
            assert_eq!(compact.history_statistics().pending_receptions, 0);
        }
    }
}

#[test]
fn idle_histories_keep_anchors_future_updates_and_explicit_query_boundary() {
    let mut full = simulator(false);
    let mut compact = full.clone();
    for ns in 1..=1000 {
        for sim in [&mut full, &mut compact] {
            sim.advance_to(time(ns)).unwrap();
            sim.set_receiver_interference("rx", jam()).unwrap();
            // Replacing at the same timestamp remains supported.
            sim.set_receiver_interference("rx", vec![]).unwrap();
            sim.set_device_mobility(
                "rx",
                MobilityModel::Static {
                    position: Position3D::new(ns as f64, 0.0, 0.0),
                },
            )
            .unwrap();
        }
        compact.compact_history();
        assert_eq!(compact.history_statistics().interference_entries, 2);
        assert_eq!(compact.history_statistics().mobility_entries, 2);
        assert_eq!(
            compact.device_position_at("rx", time(ns)).unwrap(),
            full.device_position_at("rx", time(ns)).unwrap()
        );
    }
    compact
        .schedule_receiver_interference(time(2000), "rx", jam())
        .unwrap();
    compact
        .schedule_receiver_interference(time(3000), "rx", vec![])
        .unwrap();
    compact.compact_history();
    assert_eq!(compact.history_statistics().interference_entries, 4);
    assert_eq!(
        compact.receiver_interference_at("rx", time(2000)).unwrap(),
        jam()
    );
    assert!(
        compact
            .receiver_interference_at("rx", time(3000))
            .unwrap()
            .is_empty()
    );
    assert_eq!(full.retained_history_from(), SimTime::ZERO);
    assert!(full.device_position_at("rx", time(1)).is_ok());
    let expected = SimulationError::HistoryUnavailable {
        retained_from: time(1000),
    };
    assert_eq!(
        compact.device_position_at("rx", time(999)),
        Err(expected.clone())
    );
    assert_eq!(
        compact.receiver_interference_at("rx", time(999)),
        Err(expected.clone())
    );
    assert_eq!(
        compact.channel_metrics_at("radio", time(999)),
        Err(expected.clone())
    );
    assert_eq!(
        compact.transmission_metrics_at("radio", "tx", time(999)),
        Err(expected)
    );
    // Failure cannot mutate the retained boundary or pending updates.
    assert_eq!(
        compact.schedule_receiver_interference(time(999), "rx", jam()),
        Err(SimulationError::TimeInPast)
    );
    compact.advance_to(time(3000)).unwrap();
    compact.compact_history();
    assert_eq!(compact.history_statistics().interference_entries, 2);
}

#[test]
fn retirement_releases_history_pinned_by_a_pending_reception() {
    let mut full = simulator(true);
    full.send("tx", "rx", vec![0; 100]).unwrap();
    full.advance_to(time(500)).unwrap();
    let mut compact = full.clone();
    assert_eq!(compact.compact_history(), time(100));
    for sim in [&mut full, &mut compact] {
        sim.retire_devices(&["rx".into()]).unwrap();
    }
    assert_eq!(compact.compact_history(), time(500));
    assert_eq!(compact.history_statistics().devices, 0);
    assert_eq!(full.run().unwrap(), compact.run().unwrap());
}
