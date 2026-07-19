//! Demonstrates a moving aircraft communicating with a stationary ground site.

use c3mesh::{
    ChannelConfig, ChannelState, DeviceConfig, DeviceKind, DistanceChannel, DistanceRateModel,
    MobilityModel, NetworkConfig, NetworkEvent, Position3D, SimTime, Simulator, Velocity3D,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let network = NetworkConfig {
        devices: vec![
            DeviceConfig {
                id: "aircraft".into(),
                kind: DeviceKind::Source {
                    egress: "radio".into(),
                },
                mobility: MobilityModel::Linear {
                    position_at_epoch: Position3D::new(100_000.0, 0.0, 10_000.0),
                    velocity_mps: Velocity3D::new(-150.0, 0.0, 0.0),
                    epoch_ns: 0,
                },
                interference: vec![],
            },
            DeviceConfig {
                id: "ground_station".into(),
                kind: DeviceKind::Sink,
                mobility: MobilityModel::Static {
                    position: Position3D::ORIGIN,
                },
                interference: vec![],
            },
        ],
        channels: vec![ChannelConfig {
            id: "radio".into(),
            endpoints: ["aircraft".into(), "ground_station".into()],
            bit_rate_bps: 10_000_000,
            propagation_delay_ns: 50_000,
            state: ChannelState::Operational,
            distance: Some(DistanceChannel {
                propagation_speed_mps: 299_792_458.0,
                max_range_m: 120_000.0,
                rate_model: DistanceRateModel::Linear {
                    full_rate_distance_m: 20_000.0,
                    minimum_bit_rate_bps: 1_000_000,
                },
            }),
            radio: None,
        }],
    };

    let mut simulator = Simulator::new(network)?;
    simulator.schedule_send(SimTime::ZERO, "aircraft", "ground_station", vec![0; 1_000])?;
    simulator.schedule_send(
        SimTime::from_nanos(300_000_000_000),
        "aircraft",
        "ground_station",
        vec![0; 1_000],
    )?;
    simulator.schedule_send(
        SimTime::from_nanos(1_500_000_000_000),
        "aircraft",
        "ground_station",
        vec![0; 1_000],
    )?;

    simulator.run_with(|event| match event {
        NetworkEvent::TransmissionStarted {
            at,
            distance_mm,
            effective_bit_rate_bps,
            ..
        } => println!(
            "{at}: distance {:.1} km, rate {:.2} Mbit/s",
            distance_mm.unwrap_or_default() as f64 / 1_000_000.0,
            *effective_bit_rate_bps as f64 / 1_000_000.0,
        ),
        NetworkEvent::PacketDropped { at, reason, .. } => {
            println!("{at}: packet dropped: {reason:?}");
        }
        _ => {}
    })?;
    Ok(())
}
