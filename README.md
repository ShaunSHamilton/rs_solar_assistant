# rs_solar_assistant

> [!NOTE]
> This work is an external port of [Solar-Assistant/py_solar_assistant](https://github.com/Solar-Assistant/py_solar_assistant), and is unaffiliated with SolarAssistant.

Rust client for SolarAssistant: the cloud API, a unit's REST API, and its real-time WebSocket. Also available in
[Python](https://github.com/Solar-Assistant/py_solar_assistant) and [Go](https://github.com/Solar-Assistant/go_solar_assistant).

## Installation

```bash
cargo add rs_solar_assistant tokio --features tokio/full
```

Requires Rust 1.90+ and a [Tokio](https://tokio.rs) runtime.

### Feature flags

| Feature      | Default | Brings in                                    |
| ------------ | ------- | -------------------------------------------- |
| `cloud`      | yes     | `cloud::Client`, on `reqwest`                |
| `device`     | yes     | `device::Client`, on `reqwest`               |
| `websocket`  | yes     | `socket::Socket`, on `tokio-tungstenite`     |
| `rustls-tls` | yes     | TLS through `rustls`                         |
| `native-tls` | no      | TLS through the platform's TLS stack         |

Pick exactly one TLS backend. A REST-only consumer can drop the WebSocket dependency tree:

```toml
rs_solar_assistant = { version = "0.1", default-features = false, features = ["device", "rustls-tls"] }
```

## Cloud API

Every endpoint needs an API key - generate one at [solar-assistant.io/user/edit#api](https://solar-assistant.io/user/edit#api).

```rust,no_run
use rs_solar_assistant::CloudClient;

# async fn example() -> rs_solar_assistant::Result<()> {
let cloud = CloudClient::new("<api-key>");
# Ok(())
# }
```

The client is cheap to clone and shares its connection pool, so pass clones around rather than rebuilding one per call.

### List sites

`sites()` returns a request builder. Await it for everything, or narrow it first:

```rust,no_run
# use rs_solar_assistant::CloudClient;
# async fn example(cloud: CloudClient) -> rs_solar_assistant::Result<()> {
let all = cloud.sites().await?;
let srne = cloud.sites().inverter("srne").limit(50).offset(20).await?;
let found = cloud.sites().search("my-s").await?;
# Ok(())
# }
```

Filters match exactly; `search` is a prefix plus full-text match and always leads the query.

| Builder method                                       | Sent as                              |
| ---------------------------------------------------- | ------------------------------------ |
| `.search("my-s")`                                    | `?q=my-s`                            |
| `.name("my-site")`                                   | `?q=name:my-site`                    |
| `.inverter("srne")`                                  | `?q=inverter:srne`                   |
| `.battery("daly")`                                   | `?q=battery:daly`                    |
| `.filter("inverter_params_output_power", 5000)`      | `?q=inverter_params_output_power:5000` |
| `.filter("last_seen_after", "2026-01-01")`           | `?q=last_seen_after:2026-01-01`      |
| `.limit(50)` / `.offset(20)`                         | `?limit=50&offset=20`                |

### Authorize a site

Returns a short-lived token and connection details. The token works for both cloud and local connections, and converts straight into
the credential the other two clients take:

```rust,no_run
# use rs_solar_assistant::{Auth, CloudClient};
# async fn example(cloud: CloudClient, site_id: u64) -> rs_solar_assistant::Result<()> {
let authorization = cloud.authorize_site(site_id).await?;
let auth = Auth::from(&authorization);
# Ok(())
# }
```

## Device - REST

### Local connection

```rust,no_run
use rs_solar_assistant::{Auth, DeviceClient};

# fn example() {
let device = DeviceClient::new("192.168.1.100", Auth::password("<web-password>"));
# }
```

### Cloud-proxied connection

```rust,no_run
use rs_solar_assistant::{Auth, DeviceClient, Scheme, cloud::AuthorizeResponse};

# fn example(authorization: AuthorizeResponse) -> rs_solar_assistant::Result<()> {
let device = DeviceClient::builder(&authorization.host, Auth::from(&authorization))
    .scheme(Scheme::Https)
    .build()?;
# Ok(())
# }
```

### Read metrics

```rust,no_run
# use rs_solar_assistant::DeviceClient;
# async fn example(device: DeviceClient) -> rs_solar_assistant::Result<()> {
let all = device.metrics().await?;

// One request per topic glob, concatenated with duplicates dropped.
let some = device.metrics().topics(["battery_1/*", "total/pv_power"]).await?;

// Skip the Home Assistant discovery superset.
let lean = device.metrics().discovery(false).await?;
# Ok(())
# }
```

### Write a metric

```rust,no_run
# use rs_solar_assistant::DeviceClient;
# async fn example(device: DeviceClient) -> rs_solar_assistant::Result<()> {
device.set_metric("inverter_1/charge_current_limit", "20").await?;
# Ok(())
# }
```

### Read system metrics

`GET /api/v1/system` reports unit-level metrics - site ID, software version, CPU temperature, free storage - in the same row shape as
`metrics()`. A unit running a build that predates the endpoint answers `404`, which is an ordinary API error: check `status()` to
detect old firmware.

```rust,no_run
# use rs_solar_assistant::DeviceClient;
# async fn example(device: DeviceClient) -> rs_solar_assistant::Result<()> {
let rows = device.system_metrics().await?;

// Typed accessors, each `None` only when the value is unset or unreadable.
// Every one does its own request, so prefer `system_metrics()` above to read several.
let site_id = device.site_id().await?; //           Option<u64>
let version = device.software_version().await?; //  Option<String>
let cpu = device.cpu_temperature().await?; //       Option<i64>  (°C)
let storage = device.free_storage().await?; //      Option<i64>  (MB)
# Ok(())
# }
```

## Device - WebSocket

### Local connection

```rust,no_run
use futures_util::StreamExt;
use rs_solar_assistant::{Auth, Socket, socket::Options};

# async fn example() -> rs_solar_assistant::Result<()> {
let mut socket = Socket::connect(Options::local(
    "192.168.1.100",
    Auth::password("<web-password>"),
))
.await?;
socket.subscribe_metrics([]).await?;

let mut metrics = socket.metrics();
while let Some(metric) = metrics.next().await {
    let metric = metric?;
    println!("{} = {} {}", metric.name, metric.value, metric.unit);
}
# Ok(())
# }
```

### Cloud connection

An `AuthorizeResponse` converts into local-first, cloud-fallback options: the local address is tried with a 500 ms budget and the proxy
picks up the failure.

```rust,no_run
# use rs_solar_assistant::{Socket, cloud::AuthorizeResponse, socket::Options};
# async fn example(authorization: AuthorizeResponse) -> rs_solar_assistant::Result<()> {
let socket = Socket::connect(Options::from(&authorization)).await?;
# Ok(())
# }
```

### Topic filters

Subscribe to specific topics, with optional server-side throttling:

```rust,no_run
# use std::time::Duration;
# use rs_solar_assistant::{Socket, TopicFilter};
# async fn example(socket: &mut Socket) -> rs_solar_assistant::Result<()> {
socket
    .subscribe_metrics([
        "total/*".into(),
        "inverter_*/load_power".into(),
        TopicFilter::new("battery_*/voltage").max_frequency(Duration::from_secs(10)),
    ])
    .await?;
# Ok(())
# }
```

With no filters the server applies its own default set:

```text
total/*
battery_*/voltage
battery_*/state_of_charge
battery_*/power
battery_*/temperature
inverter_*/pv_power
inverter_*/load_power
inverter_*/grid_power
inverter_*/device_mode
inverter_*/temperature
```

Only metrics in the `Info`, `Status`, and `Settings` groups are sent.

### Write a setting

```rust,no_run
# use rs_solar_assistant::Socket;
# async fn example(socket: &mut Socket) -> rs_solar_assistant::Result<()> {
socket.set_setting("inverter_1/power_mode", "Off grid with relay").await?;
# Ok(())
# }
```

An existing subscription is reused, so topic filters survive the write. A refusal is `Error::SettingRejected`; silence for ten seconds
is `Error::Channel` rather than a hang.

### Keeping the connection open

The socket has no background task: the 30-second heartbeat goes out on whichever call is reading it - `next_event()`, either stream, or
`set_setting()`. A socket nobody polls sends nothing, so keep one polled if it has to stay open while idle. Shorten the interval with
`Options::heartbeat_interval` when something between you and the unit drops idle connections sooner.

### Everything else on the channel

`metrics()` filters the stream down to values. `events()` yields the rest too - metric definitions, system snapshots, and any frame
this crate does not model:

```rust,no_run
use futures_util::StreamExt;
use rs_solar_assistant::{Event, Socket};

# async fn example(socket: &mut Socket) -> rs_solar_assistant::Result<()> {
let mut events = socket.events();
while let Some(event) = events.next().await {
    match event? {
        Event::Metrics(metrics) => println!("{} value(s)", metrics.len()),
        Event::Definitions(definitions) => println!("{} definition(s)", definitions.len()),
        Event::SystemMetrics(system) => println!("{} system row(s)", system.len()),
        Event::Message(message) => println!("{} / {}", message.topic, message.event),
        _ => {}
    }
}
# Ok(())
# }
```

## Logging

Requests, frames, and replies are logged through [`tracing`](https://docs.rs/tracing) at `DEBUG`, with credentials masked first: URLs
lose their `user:pass@` userinfo, and `token`, `site_key`, `api_key`, and `password` values are replaced with `[REDACTED]`. Nothing is
printed until you install a subscriber:

```rust,no_run
# fn example() {
tracing_subscriber::fmt()
    .with_env_filter("rs_solar_assistant=debug")
    .init();
# }
```

## Examples

| Example                                                 | Description                                                        |
| -------------------------------------------------------- | ------------------------------------------------------------------ |
| [`rest_read.rs`](examples/rest_read.rs)                 | Fetch all metrics once over REST and print them grouped by device  |
| [`rest_system.rs`](examples/rest_system.rs)             | Read a unit's system metrics, handling the 404/old-firmware case   |
| [`rest_set.rs`](examples/rest_set.rs)                   | Write a metric value over REST                                     |
| [`websocket_read.rs`](examples/websocket_read.rs)       | Stream live metrics until Ctrl+C                                   |
| [`websocket_set.rs`](examples/websocket_set.rs)         | Write a setting over the WebSocket                                 |
| [`cloud_sites.rs`](examples/cloud_sites.rs)             | List sites, authorize one, and read it through the cloud proxy     |

```bash
SA_HOST=192.168.1.100 SA_PASSWORD=secret cargo run --example rest_read
```

## Differences from the Python client

The behaviour is the same; the surface is Rust's.

| Python                                             | Here                                                             |
| --------------------------------------------------- | ----------------------------------------------------------------- |
| `password=` xor `token=`, checked at runtime       | `Auth::password` / `Auth::token` / `Auth::proxy`                  |
| `Metric` and `DeviceMetric`                        | one `Metric`, so REST and WebSocket rows interoperate             |
| `list_sites(client, **params)`                     | `cloud.sites()` request builder                                   |
| `get_metrics(*topics, discovery=True)`             | `device.metrics().topics([..]).discovery(..)`                     |
| `get_device_*` module-level twins                  | dropped - `DeviceClient::new(..).metrics()` is the one-shot form  |
| `get_site_id()`, `get_cpu_temperature()`, ...      | `site_id()`, `cpu_temperature()`, ...                             |
| `subscribe_metrics(handler)` + `await listen()`    | `subscribe_metrics([])` + the `metrics()` stream                  |
| `subscribe("*", "*", handler)`                     | the `events()` stream                                             |
| `scheme="https"`                                   | `Scheme::Https`                                                   |
| `Options(verbose=True)`                            | a `tracing` subscriber at `DEBUG`                                 |
| `SolarAssistantError.status`                       | `Error::status()`, plus typed variants                            |

Two behaviours the Python roadmap wanted, which fall out of the stream model: `set_setting` reuses an existing subscription instead of
re-joining empty, and it buffers the frames that arrive while it waits instead of dropping them.

## Development

```bash
cargo test --all-features   # unit, integration, and doc tests
cargo clippy --all-targets --all-features
cargo fmt --check
```

The REST suites run against [`wiremock`](https://docs.rs/wiremock); the WebSocket suite drives the real client against a scripted
Phoenix server over a real `ws://` connection. No network access is needed.

## License

Apache 2.0 - see [LICENSE](LICENSE).

This licence covers the Rust client library in this repository only. The SolarAssistant platform, including the downloadable device
software and cloud infrastructure, is proprietary and distributed under separate terms. See [NOTICE](NOTICE) for the copyright and
scope statement.
