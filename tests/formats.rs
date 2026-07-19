//! Feature-gated tests for serialized topology formats.

#[cfg(any(feature = "json", feature = "yaml"))]
use c3mesh::NetworkConfig;

#[cfg(feature = "json")]
#[test]
fn json_example_loads_and_round_trips() {
    let input = include_str!("../examples/data/routed.json");
    let config = NetworkConfig::from_json_str(input).unwrap();
    let encoded = serde_json::to_string(&config).unwrap();
    assert_eq!(NetworkConfig::from_json_str(&encoded).unwrap(), config);
    assert!(NetworkConfig::from_json_str("not json").is_err());
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_example_loads_and_round_trips() {
    let input = include_str!("../examples/data/switched.yaml");
    let config = NetworkConfig::from_yaml_str(input).unwrap();
    let encoded = yaml_serde::to_string(&config).unwrap();
    assert_eq!(NetworkConfig::from_yaml_str(&encoded).unwrap(), config);
    assert!(NetworkConfig::from_yaml_str("devices: [").is_err());
}

#[cfg(feature = "json")]
#[test]
fn json_radio_and_interference_round_trip() {
    let input = r#"
        {
          "devices": [
            { "id": "sender", "kind": "source", "egress": "radio" },
            {
              "id": "receiver",
              "kind": "sink",
              "interference": [{
                "band": { "lower_hz": 100, "upper_hz": 150 },
                "jammed": 0.5
              }]
            }
          ],
          "channels": [{
            "id": "radio",
            "endpoints": ["sender", "receiver"],
            "bit_rate_bps": 1000000,
            "radio": {
              "band": { "lower_hz": 100, "upper_hz": 200 },
              "interference_response": {
                "unaffected_below": 0.1,
                "severed_at": 0.9
              }
            }
          }]
        }
    "#;
    let config = NetworkConfig::from_json_str(input).unwrap();
    assert_eq!(config.devices[1].interference[0].jammed, 0.5);
    assert_eq!(config.channels[0].radio.unwrap().band.upper_hz, 200);
    let encoded = serde_json::to_string(&config).unwrap();
    assert_eq!(NetworkConfig::from_json_str(&encoded).unwrap(), config);
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_radio_and_interference_round_trip() {
    let input = r#"
devices:
  - id: sender
    kind: source
    egress: radio
  - id: receiver
    kind: sink
    interference:
      - band: { lower_hz: 100, upper_hz: 150 }
        jammed: 0.5
channels:
  - id: radio
    endpoints: [sender, receiver]
    bit_rate_bps: 1000000
    radio:
      band: { lower_hz: 100, upper_hz: 200 }
      interference_response:
        unaffected_below: 0.1
        severed_at: 0.9
"#;
    let config = NetworkConfig::from_yaml_str(input).unwrap();
    assert_eq!(config.devices[1].interference[0].jammed, 0.5);
    assert_eq!(config.channels[0].radio.unwrap().band.upper_hz, 200);
    let encoded = yaml_serde::to_string(&config).unwrap();
    assert_eq!(NetworkConfig::from_yaml_str(&encoded).unwrap(), config);
}
