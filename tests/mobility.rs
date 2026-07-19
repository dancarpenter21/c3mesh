//! Behavioral tests for moving devices and distance-aware channels.

use comms_sim::{
    ChannelConfig, ChannelState, ConfigError, DeviceConfig, DeviceKind, DistanceChannel,
    DistanceRateModel, DropReason, MobilityModel, NetworkConfig, NetworkEvent, Position3D, SimTime,
    Simulator, Velocity3D, Waypoint,
};

#[test]
fn linear_and_waypoint_positions_follow_virtual_time() {
    let linear = MobilityModel::Linear {
        position_at_epoch: Position3D::new(10.0, 20.0, 30.0),
        velocity_mps: Velocity3D::new(2.0, -1.0, 0.5),
        epoch_ns: 1_000_000_000,
    };
    assert_eq!(
        linear.position_at(SimTime::from_nanos(3_000_000_000)),
        Position3D::new(14.0, 18.0, 31.0)
    );

    let waypoints = MobilityModel::Waypoints {
        waypoints: vec![
            Waypoint {
                at_ns: 1_000,
                position: Position3D::new(0.0, 0.0, 0.0),
            },
            Waypoint {
                at_ns: 3_000,
                position: Position3D::new(20.0, 10.0, 0.0),
            },
        ],
    };
    assert_eq!(
        waypoints.position_at(SimTime::from_nanos(2_000)),
        Position3D::new(10.0, 5.0, 0.0)
    );
    assert_eq!(waypoints.position_at(SimTime::ZERO), Position3D::ORIGIN);
    assert_eq!(
        waypoints.position_at(SimTime::from_nanos(4_000)),
        Position3D::new(20.0, 10.0, 0.0)
    );
}

#[test]
fn distance_changes_rate_propagation_and_range_over_time() {
    let mut simulator = Simulator::new(moving_config()).unwrap();

    let initial = simulator
        .channel_metrics_at("radio", SimTime::ZERO)
        .unwrap();
    assert_eq!(initial.distance_m, Some(1_000.0));
    assert_eq!(initial.propagation_delay_ns, 1_000_000_000);
    assert_eq!(initial.effective_bit_rate_bps, Some(10_000_000));

    let later = simulator
        .channel_metrics_at("radio", SimTime::from_nanos(10_000_000_000))
        .unwrap();
    assert_eq!(later.distance_m, Some(2_000.0));
    assert_eq!(later.propagation_delay_ns, 2_000_000_000);
    assert_eq!(later.effective_bit_rate_bps, Some(6_000_000));

    let out_of_range = simulator
        .channel_metrics_at("radio", SimTime::from_nanos(20_000_000_000))
        .unwrap();
    assert!(!out_of_range.available);
    assert_eq!(out_of_range.effective_bit_rate_bps, None);

    simulator
        .schedule_send(SimTime::ZERO, "vehicle", "station", vec![0; 750])
        .unwrap();
    simulator
        .schedule_send(
            SimTime::from_nanos(10_000_000_000),
            "vehicle",
            "station",
            vec![0; 750],
        )
        .unwrap();
    simulator
        .schedule_send(
            SimTime::from_nanos(20_000_000_000),
            "vehicle",
            "station",
            vec![0; 750],
        )
        .unwrap();

    let events = simulator.run().unwrap();
    let starts: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            NetworkEvent::TransmissionStarted {
                at,
                distance_mm,
                effective_bit_rate_bps,
                receive_at,
                ..
            } => Some((*at, *distance_mm, *effective_bit_rate_bps, *receive_at)),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        vec![
            (
                SimTime::ZERO,
                Some(1_000_000),
                10_000_000,
                SimTime::from_nanos(1_000_600_000),
            ),
            (
                SimTime::from_nanos(10_000_000_000),
                Some(2_000_000),
                6_000_000,
                SimTime::from_nanos(12_001_000_000),
            ),
        ]
    );
    assert!(events.iter().any(|event| matches!(
        event,
        NetworkEvent::PacketDropped {
            at,
            reason: DropReason::OutOfRange { .. },
            ..
        } if *at == SimTime::from_nanos(20_000_000_000)
    )));
}

#[test]
fn queued_packet_uses_distance_at_actual_start_time() {
    let mut config = moving_config();
    config.channels[0].bit_rate_bps = 800;
    config.channels[0].propagation_delay_ns = 0;
    config.channels[0].distance = Some(DistanceChannel {
        propagation_speed_mps: 1_000_000_000.0,
        max_range_m: 2_000.0,
        rate_model: DistanceRateModel::Linear {
            full_rate_distance_m: 0.0,
            minimum_bit_rate_bps: 400,
        },
    });
    if let MobilityModel::Linear {
        position_at_epoch, ..
    } = &mut config.devices[0].mobility
    {
        *position_at_epoch = Position3D::ORIGIN;
    }

    let mut simulator = Simulator::new(config).unwrap();
    simulator
        .send("vehicle", "station", vec![0; 1_000])
        .unwrap();
    simulator.send("vehicle", "station", vec![0]).unwrap();
    let starts: Vec<_> = simulator
        .run()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            NetworkEvent::TransmissionStarted {
                at,
                effective_bit_rate_bps,
                ..
            } => Some((at, effective_bit_rate_bps)),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        vec![
            (SimTime::ZERO, 800),
            (SimTime::from_nanos(10_000_000_000), 600),
        ]
    );
}

#[test]
fn invalid_motion_and_physical_models_are_rejected() {
    let mut config = moving_config();
    config.devices[0].mobility = MobilityModel::Waypoints {
        waypoints: Vec::new(),
    };
    assert_eq!(
        config.validate(),
        Err(ConfigError::InvalidMobility("vehicle".into()))
    );

    config = moving_config();
    config.channels[0].distance.as_mut().unwrap().max_range_m = f64::NAN;
    assert_eq!(
        config.validate(),
        Err(ConfigError::InvalidDistanceModel("radio".into()))
    );
}

fn moving_config() -> NetworkConfig {
    NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: "vehicle".into(),
                kind: DeviceKind::Source {
                    egress: "radio".into(),
                },
                mobility: MobilityModel::Linear {
                    position_at_epoch: Position3D::new(1_000.0, 0.0, 0.0),
                    velocity_mps: Velocity3D::new(100.0, 0.0, 0.0),
                    epoch_ns: 0,
                },
            },
            DeviceConfig {
                id: "station".into(),
                kind: DeviceKind::Sink,
                mobility: MobilityModel::Static {
                    position: Position3D::ORIGIN,
                },
            },
        ],
        channels: vec![ChannelConfig {
            id: "radio".into(),
            endpoints: ["vehicle".into(), "station".into()],
            bit_rate_bps: 10_000_000,
            propagation_delay_ns: 0,
            state: ChannelState::Operational,
            distance: Some(DistanceChannel {
                propagation_speed_mps: 1_000.0,
                max_range_m: 2_500.0,
                rate_model: DistanceRateModel::Linear {
                    full_rate_distance_m: 1_000.0,
                    minimum_bit_rate_bps: 4_000_000,
                },
            }),
        }],
    }
}
