//! Device REST client, driven against a local mock of a unit.

#![cfg(feature = "device")]

use std::collections::HashMap;

use base64::{Engine, engine::general_purpose::STANDARD};
use rs_solar_assistant::{Auth, DeviceClient, Metric};
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path, query_param},
};

mod support;

/// A metrics row as a unit reports it.
fn row(topic: &str) -> Value {
    json!({
        "topic": topic,
        "name": "PV Power",
        "unit": "W",
        "value": 1234,
        "group": "Info",
        "device": "total",
        "number": null,
    })
}

/// A `/api/v1/system` row, which carries no `number` or `unit`.
fn system_row(topic: &str, value: Value) -> Value {
    json!({
        "topic": topic,
        "device": "system",
        "group": "Info",
        "name": "Site ID",
        "value": value,
    })
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

fn client_for(server: &MockServer, auth: Auth) -> DeviceClient {
    DeviceClient::new(host_of(server), auth)
}

/// `host:port` of the mock server, as a caller would pass it.
fn host_of(server: &MockServer) -> String {
    server.uri().trim_start_matches("http://").to_owned()
}

async fn last_request(server: &MockServer) -> Request {
    server
        .received_requests()
        .await
        .expect("the mock server records requests")
        .pop()
        .expect("a request was sent")
}

/// Mock server answering `GET /api/v1/metrics` with `body`.
async fn metrics_server(body: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/metrics"))
        .respond_with(body)
        .mount(&server)
        .await;
    server
}

/// Mock server answering `GET /api/v1/system` with `body`.
async fn system_server(body: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/system"))
        .respond_with(body)
        .mount(&server)
        .await;
    server
}

/// Mock unit whose `/api/v1/system` answers with `rows`.
///
/// The server is returned alongside the client: dropping it shuts the mock
/// down, so callers have to keep it alive.
async fn system_metrics(rows: Value) -> (MockServer, DeviceClient) {
    let server = system_server(ok(rows)).await;
    let client = client_for(&server, Auth::password("x"));
    (server, client)
}

#[tokio::test]
async fn a_password_becomes_http_basic_auth() {
    let server = metrics_server(ok(json!([]))).await;

    client_for(&server, Auth::password("web-pw"))
        .metrics()
        .await
        .unwrap();

    let header = last_request(&server).await.headers["authorization"]
        .to_str()
        .unwrap()
        .to_owned();
    let encoded = header.strip_prefix("Basic ").expect("basic auth");
    let decoded = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
    assert_eq!(decoded, "admin:web-pw");
}

#[tokio::test]
async fn a_proxy_token_becomes_a_bearer_and_site_headers() {
    let server = metrics_server(ok(json!([]))).await;

    client_for(&server, Auth::proxy("jwt", 42, "skey"))
        .metrics()
        .await
        .unwrap();

    let headers = last_request(&server).await.headers;
    assert_eq!(headers["authorization"], "Bearer jwt");
    assert_eq!(headers["site-id"], "42");
    assert_eq!(headers["site-key"], "skey");
}

#[tokio::test]
async fn a_plain_token_sends_no_site_headers() {
    let server = metrics_server(ok(json!([]))).await;

    client_for(&server, Auth::token("jwt"))
        .metrics()
        .await
        .unwrap();

    let headers = last_request(&server).await.headers;
    assert_eq!(headers["authorization"], "Bearer jwt");
    assert!(!headers.contains_key("site-id"));
    assert!(!headers.contains_key("site-key"));
}

#[tokio::test]
async fn parses_rows_into_metrics() {
    let server = metrics_server(ok(json!([row("total/pv_power")]))).await;

    let metrics = client_for(&server, Auth::password("x"))
        .metrics()
        .await
        .unwrap();

    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].topic, "total/pv_power");
    assert_eq!(metrics[0].name, "PV Power");
    assert_eq!(metrics[0].as_i64(), Some(1234));
    assert_eq!(metrics[0].unit, "W");
    assert_eq!(metrics[0].device, "total");
}

#[tokio::test]
async fn discovery_fields_come_through() {
    let mut discovery_row = row("total/pv_power");
    discovery_row["platform"] = json!("sensor");
    discovery_row["device_class"] = json!("power");
    discovery_row["min"] = json!(0);
    discovery_row["max"] = json!(5000);
    let server = metrics_server(ok(json!([discovery_row]))).await;

    let metrics = client_for(&server, Auth::password("x"))
        .metrics()
        .await
        .unwrap();

    assert_eq!(metrics[0].platform.as_deref(), Some("sensor"));
    assert_eq!(metrics[0].device_class.as_deref(), Some("power"));
    assert_eq!(metrics[0].min, Some(0.0));
    assert_eq!(metrics[0].max, Some(5000.0));
}

#[tokio::test]
async fn the_discovery_flag_is_sent_by_default_and_can_be_dropped() {
    let server = metrics_server(ok(json!([]))).await;
    let client = client_for(&server, Auth::password("x"));

    client.metrics().await.unwrap();
    assert!(
        last_request(&server)
            .await
            .url
            .query()
            .unwrap()
            .contains("discovery")
    );

    client.metrics().discovery(false).await.unwrap();
    assert_eq!(last_request(&server).await.url.query(), None);
}

#[tokio::test]
async fn a_topic_glob_survives_the_wire() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/metrics"))
        .and(query_param("topic", "battery_1/*"))
        .respond_with(ok(json!([row("battery_1/voltage")])))
        .mount(&server)
        .await;

    let metrics = client_for(&server, Auth::password("x"))
        .metrics()
        .topic("battery_1/*")
        .await
        .unwrap();

    assert_eq!(metrics[0].topic, "battery_1/voltage");
}

#[tokio::test]
async fn several_topics_are_fetched_separately_and_deduplicated() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("topic", "total/*"))
        .respond_with(ok(json!([row("total/pv_power"), row("total/load_power")])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("topic", "battery_1/*"))
        // Overlaps the first response: the duplicate must be dropped.
        .respond_with(ok(json!([row("total/pv_power"), row("battery_1/voltage")])))
        .mount(&server)
        .await;

    let metrics = client_for(&server, Auth::password("x"))
        .metrics()
        .topics(["total/*", "battery_1/*"])
        .await
        .unwrap();

    let topics: Vec<&str> = metrics.iter().map(|m| m.topic.as_str()).collect();
    assert_eq!(
        topics,
        ["total/pv_power", "total/load_power", "battery_1/voltage"]
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_sparse_row_falls_back_to_defaults() {
    let server = metrics_server(ok(json!([{"topic": "x/y"}]))).await;

    let metrics = client_for(&server, Auth::password("x"))
        .metrics()
        .await
        .unwrap();

    let mut expected = Metric::default();
    expected.topic = "x/y".to_owned();
    assert_eq!(metrics[0], expected);
}

#[tokio::test]
async fn writing_a_setting_posts_the_topic_and_value() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/metrics"))
        .respond_with(ok(json!({})))
        .mount(&server)
        .await;

    client_for(&server, Auth::password("x"))
        .set_metric("inverter_1/charge_current_limit", "40")
        .await
        .unwrap();

    let request = last_request(&server).await;
    let body: HashMap<String, String> = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["topic"], "inverter_1/charge_current_limit");
    assert_eq!(body["value"], "40");
}

#[tokio::test]
async fn a_written_setting_value_never_reaches_the_log() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok(json!({})))
        .mount(&server)
        .await;
    let client = client_for(&server, Auth::password("x"));

    let logs = support::capture_logs(async {
        client.set_metric("wifi/key", "supersecret").await.unwrap();
    })
    .await;

    assert!(!logs.contains("supersecret"), "{logs}");
    assert!(logs.contains("wifi/key"), "{logs}");
}

#[tokio::test]
async fn a_rejected_write_reports_the_status_and_not_the_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error": "denied for token=topsecret"})),
        )
        .mount(&server)
        .await;

    let error = client_for(&server, Auth::password("x"))
        .set_metric("inverter_1/x", "9999")
        .await
        .unwrap_err();

    assert_eq!(error.status(), Some(400));
    assert!(!error.to_string().contains("topsecret"), "{error}");
}

#[tokio::test]
async fn a_failed_read_reports_the_status_and_endpoint_only() {
    let server = metrics_server(
        ResponseTemplate::new(500)
            .set_body_json(json!({"error": "auth failed for token=topsecret"})),
    )
    .await;

    let error = client_for(&server, Auth::password("x"))
        .metrics()
        .await
        .unwrap_err();

    let message = error.to_string();
    assert_eq!(error.status(), Some(500));
    assert!(message.contains("GET"), "{message}");
    assert!(message.contains("/api/v1/metrics"), "{message}");
    assert!(!message.contains("topsecret"), "{message}");
}

#[tokio::test]
async fn a_body_that_is_not_an_array_of_objects_is_an_error() {
    for body in [
        json!({"error": "boom"}),
        json!("oops"),
        json!(42),
        json!([1, 2, 3]),
    ] {
        let server = system_server(ok(body.clone())).await;

        let error = client_for(&server, Auth::password("x"))
            .system_metrics()
            .await
            .unwrap_err();

        assert!(error.to_string().contains("JSON array"), "{body}: {error}");
        // Not 200: a caller branching on the status would read that as success.
        assert_eq!(error.status(), None, "{body}");
    }
}

#[tokio::test]
async fn a_non_json_body_is_catchable() {
    let server =
        system_server(ResponseTemplate::new(200).set_body_string("<html>captive portal</html>"))
            .await;

    let error = client_for(&server, Auth::password("x"))
        .system_metrics()
        .await
        .unwrap_err();

    assert!(error.to_string().contains("invalid JSON"), "{error}");
    assert!(!error.to_string().contains("None"), "{error}");
}

#[tokio::test]
async fn system_metrics_hit_the_system_endpoint() {
    let server = system_server(ok(json!([
        system_row("system/site_id", json!(12345)),
        json!({
            "topic": "system/free_storage",
            "device": "system",
            "group": "Status",
            "name": "Free storage",
            "value": 8192,
            "unit": "MB",
        }),
    ])))
    .await;

    let metrics = client_for(&server, Auth::password("x"))
        .system_metrics()
        .await
        .unwrap();

    assert_eq!(last_request(&server).await.url.path(), "/api/v1/system");
    let topics: Vec<&str> = metrics.iter().map(|m| m.topic.as_str()).collect();
    assert_eq!(topics, ["system/site_id", "system/free_storage"]);
    assert_eq!(metrics[0].as_i64(), Some(12345));
    assert_eq!(metrics[1].unit, "MB");
}

#[tokio::test]
async fn an_old_build_answers_404_like_any_other_failure() {
    let server = system_server(ResponseTemplate::new(404)).await;

    let error = client_for(&server, Auth::password("x"))
        .system_metrics()
        .await
        .unwrap_err();

    assert_eq!(error.status(), Some(404));
}

#[tokio::test]
async fn system_metrics_tell_a_null_row_from_an_absent_one() {
    // The full-fidelity path lets a caller tell "present but unregistered"
    // (retry later) from "the firmware has no such row" - a distinction
    // `site_id()` deliberately collapses into `None`.
    let (_server, client) =
        system_metrics(json!([system_row("system/site_id", Value::Null)])).await;

    let metrics = client.system_metrics().await.unwrap();

    assert!(
        metrics
            .iter()
            .find(|m| m.topic == "system/site_id")
            .unwrap()
            .is_null()
    );
    assert!(!metrics.iter().any(|m| m.topic == "system/cpu_temperature"));
}

#[tokio::test]
async fn site_id_reads_the_registration() {
    let (_server, client) =
        system_metrics(json!([system_row("system/site_id", json!(987_654))])).await;
    assert_eq!(client.site_id().await.unwrap(), Some(987_654));
}

#[tokio::test]
async fn site_id_is_none_when_unregistered_or_absent() {
    let (_unregistered_server, unregistered) =
        system_metrics(json!([system_row("system/site_id", Value::Null)])).await;
    assert_eq!(unregistered.site_id().await.unwrap(), None);

    let (_absent_server, absent) =
        system_metrics(json!([system_row("system/free_storage", json!(8192))])).await;
    assert_eq!(absent.site_id().await.unwrap(), None);
}

#[tokio::test]
async fn software_version_is_trimmed_and_blank_reads_as_unset() {
    let (_set_server, set) = system_metrics(json!([system_row(
        "system/software_version",
        json!("2026-06-15")
    )]))
    .await;
    assert_eq!(
        set.software_version().await.unwrap().as_deref(),
        Some("2026-06-15")
    );

    for blank in [Value::Null, json!(""), json!("   ")] {
        let (_server, client) = system_metrics(json!([system_row(
            "system/software_version",
            blank.clone()
        )]))
        .await;
        assert_eq!(client.software_version().await.unwrap(), None, "{blank}");
    }
}

#[tokio::test]
async fn cpu_temperature_is_not_truncated_from_a_float() {
    let (_integral_server, integral) =
        system_metrics(json!([system_row("system/cpu_temperature", json!(47))])).await;
    assert_eq!(integral.cpu_temperature().await.unwrap(), Some(47));

    let (_fractional_server, fractional) =
        system_metrics(json!([system_row("system/cpu_temperature", json!(47.8))])).await;
    assert_eq!(fractional.cpu_temperature().await.unwrap(), None);
}

#[tokio::test]
async fn free_storage_reads_megabytes() {
    let (_server, client) =
        system_metrics(json!([system_row("system/free_storage", json!(8192))])).await;
    assert_eq!(client.free_storage().await.unwrap(), Some(8192));
}

#[tokio::test]
async fn one_fetch_serves_every_system_value() {
    let server = system_server(ok(json!([
        system_row("system/site_id", json!(12345)),
        system_row("system/software_version", json!("2026-06-15")),
        system_row("system/free_storage", json!(8192)),
    ])))
    .await;

    let metrics = client_for(&server, Auth::password("x"))
        .system_metrics()
        .await
        .unwrap();

    assert_eq!(metrics.len(), 3);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn one_client_serves_repeated_calls() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ok(json!([row("total/pv_power")])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ok(json!({})))
        .mount(&server)
        .await;
    let client = client_for(&server, Auth::password("x"));

    client.metrics().await.unwrap();
    client.set_metric("a/b", "1").await.unwrap();

    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
