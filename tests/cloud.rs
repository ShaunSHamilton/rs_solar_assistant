//! Cloud REST client, driven against a local mock of the cloud API.

#![cfg(feature = "cloud")]

use rs_solar_assistant::{Auth, CloudClient};
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path, path_regex},
};

mod support;

/// Cloud client pointed at a mock server that answers `GET /api/v1/sites`.
async fn sites_server(body: ResponseTemplate) -> (MockServer, CloudClient) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/sites"))
        .respond_with(body)
        .mount(&server)
        .await;
    let client = client_for(&server);
    (server, client)
}

fn client_for(server: &MockServer) -> CloudClient {
    CloudClient::builder("k")
        .base_url(server.uri())
        .build()
        .unwrap()
}

/// Query parameters of the single request the server received.
async fn query_of(server: &MockServer) -> Vec<(String, String)> {
    last_request(server)
        .await
        .url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

async fn last_request(server: &MockServer) -> Request {
    server
        .received_requests()
        .await
        .expect("the mock server records requests")
        .pop()
        .expect("a request was sent")
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

#[tokio::test]
async fn sends_the_query_the_api_expects() {
    let (server, client) = sites_server(ok(json!([]))).await;

    client
        .sites()
        .inverter("srne")
        .battery("daly")
        .search("x")
        .limit(50)
        .await
        .unwrap();

    let query = query_of(&server).await;
    assert!(query.contains(&("limit".to_owned(), "50".to_owned())));
    assert!(query.contains(&("q".to_owned(), "x inverter:srne battery:daly".to_owned())));
}

#[tokio::test]
async fn sends_no_query_when_nothing_is_filtered() {
    let (server, client) = sites_server(ok(json!([]))).await;

    client.sites().await.unwrap();

    assert!(query_of(&server).await.is_empty());
}

#[tokio::test]
async fn authenticates_with_a_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/sites"))
        .respond_with(ok(json!([])))
        .mount(&server)
        .await;
    let client = CloudClient::builder("secret-key")
        .base_url(server.uri())
        .build()
        .unwrap();

    client.sites().await.unwrap();

    let request = last_request(&server).await;
    assert_eq!(request.headers["authorization"], "Bearer secret-key");
    assert_eq!(request.url.path(), "/api/v1/sites");
}

#[tokio::test]
async fn a_trailing_slash_in_the_base_url_is_stripped() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/sites"))
        .respond_with(ok(json!([])))
        .mount(&server)
        .await;
    let client = CloudClient::builder("k")
        .base_url(format!("{}/", server.uri()))
        .build()
        .unwrap();

    client.sites().await.unwrap();

    assert_eq!(last_request(&server).await.url.path(), "/api/v1/sites");
}

#[tokio::test]
async fn parses_a_site_and_its_owner() {
    let (_server, client) = sites_server(ok(json!([{
        "id": 1,
        "name": "Home",
        "inverter": "srne",
        "inverter_count": 2,
        "owner": {"id": 9, "email": "a@b.c", "first_name": "Ann", "last_name": "Lee"},
    }])))
    .await;

    let sites = client.sites().await.unwrap();

    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].id, 1);
    assert_eq!(sites[0].name, "Home");
    assert_eq!(sites[0].inverter, "srne");
    assert_eq!(sites[0].inverter_count, 2);
    assert_eq!(sites[0].owner.email, "a@b.c");
    assert_eq!(sites[0].owner.first_name, "Ann");
}

#[tokio::test]
async fn missing_site_fields_fall_back_to_defaults() {
    let (_server, client) = sites_server(ok(json!([{"id": 5}]))).await;

    let sites = client.sites().await.unwrap();

    assert_eq!(sites[0].name, "");
    assert!(sites[0].inverter_params.is_empty());
    assert_eq!(sites[0].battery_count, 0);
    assert!(!sites[0].beta);
    assert_eq!(sites[0].owner.id, 0);
}

#[tokio::test]
async fn parses_several_sites_in_order() {
    let (_server, client) =
        sites_server(ok(json!([{"id": 1, "name": "A"}, {"id": 2, "name": "B"}]))).await;

    let names: Vec<String> = client
        .sites()
        .await
        .unwrap()
        .into_iter()
        .map(|site| site.name)
        .collect();

    assert_eq!(names, ["A", "B"]);
}

#[tokio::test]
async fn authorizes_a_site() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v1/sites/\d+/authorize$"))
        .respond_with(ok(json!({
            "host": "proxy.example",
            "site_id": 7,
            "site_name": "Home",
            "site_key": "abc",
            "token": "jwt",
            "local_ip": "192.168.1.5",
        })))
        .mount(&server)
        .await;
    let client = client_for(&server);

    let authorization = client.authorize_site(7).await.unwrap();

    assert_eq!(authorization.host, "proxy.example");
    assert_eq!(authorization.site_id, 7);
    assert_eq!(authorization.site_key, "abc");
    assert_eq!(authorization.token, "jwt");
    assert_eq!(authorization.local_ip, "192.168.1.5");
    assert_eq!(Auth::from(&authorization), Auth::proxy("jwt", 7, "abc"));

    let request = last_request(&server).await;
    assert_eq!(request.url.path(), "/api/v1/sites/7/authorize");
    assert_eq!(request.headers["authorization"], "Bearer k");
}

#[tokio::test]
async fn an_empty_authorization_is_rejected_rather_than_defaulted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok(json!({})))
        .mount(&server)
        .await;

    let error = client_for(&server).authorize_site(1).await.unwrap_err();

    assert_eq!(error.status(), None);
    let message = error.to_string();
    for field in ["host", "site_id", "site_key", "token"] {
        assert!(message.contains(field), "{message}");
    }
}

#[tokio::test]
async fn an_authorization_missing_one_field_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok(json!({
            "host": "proxy.example",
            "site_id": 7,
            "site_key": "abc",
        })))
        .mount(&server)
        .await;

    let error = client_for(&server).authorize_site(7).await.unwrap_err();

    assert!(error.to_string().contains("token"), "{error}");
}

#[tokio::test]
async fn a_non_success_status_surfaces_as_an_api_error() {
    let (_server, client) =
        sites_server(ResponseTemplate::new(403).set_body_string("forbidden")).await;

    let error = client.sites().await.unwrap_err();

    assert_eq!(error.status(), Some(403));
}

#[tokio::test]
async fn an_error_reports_the_endpoint_and_never_the_body() {
    // The body can carry a credential in free text, which key-based redaction
    // cannot scrub, so it is dropped wholesale rather than filtered.
    let (_server, client) = sites_server(
        ResponseTemplate::new(400).set_body_json(json!({"error": "denied for token=leakme"})),
    )
    .await;

    let error = client.sites().search("boom").await.unwrap_err();

    let message = error.to_string();
    assert_eq!(error.status(), Some(400));
    assert!(message.contains("GET"), "{message}");
    assert!(message.contains("/api/v1/sites"), "{message}");
    assert!(message.contains("q=boom"), "{message}");
    assert!(!message.contains("leakme"), "{message}");
}

#[tokio::test]
async fn a_malformed_body_is_an_invalid_response_not_a_panic() {
    let (_server, client) =
        sites_server(ResponseTemplate::new(200).set_body_string("<html>captive portal</html>"))
            .await;

    let error = client.sites().await.unwrap_err();

    assert_eq!(error.status(), None);
    assert!(!error.to_string().contains("None"), "{error}");
}

#[tokio::test]
async fn one_client_serves_repeated_calls() {
    let (server, client) = sites_server(ok(json!([]))).await;

    client.sites().await.unwrap();
    client.sites().await.unwrap();

    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn credentials_are_redacted_in_the_debug_log() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok(json!({
            "host": "h",
            "site_id": 1,
            "token": "supersecret",
            "site_key": "topsecret",
        })))
        .mount(&server)
        .await;
    let client = client_for(&server);

    let logs = support::capture_logs(async {
        client.authorize_site(1).await.unwrap();
    })
    .await;

    assert!(logs.contains("[REDACTED]"), "{logs}");
    assert!(!logs.contains("supersecret"), "{logs}");
    assert!(!logs.contains("topsecret"), "{logs}");
}
