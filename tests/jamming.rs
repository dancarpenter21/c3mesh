//! Behavioral tests for receiver-side spectrum and interference modeling.

use c3mesh::{
    ChannelConfig, ChannelState, ConfigError, DeviceConfig, DeviceKind, DropReason, FrequencyBand,
    InterferenceResponse, NetworkConfig, NetworkEvent, RadioChannel, ReceiverInterference, SimTime,
    SimulationError, Simulator,
};

const RADIO_BAND: FrequencyBand = FrequencyBand::new(100, 200);

fn interference(lower_hz: u64, upper_hz: u64, jammed: f64) -> ReceiverInterference {
    ReceiverInterference {
        band: FrequencyBand::new(lower_hz, upper_hz),
        jammed,
    }
}

fn radio_config(receiver_interference: Vec<ReceiverInterference>) -> NetworkConfig {
    NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: "source".into(),
                kind: DeviceKind::Source {
                    egress: "radio".into(),
                },
                mobility: Default::default(),
                interference: vec![],
            },
            DeviceConfig {
                id: "sink".into(),
                kind: DeviceKind::Sink,
                mobility: Default::default(),
                interference: receiver_interference,
            },
        ],
        channels: vec![ChannelConfig {
            id: "radio".into(),
            endpoints: ["source".into(), "sink".into()],
            bit_rate_bps: 8_000_000,
            propagation_delay_ns: 100,
            state: ChannelState::Operational,
            distance: None,
            radio: Some(RadioChannel {
                band: RADIO_BAND,
                interference_response: InterferenceResponse::default(),
            }),
        }],
    }
}

#[test]
fn overlap_is_fractional_additive_and_capped() {
    let simulator = Simulator::new(radio_config(vec![
        interference(100, 150, 0.5),
        interference(100, 200, 0.25),
        interference(200, 300, 1.0),
    ]))
    .unwrap();
    let metrics = simulator
        .transmission_metrics_at("radio", "source", SimTime::ZERO)
        .unwrap();

    assert_eq!(metrics.receiver.as_str(), "sink");
    assert_eq!(metrics.frequency_band, Some(RADIO_BAND));
    assert_eq!(metrics.jammed, 0.5);
    assert_eq!(metrics.base_bit_rate_bps, Some(8_000_000));
    assert_eq!(metrics.effective_bit_rate_bps, Some(4_000_000));

    let simulator = Simulator::new(radio_config(vec![
        interference(100, 200, 0.75),
        interference(100, 200, 0.75),
    ]))
    .unwrap();
    assert_eq!(
        simulator
            .transmission_metrics_at("radio", "source", SimTime::ZERO)
            .unwrap()
            .jammed,
        1.0
    );
}

#[test]
fn receiver_interference_is_directional() {
    let mut config = radio_config(vec![]);
    config.devices[0].interference = vec![interference(100, 200, 1.0)];
    let simulator = Simulator::new(config).unwrap();

    assert!(
        simulator
            .transmission_metrics_at("radio", "source", SimTime::ZERO)
            .unwrap()
            .available
    );
    assert!(
        !simulator
            .transmission_metrics_at("radio", "sink", SimTime::ZERO)
            .unwrap()
            .available
    );
}

#[test]
fn custom_response_thresholds_control_supported_rate() {
    let mut config = radio_config(vec![interference(100, 200, 0.2)]);
    config.channels[0]
        .radio
        .as_mut()
        .unwrap()
        .interference_response = InterferenceResponse {
        unaffected_below: 0.2,
        severed_at: 0.8,
    };
    let simulator = Simulator::new(config).unwrap();
    assert_eq!(
        simulator
            .transmission_metrics_at("radio", "source", SimTime::ZERO)
            .unwrap()
            .effective_bit_rate_bps,
        Some(8_000_000)
    );

    let mut config = radio_config(vec![interference(100, 200, 0.5)]);
    config.channels[0]
        .radio
        .as_mut()
        .unwrap()
        .interference_response = InterferenceResponse {
        unaffected_below: 0.2,
        severed_at: 0.8,
    };
    assert_eq!(
        Simulator::new(config)
            .unwrap()
            .transmission_metrics_at("radio", "source", SimTime::ZERO)
            .unwrap()
            .effective_bit_rate_bps,
        Some(4_000_000)
    );
}

#[test]
fn start_interference_degrades_or_severs_the_radio() {
    let mut simulator = Simulator::new(radio_config(vec![interference(100, 200, 0.5)])).unwrap();
    simulator.send("source", "sink", vec![0]).unwrap();
    assert!(matches!(
        simulator.run().unwrap().first(),
        Some(NetworkEvent::TransmissionStarted {
            receive_at,
            effective_bit_rate_bps: 4_000_000,
            frequency_band: Some(band),
            ..
        }) if *receive_at == SimTime::from_nanos(2_100) && *band == RADIO_BAND
    ));

    let mut simulator = Simulator::new(radio_config(vec![interference(100, 200, 1.0)])).unwrap();
    simulator.send("source", "sink", vec![0]).unwrap();
    assert!(matches!(
        simulator.run().unwrap().as_slice(),
        [NetworkEvent::PacketDropped {
            at,
            device,
            reason: DropReason::ReceiverInterference { .. },
            ..
        }] if *at == SimTime::ZERO && device.as_str() == "sink"
    ));
}

#[test]
fn intermittent_pulse_during_reception_corrupts_in_flight_packet() {
    let mut simulator = Simulator::new(radio_config(vec![])).unwrap();
    simulator
        .schedule_receiver_interference(
            SimTime::from_nanos(500),
            "sink",
            vec![interference(100, 200, 0.5)],
        )
        .unwrap();
    simulator
        .schedule_receiver_interference(SimTime::from_nanos(600), "sink", vec![])
        .unwrap();
    simulator.send("source", "sink", vec![0]).unwrap();

    assert!(matches!(
        simulator.run().unwrap().as_slice(),
        [
            NetworkEvent::TransmissionStarted {
                receive_at,
                effective_bit_rate_bps: 8_000_000,
                ..
            },
            NetworkEvent::PacketDropped {
                at,
                reason: DropReason::ReceiverInterference { .. },
                ..
            }
        ] if *receive_at == SimTime::from_nanos(1_100) && at == receive_at
    ));
}

#[test]
fn queued_packet_samples_interference_at_its_actual_start() {
    let mut config = radio_config(vec![]);
    config.channels[0].bit_rate_bps = 8;
    config.channels[0].propagation_delay_ns = 0;
    let mut simulator = Simulator::new(config).unwrap();
    simulator
        .schedule_receiver_interference(
            SimTime::from_nanos(1_000_000_000),
            "sink",
            vec![interference(100, 200, 0.5)],
        )
        .unwrap();
    simulator.send("source", "sink", vec![0]).unwrap();
    simulator.send("source", "sink", vec![0]).unwrap();

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
        vec![(SimTime::ZERO, 8), (SimTime::from_nanos(1_000_000_000), 4),]
    );
}

#[test]
fn receive_window_excludes_propagation_and_its_end_boundary() {
    let mut simulator = Simulator::new(radio_config(vec![])).unwrap();
    simulator
        .schedule_receiver_interference(
            SimTime::from_nanos(10),
            "sink",
            vec![interference(100, 200, 1.0)],
        )
        .unwrap();
    simulator
        .schedule_receiver_interference(SimTime::from_nanos(100), "sink", vec![])
        .unwrap();
    simulator
        .schedule_receiver_interference(
            SimTime::from_nanos(1_100),
            "sink",
            vec![interference(100, 200, 1.0)],
        )
        .unwrap();
    simulator.send("source", "sink", vec![0]).unwrap();

    assert!(simulator.run().unwrap().iter().any(|event| matches!(
        event,
        NetworkEvent::PacketDelivered { at, .. } if *at == SimTime::from_nanos(1_100)
    )));
}

#[test]
fn live_update_after_start_is_seen_during_reception() {
    let mut simulator = Simulator::new(radio_config(vec![])).unwrap();
    simulator.send("source", "sink", vec![0]).unwrap();
    assert!(matches!(
        simulator.step().unwrap(),
        Some(NetworkEvent::TransmissionStarted { .. })
    ));
    simulator
        .set_receiver_interference("sink", vec![interference(100, 200, 0.25)])
        .unwrap();

    assert!(matches!(
        simulator.step().unwrap(),
        Some(NetworkEvent::PacketDropped {
            reason: DropReason::ReceiverInterference { .. },
            ..
        })
    ));
}

#[test]
fn bandless_channels_ignore_receiver_interference() {
    let mut config = radio_config(vec![interference(100, 200, 1.0)]);
    config.channels[0].radio = None;
    let mut simulator = Simulator::new(config).unwrap();
    simulator.send("source", "sink", vec![0]).unwrap();
    assert!(
        simulator
            .run()
            .unwrap()
            .iter()
            .any(|event| matches!(event, NetworkEvent::PacketDelivered { .. }))
    );
}

#[test]
fn invalid_config_and_runtime_snapshots_are_rejected() {
    let mut config = radio_config(vec![interference(100, 100, 0.5)]);
    assert_eq!(
        config.validate(),
        Err(ConfigError::InvalidReceiverInterference("sink".into()))
    );

    config = radio_config(vec![]);
    config.channels[0]
        .radio
        .as_mut()
        .unwrap()
        .interference_response
        .severed_at = 0.0;
    assert_eq!(
        config.validate(),
        Err(ConfigError::InvalidRadio("radio".into()))
    );

    let mut simulator = Simulator::new(radio_config(vec![])).unwrap();
    assert_eq!(
        simulator.set_receiver_interference("sink", vec![interference(100, 200, f64::NAN)]),
        Err(SimulationError::InvalidReceiverInterference("sink".into()))
    );
    assert!(
        simulator
            .receiver_interference_at("sink", SimTime::ZERO)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        simulator.set_receiver_interference("missing", vec![]),
        Err(SimulationError::UnknownDevice("missing".into()))
    );

    simulator
        .schedule_receiver_interference(
            SimTime::from_nanos(10),
            "sink",
            vec![interference(100, 200, 0.5)],
        )
        .unwrap();
    simulator
        .schedule_receiver_interference(SimTime::from_nanos(10), "sink", vec![])
        .unwrap();
    assert!(
        simulator
            .receiver_interference_at("sink", SimTime::from_nanos(10))
            .unwrap()
            .is_empty()
    );

    simulator.send("source", "sink", vec![0]).unwrap();
    simulator.run().unwrap();
    assert_eq!(
        simulator.schedule_receiver_interference(SimTime::ZERO, "sink", vec![]),
        Err(SimulationError::TimeInPast)
    );
}

#[test]
fn explicit_channel_failure_takes_precedence_over_jamming() {
    let mut config = radio_config(vec![interference(100, 200, 1.0)]);
    config.channels[0].state = ChannelState::Severed;
    let mut simulator = Simulator::new(config).unwrap();
    simulator.send("source", "sink", vec![0]).unwrap();
    assert!(matches!(
        simulator.run().unwrap().as_slice(),
        [NetworkEvent::PacketDropped {
            reason: DropReason::ChannelSevered { .. },
            ..
        }]
    ));
}
