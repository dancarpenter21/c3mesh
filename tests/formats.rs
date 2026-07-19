//! Feature-gated tests for serialized topology formats.

#[cfg(any(feature = "json", feature = "yaml"))]
use comms_sim::NetworkConfig;

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
