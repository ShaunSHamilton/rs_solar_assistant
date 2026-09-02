//! Read a unit's own metrics: site ID, software version, CPU temperature,
//! free storage.
//!
//! `GET /api/v1/system` only exists on newer builds. An older unit answers
//! `404`, which is an ordinary API error - catch it and check the status
//! rather than looking for a special return value.
//!
//! ```sh
//! SA_HOST=192.168.1.100 SA_PASSWORD=secret cargo run --example rest_system
//! ```

use rs_solar_assistant::{Auth, DeviceClient, Result};

mod common;

#[tokio::main]
async fn main() -> Result<()> {
    let host = common::host();
    let device = DeviceClient::new(&host, Auth::password(common::password()));

    let rows = match device.system_metrics().await {
        Ok(rows) => rows,
        Err(error) if error.status() == Some(404) => {
            println!("{host} runs a build without /api/v1/system");
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    println!(
        "Connected to {host} - {} system metrics received\n",
        rows.len()
    );
    for metric in &rows {
        println!(
            "  {}: {}{}",
            metric.name,
            metric.value,
            common::unit(metric)
        );
    }
    Ok(())
}
