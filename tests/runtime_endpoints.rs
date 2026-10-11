//! Dynamic topology and trajectory regression tests.
use c3mesh::*;
use std::collections::BTreeMap;
fn topology(prefix: &str) -> NetworkConfig {
    let tx: DeviceId = format!("{prefix}-tx").into();
    let rx: DeviceId = format!("{prefix}-rx").into();
    let channel: ChannelId = prefix.into();
    NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: tx.clone(),
                kind: DeviceKind::Source {
                    egress: channel.clone(),
                },
                mobility: Default::default(),
                interference: vec![],
            },
            DeviceConfig {
                id: rx.clone(),
                kind: DeviceKind::Sink,
                mobility: Default::default(),
                interference: vec![],
            },
        ],
        channels: vec![ChannelConfig {
            id: channel,
            endpoints: [tx, rx],
            bit_rate_bps: 8000,
            propagation_delay_ns: 0,
            state: Default::default(),
            distance: None,
            radio: None,
        }],
    }
}
#[test]
fn retirement_cancels_flight_and_queue_without_affecting_other_traffic() {
    let mut sim = Simulator::new_with_options(
        topology("old"),
        SimulatorOptions {
            seed: 7,
            channels: BTreeMap::from([("old".into(), ChannelOptions::default())]),
        },
    )
    .unwrap();
    let old = sim.send("old-tx", "old-rx", vec![0; 100]).unwrap();
    sim.register_topology(
        topology("weapon"),
        SimulatorOptions {
            seed: 0,
            channels: BTreeMap::from([("weapon".into(), ChannelOptions::default())]),
        },
    )
    .unwrap();
    let flying = sim.send("weapon-tx", "weapon-rx", vec![0; 100]).unwrap();
    let queued = sim.send("weapon-tx", "weapon-rx", vec![0; 100]).unwrap();
    sim.advance_to(SimTime::from_nanos(1)).unwrap();
    sim.retire_devices(&["weapon-rx".into()]).unwrap();
    let events = sim.run().unwrap();
    for id in [flying, queued] {
        assert_eq!(events.iter().filter(|e|matches!(e,NetworkEvent::PacketDropped{packet,reason:DropReason::EndpointRetired,..} if packet.id()==id)).count(),1);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e,NetworkEvent::PacketDelivered{packet,..} if packet.id()==id))
        );
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e,NetworkEvent::PacketDelivered{packet,..} if packet.id()==old))
    );
    assert!(
        sim.register_topology(topology("weapon"), SimulatorOptions::default())
            .is_err()
    );
}
#[test]
fn registration_is_atomic_and_mobility_can_change_at_current_time() {
    let mut sim = Simulator::new(topology("a")).unwrap();
    let mut invalid = topology("b");
    invalid.channels[0].bit_rate_bps = 0;
    assert!(
        sim.register_topology(invalid, SimulatorOptions::default())
            .is_err()
    );
    sim.register_topology(topology("b"), SimulatorOptions::default())
        .unwrap();
    sim.advance_to(SimTime::from_nanos(100)).unwrap();
    sim.set_device_mobility(
        "b-rx",
        MobilityModel::Static {
            position: Position3D::new(12.0, 34.0, 56.0),
        },
    )
    .unwrap();
    assert_eq!(
        sim.device_position_at("b-rx", sim.now()).unwrap(),
        Position3D::new(12.0, 34.0, 56.0)
    );
    assert!(
        sim.set_device_mobility(
            "b-rx",
            MobilityModel::Static {
                position: Position3D::new(f64::NAN, 0.0, 0.0)
            }
        )
        .is_err()
    );
}
#[test]
fn legacy_retirement_drops_reserved_transmissions_once() {
    let mut sim = Simulator::new(topology("a")).unwrap();
    for _ in 0..3 {
        sim.send("a-tx", "a-rx", vec![0; 100]).unwrap();
    }
    sim.advance_to(SimTime::from_nanos(1)).unwrap();
    sim.retire_devices(&["a-rx".into()]).unwrap();
    assert_eq!(
        sim.run()
            .unwrap()
            .iter()
            .filter(|e| matches!(
                e,
                NetworkEvent::PacketDropped {
                    reason: DropReason::EndpointRetired,
                    ..
                }
            ))
            .count(),
        3
    );
}

#[test]
fn snapshot_restores_pending_queue_receptions_and_retired_endpoint_ids() {
    let mut live = Simulator::new_with_options(
        topology("primary"),
        SimulatorOptions {
            seed: 7,
            channels: BTreeMap::from([("primary".into(), ChannelOptions::default())]),
        },
    )
    .unwrap();
    live.register_topology(topology("retired"), SimulatorOptions::default())
        .unwrap();
    live.retire_devices(&["retired-tx".into(), "retired-rx".into()])
        .unwrap();
    for _ in 0..3 {
        live.send("primary-tx", "primary-rx", vec![42; 100])
            .unwrap();
    }
    live.advance_to(SimTime::from_nanos(1)).unwrap();
    let mut restored = Simulator::restore(live.snapshot()).unwrap();
    assert!(
        restored
            .register_topology(topology("retired"), SimulatorOptions::default())
            .is_err()
    );
    assert_eq!(
        live.send("primary-tx", "primary-rx", vec![9; 100]).unwrap(),
        restored
            .send("primary-tx", "primary-rx", vec![9; 100])
            .unwrap()
    );
    assert_eq!(live.run().unwrap(), restored.run().unwrap());
}

#[cfg(feature = "json")]
#[test]
fn serialized_snapshot_preserves_events_and_rejects_corrupt_boundaries() {
    let mut live = Simulator::new(topology("radio")).unwrap();
    live.send("radio-tx", "radio-rx", vec![42; 100]).unwrap();
    live.advance_to(SimTime::from_nanos(10)).unwrap();
    let bytes = serde_json::to_vec(&live.snapshot()).unwrap();
    let mut restored = Simulator::restore(serde_json::from_slice(&bytes).unwrap()).unwrap();
    assert_eq!(bytes, serde_json::to_vec(&restored.snapshot()).unwrap());
    assert_eq!(live.run().unwrap(), restored.run().unwrap());
    for (field, value) in [("schema_version", 99), ("next_sequence", 0)] {
        let mut saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        saved[field] = value.into();
        assert!(Simulator::restore(serde_json::from_value(saved).unwrap()).is_err());
    }
}
