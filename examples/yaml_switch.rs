//! Demonstrates loading a statically forwarded switched topology from YAML.

use c3mesh::{NetworkConfig, NetworkEvent, Simulator};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = NetworkConfig::from_yaml_str(include_str!("data/switched.yaml"))?;
    let mut simulator = Simulator::new(config)?;
    simulator.send("workstation", "server_a", b"for A".to_vec())?;
    simulator.send("workstation", "server_b", b"for B".to_vec())?;

    simulator.run_with(|event| {
        if let NetworkEvent::PacketDelivered { at, packet, sink } = event {
            println!("packet {} reached {sink} at {at}", packet.id());
        }
    })?;
    Ok(())
}
