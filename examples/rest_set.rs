//! Write one setting on a unit over REST.
//!
//! ```sh
//! SA_HOST=192.168.1.100 SA_PASSWORD=secret \
//!   cargo run --example rest_set -- inverter_1/power_mode "Off grid with relay"
//! ```

use std::process::ExitCode;

use rs_solar_assistant::{Auth, DeviceClient};

mod common;

#[tokio::main]
async fn main() -> ExitCode {
    let Some((topic, value)) = common::topic_and_value("rest_set") else {
        return ExitCode::FAILURE;
    };

    let device = DeviceClient::new(common::host(), Auth::password(common::password()));
    match device.set_metric(&topic, &value).await {
        Ok(()) => {
            println!("saved {topic} = {value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
