use reqwest::{Client, ClientBuilder};
use serde::{Serialize, de::DeserializeOwned};

pub const DEFAULT_BASE_URL: &str = "https://solar-assistant.io";
const TIMEOUT: u16 = 10;
const PAGINATION_KEYS: [&str; 2] = ["limit", "offset"];
const SEARCH_KEY: &str = "search";
const SENSITIVE_FIELDS: [&str; 4] = ["token", "site_key", "api_key", "password"];
const SITES_PATH: &str = "/api/v1/sites";

pub struct AuthorizeResponse {}

pub struct Site {}

pub struct SiteOwner {}

pub struct SolarAssistantClient {
    pub api_key: String,
    pub base_url: String,
    pub verbose: bool,
    http: Client,
}

pub struct RequestBuilder {
    inner: reqwest::RequestBuilder,
}

impl RequestBuilder {
    pub fn query<T>(self, query: &T) -> RequestBuilder
    where
        T: Serialize + ?Sized,
    {
        Self {
            inner: self.inner.query(query),
        }
    }

    pub async fn send(self) {
        self.inner.send()
    }
}

impl SolarAssistantClient {
    pub fn new(api_key: &str) -> Self {
        let http = ClientBuilder::new()
            .build()
            .expect("tls backend unable to init");

        SolarAssistantClient {
            api_key: api_key.to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
            verbose: false,
            http,
        }
    }

    /// GET <path> with optional filter/search params.
    ///
    /// ```rust
    /// client.get("/path").query([("some","a")]).send().await?;
    /// ```
    /// Pagination keys (limit, offset) are sent as top-level query params.
    /// A "search" value is sent as a bare term in ?q= (server-side prefix +
    /// full-text match). All other keys become "key:value" filters joined
    /// into the same ?q=.
    pub async fn get<T: DeserializeOwned, Q: Serialize + ?Sized>(
        self,
        path: &str,
        params: &Q,
    ) -> Result<T, crate::error::Error> {
        let res = self.http.get(path).query(params).send().await?;
        let body = res.bytes().await?;
        serde_json::from_slice(&body).map_err(|e| e.into())
    }

    /// TODO: NEw struct returned from `get`
    pub async fn query() {
        todo!()
    }

    pub async fn send() {}
}

pub async fn authorize_site() {}

pub async fn list_sites() {}
