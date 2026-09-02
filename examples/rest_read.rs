//! Fetch every metric from a unit once, over REST, and print them by device.
//!
//! ```sh
//! SA_HOST=192.168.1.100 SA_PASSWORD=secret cargo run --example rest_read
//! ```

use rs_solar_assistant::{Auth, DeviceClient, Result};

mod common;

#[tokio::main]
async fn main() -> Result<()> {
    let host = common::host();
    let device = DeviceClient::new(&host, Auth::password(common::password()));

    let metrics = device.metrics().await?;
    println!("Connected to {host} - {} metrics received\n", metrics.len());

    let mut current_device = String::new();
    for metric in metrics {
        if metric.device != current_device {
            current_device.clone_from(&metric.device);
            println!("--- {} ---", metric.device_label());
        }
        println!(
            "  {}: {}{}",
            metric.name,
            metric.value,
            common::unit(&metric)
        );
    }
    Ok(())
}
