//! List the sites an API key can reach, then authorize one and read it back
//! through the cloud proxy.
//!
//! ```sh
//! SA_API_KEY=... cargo run --example cloud_sites
//! ```

use rs_solar_assistant::{Auth, CloudClient, DeviceClient, Result, Scheme};

mod common;

#[tokio::main]
async fn main() -> Result<()> {
    let cloud = CloudClient::new(common::api_key());

    let sites = cloud.sites().await?;
    println!("{} site(s)\n", sites.len());
    for site in &sites {
        println!(
            "  [{}] {} - {} x {}, last seen {}",
            site.id, site.name, site.inverter_count, site.inverter, site.last_seen_at,
        );
    }

    let Some(site) = sites.first() else {
        return Ok(());
    };

    // The token is short-lived and works for the proxy and the local network
    // alike, so the same credential drives REST and the WebSocket.
    let authorization = cloud.authorize_site(site.id).await?;
    let device = DeviceClient::builder(&authorization.host, Auth::from(&authorization))
        .scheme(Scheme::Https)
        .build()?;

    println!("\nsystem metrics for {}:", authorization.site_name);
    for metric in device.system_metrics().await? {
        println!("  {}: {}", metric.name, metric.value);
    }
    Ok(())
}
