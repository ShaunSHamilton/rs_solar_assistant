//! Stream live metrics from a unit until Ctrl+C.
//!
//! ```sh
//! SA_HOST=192.168.1.100 SA_PASSWORD=secret cargo run --example websocket_read
//! ```

use futures_util::StreamExt;
use rs_solar_assistant::{Auth, Result, Socket, socket::Options};

mod common;

#[tokio::main]
async fn main() -> Result<()> {
    let host = common::host();

    println!("Connecting to {host} ...");
    let mut socket =
        Socket::connect(Options::local(&host, Auth::password(common::password()))).await?;
    println!("Connected - streaming metrics (Ctrl+C to stop)\n");

    // No filters: the server picks its curated default set.
    socket.subscribe_metrics([]).await?;

    let mut metrics = socket.metrics();
    loop {
        // The stream composes with anything else the task is waiting on.
        tokio::select! {
            metric = metrics.next() => match metric {
                Some(metric) => {
                    let metric = metric?;
                    println!(
                        "[{}] {}: {}{}",
                        metric.device_label(),
                        metric.name,
                        metric.value,
                        common::unit(&metric),
                    );
                }
                None => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    drop(metrics);
    socket.close().await
}
