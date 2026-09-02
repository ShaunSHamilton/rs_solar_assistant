//! Client for the SolarAssistant cloud API.
//!
//! Every endpoint needs an API key; generate one at
//! [solar-assistant.io/user/edit#api](https://solar-assistant.io/user/edit#api).
//!
//! ```no_run
//! use rs_solar_assistant::CloudClient;
//!
//! # async fn example() -> rs_solar_assistant::Result<()> {
//! let cloud = CloudClient::new("<api-key>");
//!
//! let sites = cloud.sites().inverter("srne").limit(50).await?;
//! let authorization = cloud.authorize_site(sites[0].id).await?;
//! # Ok(())
//! # }
//! ```

use std::{
    fmt,
    future::{Future, IntoFuture},
    pin::Pin,
    time::Duration,
};

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    Auth,
    error::{Error, Result},
    redact::{REDACTED, safe_body, safe_query_url},
    serde_util::null_default,
};

/// Base URL of the public SolarAssistant cloud API.
pub const DEFAULT_BASE_URL: &str = "https://solar-assistant.io";

/// Total time allowed per cloud request unless overridden.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

const SITES_PATH: &str = "/api/v1/sites";

/// Authenticated client for the SolarAssistant cloud API.
///
/// Cloning is cheap and shares the underlying connection pool, so pass clones
/// around rather than rebuilding a client per call.
#[derive(Clone)]
pub struct Client {
    api_key: String,
    base_url: String,
    timeout: Duration,
    http: reqwest::Client,
}

/// Builder for a [`Client`] with a non-default base URL, timeout, or
/// pre-configured [`reqwest::Client`].
#[derive(Debug, Clone)]
pub struct ClientBuilder {
    api_key: String,
    base_url: String,
    timeout: Duration,
    http: Option<reqwest::Client>,
}

impl Client {
    /// Client for the public cloud API.
    ///
    /// # Panics
    ///
    /// If the TLS backend cannot be initialised, matching
    /// [`reqwest::Client::new`]. Use [`Client::builder`] to handle that
    /// failure instead.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::builder(api_key)
            .build()
            .expect("failed to initialise the TLS backend")
    }

    /// Builder for a client with a custom base URL, timeout, or HTTP client.
    pub fn builder(api_key: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            http: None,
        }
    }

    /// Base URL this client talks to, without a trailing slash.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Sites reachable with this API key.
    ///
    /// The returned request is a builder; `await` it directly for every site,
    /// or narrow it first:
    ///
    /// ```no_run
    /// # use rs_solar_assistant::CloudClient;
    /// # async fn example(cloud: CloudClient) -> rs_solar_assistant::Result<()> {
    /// let all = cloud.sites().await?;
    /// let srne = cloud.sites().inverter("srne").limit(50).await?;
    /// let found = cloud.sites().search("my-s").await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn sites(&self) -> SitesRequest<'_> {
        SitesRequest {
            client: self,
            query: SiteQuery::default(),
        }
    }

    /// Short-lived token and connection details for one site.
    ///
    /// The token works for both cloud-proxied and local connections; feed it
    /// to a [`device::Client`](crate::device::Client) or a
    /// [`Socket`](crate::socket::Socket) through [`Auth::from`].
    pub async fn authorize_site(&self, site_id: u64) -> Result<AuthorizeResponse> {
        let body = self
            .post(&format!("{SITES_PATH}/{site_id}/authorize"))
            .await?;
        parse_json(&body, "authorization")
    }

    /// `GET <path>`, returning the raw response body.
    ///
    /// An escape hatch for endpoints this crate does not model yet. `query` is
    /// anything `reqwest` can serialise, such as a slice of pairs; see
    /// [`SitesRequest`] for the `?q=` filter syntax the list endpoints use.
    ///
    /// ```no_run
    /// # use rs_solar_assistant::CloudClient;
    /// # async fn example(cloud: CloudClient) -> rs_solar_assistant::Result<()> {
    /// let body = cloud.get("/api/v1/sites", &[("limit", "1")]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get<Q>(&self, path: &str, query: &Q) -> Result<Vec<u8>>
    where
        Q: Serialize + ?Sized,
    {
        self.send(Method::GET, "GET", path, query).await
    }

    /// `POST <path>` with no request body, returning the raw response body.
    pub async fn post(&self, path: &str) -> Result<Vec<u8>> {
        self.send(Method::POST, "POST", path, &()).await
    }

    async fn send<Q>(
        &self,
        method: Method,
        name: &'static str,
        path: &str,
        query: &Q,
    ) -> Result<Vec<u8>>
    where
        Q: Serialize + ?Sized,
    {
        // Built before sending so the log and any error carry the encoded
        // query, which is what tells two otherwise identical requests apart.
        let request = self
            .http
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(&self.api_key)
            .timeout(self.timeout)
            .query(query)
            .build()?;
        let url = request.url().to_string();
        tracing::debug!(target: "rs_solar_assistant::cloud", "> {name} {}", safe_query_url(&url));

        let response = self.http.execute(request).await?;
        let status = response.status();
        let body = response.bytes().await?;
        tracing::debug!(target: "rs_solar_assistant::cloud", "< {status} {}", safe_body(&body));

        if !status.is_success() {
            return Err(Error::api(name, &url, status.as_u16()));
        }
        Ok(body.to_vec())
    }
}

impl ClientBuilder {
    /// Point the client at another deployment, e.g. a staging environment.
    ///
    /// A trailing slash is stripped so paths join cleanly.
    #[must_use]
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        base_url
            .trim_end_matches('/')
            .clone_into(&mut self.base_url);
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
            api_key: self.api_key,
            base_url: self.base_url,
            timeout: self.timeout,
            http,
        })
    }
}

/// Hides the API key from logs and panic output.
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("api_key", &REDACTED)
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// A pending `GET /api/v1/sites`, narrowed by filters.
///
/// Filters match exactly; [`search`](Self::search) is a prefix plus full-text
/// match. `limit` and `offset` page the result; every other key is sent as a
/// `key:value` term in the server's `?q=` query, with the search term leading.
#[derive(Debug)]
pub struct SitesRequest<'a> {
    client: &'a Client,
    query: SiteQuery,
}

#[derive(Debug, Default, Clone)]
struct SiteQuery {
    limit: Option<u64>,
    offset: Option<u64>,
    search: Option<String>,
    filters: Vec<(String, String)>,
}

impl SitesRequest<'_> {
    /// Maximum number of sites to return.
    #[must_use]
    pub fn limit(mut self, limit: u64) -> Self {
        self.query.limit = Some(limit);
        self
    }

    /// Number of sites to skip.
    #[must_use]
    pub fn offset(mut self, offset: u64) -> Self {
        self.query.offset = Some(offset);
        self
    }

    /// Prefix plus full-text search, e.g. `"my-s"`.
    ///
    /// Leads the `?q=` query however late it is added, matching the server's
    /// expectation that the bare term comes first.
    #[must_use]
    pub fn search(mut self, term: impl Into<String>) -> Self {
        self.query.search = Some(term.into());
        self
    }

    /// Exact-match filter on any field the API supports, e.g.
    /// `filter("last_seen_after", "2026-01-01")`.
    #[must_use]
    pub fn filter(mut self, key: impl Into<String>, value: impl fmt::Display) -> Self {
        self.query.filters.push((key.into(), value.to_string()));
        self
    }

    /// Exact-match filter on the site name.
    #[must_use]
    pub fn name(self, name: impl Into<String>) -> Self {
        self.filter("name", name.into())
    }

    /// Exact-match filter on the inverter model, e.g. `"srne"`.
    #[must_use]
    pub fn inverter(self, inverter: impl Into<String>) -> Self {
        self.filter("inverter", inverter.into())
    }

    /// Exact-match filter on the battery model, e.g. `"daly"`.
    #[must_use]
    pub fn battery(self, battery: impl Into<String>) -> Self {
        self.filter("battery", battery.into())
    }

    /// Sends the request.
    ///
    /// Equivalent to awaiting the request directly.
    pub async fn send(self) -> Result<Vec<Site>> {
        let body = self.client.get(SITES_PATH, &self.query.to_params()).await?;
        parse_json(&body, "site list")
    }
}

impl<'a> IntoFuture for SitesRequest<'a> {
    type Output = Result<Vec<Site>>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

impl SiteQuery {
    /// Flattens the query the way the API expects it: pagination as top-level
    /// parameters, everything else folded into one `q` term list.
    fn to_params(&self) -> Vec<(&'static str, String)> {
        let mut params = Vec::new();
        if let Some(limit) = self.limit {
            params.push(("limit", limit.to_string()));
        }
        if let Some(offset) = self.offset {
            params.push(("offset", offset.to_string()));
        }

        let terms: Vec<String> = self
            .search
            .iter()
            .cloned()
            .chain(
                self.filters
                    .iter()
                    .map(|(key, value)| format!("{key}:{value}")),
            )
            .collect();
        if !terms.is_empty() {
            params.push(("q", terms.join(" ")));
        }
        params
    }
}

/// A site registered with the cloud account.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Site {
    /// Numeric site identifier.
    pub id: u64,
    /// Name the owner gave the site.
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    /// Inverter model, e.g. `srne`.
    #[serde(deserialize_with = "null_default")]
    pub inverter: String,
    /// Number of inverters attached to the unit.
    pub inverter_count: u32,
    /// Model-specific inverter settings reported by the unit.
    #[serde(deserialize_with = "null_default")]
    pub inverter_params: Map<String, Value>,
    /// Battery model, e.g. `daly`.
    #[serde(deserialize_with = "null_default")]
    pub battery: String,
    /// Number of batteries attached to the unit.
    pub battery_count: u32,
    /// Model-specific battery settings reported by the unit.
    #[serde(deserialize_with = "null_default")]
    pub battery_params: Map<String, Value>,
    /// Cloud proxy the site connects through.
    #[serde(deserialize_with = "null_default")]
    pub proxy: String,
    /// CPU architecture of the unit.
    #[serde(deserialize_with = "null_default")]
    pub arch: String,
    /// Board the unit runs on, e.g. `rpi4`.
    #[serde(deserialize_with = "null_default")]
    pub board: String,
    /// Whether the unit is on the beta release channel.
    pub beta: bool,
    /// Build date of the software the unit runs.
    #[serde(deserialize_with = "null_default")]
    pub build_date: String,
    /// When the cloud last heard from the unit.
    #[serde(deserialize_with = "null_default")]
    pub last_seen_at: String,
    /// Address of the unit on its own network, when known.
    #[serde(deserialize_with = "null_default")]
    pub local_ip: String,
    /// Account the site belongs to.
    #[serde(deserialize_with = "null_default")]
    pub owner: SiteOwner,
}

/// Account a [`Site`] belongs to.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SiteOwner {
    /// Numeric account identifier.
    pub id: u64,
    /// Account email address.
    #[serde(deserialize_with = "null_default")]
    pub email: String,
    /// Account first name.
    #[serde(deserialize_with = "null_default")]
    pub first_name: String,
    /// Account last name.
    #[serde(deserialize_with = "null_default")]
    pub last_name: String,
}

/// Short-lived credentials for reaching one site.
///
/// Convert it into an [`Auth`] to use it:
///
/// ```no_run
/// # use rs_solar_assistant::{Auth, CloudClient};
/// # async fn example(cloud: CloudClient) -> rs_solar_assistant::Result<()> {
/// let authorization = cloud.authorize_site(7).await?;
/// let auth = Auth::from(&authorization);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
#[non_exhaustive]
pub struct AuthorizeResponse {
    /// Hostname (or `host:port`) of the cloud proxy fronting the site.
    #[serde(deserialize_with = "null_default")]
    pub host: String,
    /// Numeric site identifier.
    pub site_id: u64,
    /// Name the owner gave the site.
    #[serde(deserialize_with = "null_default")]
    pub site_name: String,
    /// Key the proxy authenticates the site with.
    #[serde(deserialize_with = "null_default")]
    pub site_key: String,
    /// Short-lived JWT, valid for both cloud and local connections.
    #[serde(deserialize_with = "null_default")]
    pub token: String,
    /// Address of the unit on its own network, when known.
    #[serde(deserialize_with = "null_default")]
    pub local_ip: String,
}

/// Hides the token and site key, which are credentials.
impl fmt::Debug for AuthorizeResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizeResponse")
            .field("host", &self.host)
            .field("site_id", &self.site_id)
            .field("site_name", &self.site_name)
            .field("site_key", &REDACTED)
            .field("token", &REDACTED)
            .field("local_ip", &self.local_ip)
            .finish()
    }
}

impl From<&AuthorizeResponse> for Auth {
    fn from(authorization: &AuthorizeResponse) -> Self {
        Self::proxy(
            authorization.token.clone(),
            authorization.site_id,
            authorization.site_key.clone(),
        )
    }
}

impl From<AuthorizeResponse> for Auth {
    fn from(authorization: AuthorizeResponse) -> Self {
        Self::proxy(
            authorization.token,
            authorization.site_id,
            authorization.site_key,
        )
    }
}

/// Parses a response body, reporting the shape that was expected rather than
/// serde's positional complaint.
fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(body)
        .map_err(|err| Error::InvalidResponse(format!("could not read the {what}: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(
        build: impl FnOnce(SitesRequest<'_>) -> SitesRequest<'_>,
    ) -> Vec<(&'static str, String)> {
        let client = Client::new("k");
        build(client.sites()).query.to_params()
    }

    #[test]
    fn search_becomes_a_leading_bare_term() {
        assert_eq!(
            params(|sites| sites.search("my-site")),
            [("q", "my-site".to_owned())]
        );
    }

    #[test]
    fn a_filter_becomes_a_key_value_term() {
        assert_eq!(
            params(|sites| sites.inverter("srne")),
            [("q", "inverter:srne".to_owned())]
        );
    }

    #[test]
    fn pagination_stays_top_level() {
        assert_eq!(
            params(|sites| sites.limit(50).offset(20)),
            [("limit", "50".to_owned()), ("offset", "20".to_owned())]
        );
    }

    #[test]
    fn search_leads_however_late_it_is_added() {
        assert_eq!(
            params(|sites| sites.inverter("srne").battery("daly").search("x")),
            [("q", "x inverter:srne battery:daly".to_owned())]
        );
    }

    #[test]
    fn search_and_pagination_coexist() {
        assert_eq!(
            params(|sites| sites.search("home").limit(10)),
            [("limit", "10".to_owned()), ("q", "home".to_owned())]
        );
    }

    #[test]
    fn no_filters_send_no_query() {
        assert!(params(|sites| sites).is_empty());
    }

    #[test]
    fn base_url_loses_its_trailing_slash() {
        let client = Client::builder("k")
            .base_url("http://example.test/")
            .build()
            .unwrap();
        assert_eq!(client.base_url(), "http://example.test");
    }

    #[test]
    fn debug_never_prints_the_api_key() {
        let rendered = format!("{:?}", Client::new("super-secret-key"));
        assert!(!rendered.contains("super-secret-key"), "{rendered}");
    }

    #[test]
    fn null_params_read_as_empty_maps() {
        let site: Site =
            serde_json::from_str(r#"{"id": 1, "inverter_params": null, "battery_params": null}"#)
                .unwrap();
        assert!(site.inverter_params.is_empty());
        assert!(site.battery_params.is_empty());
        assert_eq!(site.owner, SiteOwner::default());
    }

    #[test]
    fn authorization_converts_into_proxy_credentials() {
        let authorization = AuthorizeResponse {
            token: "jwt".to_owned(),
            site_id: 7,
            site_key: "key".to_owned(),
            ..AuthorizeResponse::default()
        };
        assert_eq!(Auth::from(&authorization), Auth::proxy("jwt", 7, "key"));
    }

    #[test]
    fn authorization_debug_hides_its_credentials() {
        let rendered = format!(
            "{:?}",
            AuthorizeResponse {
                token: "jwt-secret".to_owned(),
                site_key: "key-secret".to_owned(),
                ..AuthorizeResponse::default()
            }
        );
        assert!(!rendered.contains("jwt-secret"), "{rendered}");
        assert!(!rendered.contains("key-secret"), "{rendered}");
    }
}
