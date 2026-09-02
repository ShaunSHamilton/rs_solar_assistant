//! Client for a SolarAssistant unit's own REST API.
//!
//! Reaches a unit two ways: directly on its local network with the web
//! password, or through the cloud proxy with a token from the cloud client's
//! `authorize_site`.
//!
//! ```no_run
//! use rs_solar_assistant::{Auth, DeviceClient};
//!
//! # async fn example() -> rs_solar_assistant::Result<()> {
//! let device = DeviceClient::new("192.168.1.100", Auth::password("<web-password>"));
//!
//! let metrics = device.metrics().topic("battery_1/*").await?;
//! device.set_metric("inverter_1/charge_current_limit", "40").await?;
//! # Ok(())
//! # }
//! ```

use std::{
    collections::HashSet,
    fmt,
    future::{Future, IntoFuture},
    pin::Pin,
    time::Duration,
};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};

use crate::{
    Auth, Metric,
    error::{Error, Result},
    redact::safe_url,
};

/// Username the unit expects alongside its web password.
const REST_USERNAME: &str = "admin";

/// Total time allowed per request unless overridden.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

const METRICS_PATH: &str = "/api/v1/metrics";
const SYSTEM_PATH: &str = "/api/v1/system";

const SITE_ID_TOPIC: &str = "system/site_id";
const SOFTWARE_VERSION_TOPIC: &str = "system/software_version";
const CPU_TEMPERATURE_TOPIC: &str = "system/cpu_temperature";
const FREE_STORAGE_TOPIC: &str = "system/free_storage";

/// Everything except the unreserved characters is percent-encoded, so a topic
/// glob such as `battery_1/*` survives the round trip intact.
const TOPIC: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Transport used to reach the unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scheme {
    /// Plain HTTP, the default for a unit on your own network.
    #[default]
    Http,
    /// HTTPS, required for the cloud proxy.
    Https,
}

impl Scheme {
    /// The scheme as it appears in a URL.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// REST client for one SolarAssistant unit.
///
/// Cloning is cheap and shares the underlying connection pool.
#[derive(Clone, Debug)]
pub struct Client {
    host: String,
    auth: Auth,
    scheme: Scheme,
    timeout: Duration,
    http: reqwest::Client,
}

/// Builder for a [`Client`] that needs HTTPS, another timeout, or a
/// pre-configured [`reqwest::Client`].
#[derive(Clone, Debug)]
pub struct ClientBuilder {
    host: String,
    auth: Auth,
    scheme: Scheme,
    timeout: Duration,
    http: Option<reqwest::Client>,
}

impl Client {
    /// Client for a unit at `host`, over plain HTTP.
    ///
    /// `host` is an address or hostname, optionally with a port
    /// (`192.168.1.100`, `unit.local:8080`).
    ///
    /// # Panics
    ///
    /// If the TLS backend cannot be initialised, matching
    /// [`reqwest::Client::new`]. Use [`Client::builder`] to handle that
    /// failure instead.
    #[must_use]
    pub fn new(host: impl Into<String>, auth: Auth) -> Self {
        Self::builder(host, auth)
            .build()
            .expect("failed to initialise the TLS backend")
    }

    /// Builder for a client with a custom scheme, timeout, or HTTP client.
    pub fn builder(host: impl Into<String>, auth: Auth) -> ClientBuilder {
        ClientBuilder {
            host: host.into(),
            auth,
            scheme: Scheme::Http,
            timeout: DEFAULT_TIMEOUT,
            http: None,
        }
    }

    /// Host this client talks to.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Metrics reported by the unit, from `GET /api/v1/metrics`.
    ///
    /// The returned request is a builder; `await` it directly for every
    /// metric, or narrow it by topic first:
    ///
    /// ```no_run
    /// # use rs_solar_assistant::DeviceClient;
    /// # async fn example(device: DeviceClient) -> rs_solar_assistant::Result<()> {
    /// let all = device.metrics().await?;
    /// let some = device.metrics().topic("battery_1/*").topic("total/pv_power").await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn metrics(&self) -> MetricsRequest<'_> {
        MetricsRequest {
            client: self,
            topics: Vec::new(),
            discovery: true,
        }
    }

    /// Writes a setting through `POST /api/v1/metrics`.
    ///
    /// `topic` is MQTT-style, e.g. `inverter_1/power_mode`; `value` is the new
    /// value as a string, in the same form the unit reports it.
    pub async fn set_metric(&self, topic: &str, value: &str) -> Result<()> {
        let url = self.url(METRICS_PATH, "");
        tracing::debug!(target: "rs_solar_assistant::device", "> POST {} {topic}={value}", safe_url(&url));

        let response = self
            .authenticated(self.http.post(&url))
            .json(&json!({ "topic": topic, "value": value }))
            .timeout(self.timeout)
            .send()
            .await?;

        let status = response.status();
        tracing::debug!(target: "rs_solar_assistant::device", "< {status}");
        if status.is_success() {
            Ok(())
        } else {
            Err(Error::api("POST", &url, status.as_u16()))
        }
    }

    /// Unit-level metrics from `GET /api/v1/system`: site ID, software
    /// version, CPU temperature, free storage.
    ///
    /// A unit running a build that predates the endpoint answers `404`, which
    /// surfaces as an [`Error::Api`] with `status() == Some(404)` like any
    /// other failure - match on that to detect old firmware.
    pub async fn system_metrics(&self) -> Result<Vec<Metric>> {
        self.rows(&self.url(SYSTEM_PATH, "")).await
    }

    /// The unit's numeric site ID, or `None` when it is unregistered.
    ///
    /// Feed it to [`Auth::proxy`] when setting up a cloud-proxied connection.
    /// Each of these accessors fetches `/api/v1/system` on its own; to read
    /// several, call [`system_metrics`](Self::system_metrics) once instead.
    pub async fn site_id(&self) -> Result<Option<u64>> {
        Ok(self
            .system_value(SITE_ID_TOPIC)
            .await?
            .and_then(|v| v.as_u64()))
    }

    /// The build the unit is running, e.g. `2026-06-15`, or `None` when unset.
    pub async fn software_version(&self) -> Result<Option<String>> {
        Ok(self
            .system_value(SOFTWARE_VERSION_TOPIC)
            .await?
            .and_then(|value| value.as_str().map(str::trim).map(ToOwned::to_owned))
            .filter(|version| !version.is_empty()))
    }

    /// The unit's CPU temperature in °C, or `None` when the sensor read failed.
    ///
    /// These values are integers on the wire, so a float is reported as `None`
    /// rather than silently truncated.
    pub async fn cpu_temperature(&self) -> Result<Option<i64>> {
        Ok(self
            .system_value(CPU_TEMPERATURE_TOPIC)
            .await?
            .and_then(|value| value.as_i64()))
    }

    /// Free root-filesystem storage in MB, or `None` when the read failed.
    pub async fn free_storage(&self) -> Result<Option<i64>> {
        Ok(self
            .system_value(FREE_STORAGE_TOPIC)
            .await?
            .and_then(|value| value.as_i64()))
    }

    /// Value of one `/api/v1/system` row, or `None` when the row is absent.
    async fn system_value(&self, topic: &str) -> Result<Option<Value>> {
        Ok(self
            .system_metrics()
            .await?
            .into_iter()
            .find(|metric| metric.topic == topic)
            .map(|metric| metric.value))
    }

    /// `GET url` and parse the JSON array of metric rows it answers with.
    async fn rows(&self, url: &str) -> Result<Vec<Metric>> {
        tracing::debug!(target: "rs_solar_assistant::device", "> GET {}", safe_url(url));

        let response = self
            .authenticated(self.http.get(url))
            .timeout(self.timeout)
            .send()
            .await?;

        let status = response.status();
        tracing::debug!(target: "rs_solar_assistant::device", "< {status}");
        if !status.is_success() {
            return Err(Error::api("GET", url, status.as_u16()));
        }

        let body = response.bytes().await?;
        let parsed: Value = serde_json::from_slice(&body)
            .map_err(|_| Error::invalid_response("invalid JSON", url))?;

        // A unit behind a captive portal or a confused proxy can answer 200
        // with an error envelope; that is a failure, not an empty result.
        let Value::Array(rows) = parsed else {
            return Err(Error::invalid_response(
                "expected a JSON array of objects",
                url,
            ));
        };
        rows.into_iter()
            .map(|row| {
                serde_json::from_value(row)
                    .map_err(|_| Error::invalid_response("expected a JSON array of objects", url))
            })
            .collect()
    }

    /// Applies the credential this client was built with.
    fn authenticated(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            Auth::Password(password) => request.basic_auth(REST_USERNAME, Some(password)),
            Auth::Token {
                token,
                site_id,
                site_key,
            } => {
                let mut request = request.bearer_auth(token);
                if let Some(site_id) = site_id {
                    request = request.header("site-id", site_id.to_string());
                }
                if let Some(site_key) = site_key {
                    request = request.header("site-key", site_key);
                }
                request
            }
        }
    }

    fn url(&self, path: &str, query: &str) -> String {
        format!("{}://{}{path}{query}", self.scheme, self.host)
    }
}

impl ClientBuilder {
    /// Transport to use. Defaults to [`Scheme::Http`]; the cloud proxy needs
    /// [`Scheme::Https`].
    #[must_use]
    pub fn scheme(mut self, scheme: Scheme) -> Self {
        self.scheme = scheme;
        self
    }

    /// Total time allowed per request. Defaults to [`DEFAULT_TIMEOUT`].
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Reuse a pre-configured [`reqwest::Client`] - for a proxy, a custom
    /// certificate store, or a shared connection pool.
    #[must_use]
    pub fn http(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }

    /// Builds the client, initialising an HTTP client if none was supplied.
    pub fn build(self) -> Result<Client> {
        let http = match self.http {
            Some(http) => http,
            None => reqwest::Client::builder().build()?,
        };
        Ok(Client {
            host: self.host,
            auth: self.auth,
            scheme: self.scheme,
            timeout: self.timeout,
            http,
        })
    }
}

/// A pending `GET /api/v1/metrics`, narrowed by topic.
///
/// Each topic is a separate request to the unit - the endpoint takes one
/// `topic` at a time - and the results are concatenated with duplicates
/// dropped, keeping the order in which they were first seen.
#[derive(Debug)]
pub struct MetricsRequest<'a> {
    client: &'a Client,
    topics: Vec<String>,
    discovery: bool,
}

impl MetricsRequest<'_> {
    /// Restricts the request to one topic glob, e.g. `battery_1/*` or
    /// `total/pv_power`. Call repeatedly to fetch several.
    #[must_use]
    pub fn topic(mut self, topic: impl Into<String>) -> Self {
        self.topics.push(topic.into());
        self
    }

    /// Restricts the request to several topic globs at once.
    #[must_use]
    pub fn topics<I, T>(mut self, topics: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        self.topics.extend(topics.into_iter().map(Into::into));
        self
    }

    /// Whether to ask for the Home Assistant discovery superset - `platform`,
    /// `device_class`, `min`, `max`, `options`, and friends. On by default.
    #[must_use]
    pub fn discovery(mut self, discovery: bool) -> Self {
        self.discovery = discovery;
        self
    }

    /// Sends the request.
    ///
    /// Equivalent to awaiting the request directly.
    pub async fn send(self) -> Result<Vec<Metric>> {
        if self.topics.is_empty() {
            return self.client.rows(&self.url(None)).await;
        }

        let mut seen = HashSet::new();
        let mut metrics = Vec::new();
        for topic in &self.topics {
            for metric in self.client.rows(&self.url(Some(topic))).await? {
                if seen.insert(metric.topic.clone()) {
                    metrics.push(metric);
                }
            }
        }
        Ok(metrics)
    }

    /// Builds the request URL, matching the bare `?discovery` flag the unit
    /// expects (it tests for the key, not for a value).
    fn url(&self, topic: Option<&str>) -> String {
        let mut query = String::new();
        if self.discovery {
            query.push_str("?discovery");
        }
        if let Some(topic) = topic {
            query.push(if query.is_empty() { '?' } else { '&' });
            query.push_str("topic=");
            query.extend(utf8_percent_encode(topic, TOPIC));
        }
        self.client.url(METRICS_PATH, &query)
    }
}

impl<'a> IntoFuture for MetricsRequest<'a> {
    type Output = Result<Vec<Metric>>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Client {
        Client::new("192.168.1.100", Auth::password("pw"))
    }

    #[test]
    fn asks_for_discovery_by_default() {
        assert_eq!(
            client().metrics().url(None),
            "http://192.168.1.100/api/v1/metrics?discovery"
        );
    }

    #[test]
    fn discovery_can_be_turned_off() {
        assert_eq!(
            client().metrics().discovery(false).url(None),
            "http://192.168.1.100/api/v1/metrics"
        );
    }

    #[test]
    fn a_topic_glob_is_percent_encoded() {
        assert_eq!(
            client().metrics().url(Some("battery_1/*")),
            "http://192.168.1.100/api/v1/metrics?discovery&topic=battery_1%2F%2A"
        );
        assert_eq!(
            client()
                .metrics()
                .discovery(false)
                .url(Some("total/pv_power")),
            "http://192.168.1.100/api/v1/metrics?topic=total%2Fpv_power"
        );
    }

    #[test]
    fn https_and_a_port_survive_url_building() {
        let client = Client::builder("proxy.example:8443", Auth::token("jwt"))
            .scheme(Scheme::Https)
            .build()
            .unwrap();
        assert_eq!(
            client.metrics().discovery(false).url(None),
            "https://proxy.example:8443/api/v1/metrics"
        );
    }
}
