//! Demonstrates loading a routed topology from JSON.

use comms_sim::{NetworkConfig, NetworkEvent, Simulator};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = NetworkConfig::from_json_str(include_str!("data/routed.json"))?;
    let mut simulator = Simulator::new(config)?;
    simulator.send("client", "service", b"request".to_vec())?;

    for event in simulator.run()? {
        if let NetworkEvent::DataReceived {
            at, packet, device, ..
        } = event
        {
            println!("packet {} received by {device} at {at}", packet.id());
        }
    }
    Ok(())
}
