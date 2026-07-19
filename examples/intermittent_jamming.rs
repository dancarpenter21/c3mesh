//! Demonstrates degradation, recovery, and an intermittent in-flight radio drop.

use c3mesh::{
    ChannelConfig, ChannelState, DeviceConfig, DeviceKind, FrequencyBand, InterferenceResponse,
    NetworkConfig, NetworkEvent, RadioChannel, ReceiverInterference, SimTime, Simulator,
};

const RADIO_BAND: FrequencyBand = FrequencyBand::new(2_400_000_000, 2_420_000_000);

fn interference(band: FrequencyBand, jammed: f64) -> ReceiverInterference {
    ReceiverInterference { band, jammed }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let network = NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: "vehicle".into(),
                kind: DeviceKind::Source {
                    egress: "radio".into(),
                },
                mobility: Default::default(),
                interference: vec![],
            },
            DeviceConfig {
                id: "operator".into(),
                kind: DeviceKind::Sink,
                mobility: Default::default(),
                interference: vec![],
            },
        ],
        channels: vec![ChannelConfig {
            id: "radio".into(),
            endpoints: ["vehicle".into(), "operator".into()],
            bit_rate_bps: 1_000_000,
            propagation_delay_ns: 100_000,
            state: ChannelState::Operational,
            distance: None,
            radio: Some(RadioChannel {
                band: RADIO_BAND,
                interference_response: InterferenceResponse::default(),
            }),
        }],
    };

    let mut simulator = Simulator::new(network)?;

    // A continuous, full-band jammer degrades the second packet to half rate.
    simulator.schedule_receiver_interference(
        SimTime::from_nanos(2_000_000),
        "operator",
        vec![interference(RADIO_BAND, 0.5)],
    )?;
    simulator.schedule_receiver_interference(SimTime::from_nanos(4_000_000), "operator", vec![])?;

    // A short pulse begins after the third packet starts and corrupts it.
    simulator.schedule_receiver_interference(
        SimTime::from_nanos(5_200_000),
        "operator",
        vec![interference(RADIO_BAND, 0.5)],
    )?;
    simulator.schedule_receiver_interference(SimTime::from_nanos(5_300_000), "operator", vec![])?;

    // An accidental emitter covering half the radio band contributes 0.3.
    simulator.schedule_receiver_interference(
        SimTime::from_nanos(8_000_000),
        "operator",
        vec![interference(
            FrequencyBand::new(2_400_000_000, 2_410_000_000),
            0.6,
        )],
    )?;

    for at in [0, 2_000_000, 5_000_000, 8_000_000] {
        simulator.schedule_send(SimTime::from_nanos(at), "vehicle", "operator", vec![0; 100])?;
    }

    simulator.run_with(|event| match event {
        NetworkEvent::TransmissionStarted {
            at,
            packet,
            effective_bit_rate_bps,
            ..
        } => println!(
            "{at}: packet {} started at {effective_bit_rate_bps} bit/s",
            packet.id()
        ),
        NetworkEvent::PacketDelivered { at, packet, .. } => {
            println!("{at}: packet {} delivered", packet.id());
        }
        NetworkEvent::PacketDropped {
            at, packet, reason, ..
        } => println!("{at}: packet {} dropped: {reason:?}", packet.id()),
        _ => {}
    })?;
    Ok(())
}
