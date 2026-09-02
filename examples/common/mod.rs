//! Configuration shared by the examples, read from the environment.

#![allow(dead_code)]

use rs_solar_assistant::Metric;

/// Address of the unit, e.g. `192.168.1.100` or `unit.local:8080`.
pub fn host() -> String {
    std::env::var("SA_HOST").unwrap_or_else(|_| "192.168.1.100".to_owned())
}

/// The unit's web password.
pub fn password() -> String {
    std::env::var("SA_PASSWORD").unwrap_or_else(|_| {
        eprintln!("set SA_PASSWORD to the unit's web password");
        std::process::exit(1);
    })
}

/// A cloud API key from solar-assistant.io/user/edit#api.
pub fn api_key() -> String {
    std::env::var("SA_API_KEY").unwrap_or_else(|_| {
        eprintln!("set SA_API_KEY to a cloud API key");
        std::process::exit(1);
    })
}

/// The topic and value to write, from the command line.
pub fn topic_and_value(example: &str) -> Option<(String, String)> {
    let mut args = std::env::args().skip(1);
    match (args.next(), args.next()) {
        (Some(topic), Some(value)) => Some((topic, value)),
        _ => {
            eprintln!("usage: cargo run --example {example} -- <topic> <value>");
            eprintln!("  e.g. cargo run --example {example} -- inverter_1/power_mode 'Off grid'");
            None
        }
    }
}

/// A metric's unit, ready to append to its value.
pub fn unit(metric: &Metric) -> String {
    if metric.unit.is_empty() {
        String::new()
    } else {
        format!(" {}", metric.unit)
    }
}
