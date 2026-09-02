//! Write one setting over the WebSocket and wait for the unit to confirm it.
//!
//! ```sh
//! SA_HOST=192.168.1.100 SA_PASSWORD=secret \
//!   cargo run --example websocket_set -- inverter_1/power_mode "Off grid with relay"
//! ```

use std::process::ExitCode;

use rs_solar_assistant::{Auth, Socket, socket::Options};

mod common;

#[tokio::main]
async fn main() -> ExitCode {
    let Some((topic, value)) = common::topic_and_value("websocket_set") else {
        return ExitCode::FAILURE;
    };
    let host = common::host();

    println!("Connecting to {host} ...");
    let options = Options::local(&host, Auth::password(common::password()));
    let mut socket = match Socket::connect(options).await {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };

    let outcome = socket.set_setting(&topic, &value).await;
    let _ = socket.close().await;

    match outcome {
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
