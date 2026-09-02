//! WebSocket client, driven against a scripted Phoenix server over a real
//! `ws://` connection.

#![cfg(feature = "websocket")]

use std::time::Duration;

use futures_util::StreamExt;
use rs_solar_assistant::{
    Auth, Error, Event, Metric, Socket, TopicFilter,
    socket::{METRICS_CHANNEL, Options},
};
use serde_json::{Value, json};

mod support;

use support::phoenix::{Script, Server};

/// Connects to a scripted server with a local web password.
async fn connect(script: Script) -> (Server, Socket) {
    let server = Server::start(script).await;
    let socket = Socket::connect(Options::local(server.host(), Auth::password("secret")))
        .await
        .expect("the scripted server accepts the connection");
    (server, socket)
}

/// Drains the socket until it closes, collecting what it yielded.
async fn drain(socket: &mut Socket) -> Vec<Event> {
    let mut events = Vec::new();
    while let Some(event) = socket.next_event().await {
        events.push(event.expect("no channel error"));
    }
    events
}

/// An address nothing is listening on.
async fn dead_address() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    address
}

fn definition(topic: &str) -> Value {
    json!({
        "topic": topic,
        "device": "total",
        "number": null,
        "group": "Info",
        "name": "PV Power",
        "unit": "W",
        "platform": "sensor",
        "device_class": "power",
        "state_class": "measurement",
        "unit_of_measurement": "W",
        "min": 0,
        "max": 10000,
    })
}

#[tokio::test]
async fn a_local_password_connection_sends_the_protocol_version() {
    let (server, socket) = connect(Script::default()).await;

    assert_eq!(socket.connected_host(), server.host());
    socket.close().await.unwrap();

    let handshake = server.handshake();
    assert_eq!(handshake.path, "/api/websocket");
    assert_eq!(handshake.query["vsn"], "2.0.0");
    assert_eq!(handshake.query["password"], "secret");
}

#[tokio::test]
async fn a_local_token_connection_sends_no_routing_headers() {
    let server = Server::start(Script::default()).await;

    let socket = Socket::connect(Options::local(server.host(), Auth::token("jwt")))
        .await
        .unwrap();
    socket.close().await.unwrap();

    let handshake = server.handshake();
    assert_eq!(handshake.query["token"], "jwt");
    assert!(!handshake.headers.contains_key("site-id"));
    assert!(!handshake.headers.contains_key("site-key"));
}

#[tokio::test]
async fn an_unreachable_unit_without_a_fallback_names_the_address() {
    let dead = dead_address().await;

    let error = Socket::connect(Options::local(&dead, Auth::password("x")))
        .await
        .unwrap_err();

    assert!(error.to_string().contains(&dead), "{error}");
}

#[tokio::test]
async fn a_dead_local_address_falls_through_to_the_cloud() {
    // The cloud leg cannot succeed against a plain server - it dials `wss` -
    // but the error must come from that leg, not from the local address.
    let dead = dead_address().await;
    let server = Server::start(Script::default()).await;

    let error =
        Socket::connect(Options::local(&dead, Auth::proxy("jwt", 1, "k")).host(server.host()))
            .await
            .unwrap_err();

    assert!(!error.to_string().contains(&dead), "{error}");
}

#[tokio::test]
async fn a_web_password_is_refused_for_the_cloud_proxy() {
    let error = Socket::connect(Options::cloud("proxy.example", Auth::password("pw")))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("needs a token"), "{error}");
}

#[tokio::test]
async fn a_rejected_upgrade_keeps_its_status_and_explains_itself() {
    for (status, expected) in [
        (401, "authentication failed"),
        (403, "authentication failed"),
        (404, "older than"),
        (502, "offline or unreachable"),
        (500, "connection rejected"),
    ] {
        let server = Server::start(Script {
            upgrade_status: Some(status),
            ..Script::default()
        })
        .await;

        let error = Socket::connect(Options::local(server.host(), Auth::password("x")))
            .await
            .unwrap_err();

        assert_eq!(error.status(), Some(status), "{status}");
        assert!(error.to_string().contains(expected), "{status}: {error}");
    }
}

#[tokio::test]
async fn definitions_are_merged_into_the_values_that_follow() {
    let (_server, mut socket) = connect(Script {
        definitions: Some(json!([definition("total/pv_power")])),
        data: Some(json!([{"topic": "total/pv_power", "value": 1234}])),
        close_after_join: true,
        ..Script::default()
    })
    .await;

    socket.subscribe_metrics([]).await.unwrap();
    let metrics: Vec<Metric> = socket
        .metrics()
        .map(|metric| metric.expect("no channel error"))
        .collect()
        .await;

    assert_eq!(metrics.len(), 1);
    let metric = &metrics[0];
    assert_eq!(metric.topic, "total/pv_power");
    assert_eq!(metric.as_i64(), Some(1234)); // from the data event
    assert_eq!(metric.name, "PV Power"); // from the definition event
    assert_eq!(metric.unit, "W");
    assert_eq!(metric.device, "total");
    assert_eq!(metric.platform.as_deref(), Some("sensor"));
    assert_eq!(metric.device_class.as_deref(), Some("power"));
    assert_eq!(metric.min, Some(0.0));
    assert_eq!(metric.max, Some(10000.0));
}

#[tokio::test]
async fn a_value_without_a_definition_keeps_empty_metadata() {
    let (_server, mut socket) = connect(Script {
        data: Some(json!([{"topic": "unknown/x", "value": 5}])),
        close_after_join: true,
        ..Script::default()
    })
    .await;

    socket.subscribe_metrics([]).await.unwrap();
    let metrics: Vec<Metric> = socket
        .metrics()
        .map(|metric| metric.expect("no channel error"))
        .collect()
        .await;

    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].topic, "unknown/x");
    assert_eq!(metrics[0].as_i64(), Some(5));
    assert_eq!(metrics[0].name, "");
    assert_eq!(metrics[0].device, "");
    assert_eq!(metrics[0].platform, None);
}

#[tokio::test]
async fn definitions_also_surface_as_an_event() {
    // A Home Assistant bridge builds its entities from these before the first
    // value arrives, so they are not swallowed by the merge.
    let (_server, mut socket) = connect(Script {
        definitions: Some(json!([definition("total/pv_power")])),
        close_after_join: true,
        ..Script::default()
    })
    .await;

    socket.subscribe_metrics([]).await.unwrap();
    let events = drain(&mut socket).await;

    let definitions = events
        .iter()
        .find_map(|event| match event {
            Event::Definitions(definitions) => Some(definitions),
            _ => None,
        })
        .expect("a definitions event");
    assert_eq!(definitions[0].topic, "total/pv_power");
    assert_eq!(definitions[0].name, "PV Power");
}

#[tokio::test]
async fn topic_filters_travel_in_the_join_payload() {
    let (server, mut socket) = connect(Script {
        close_after_join: true,
        ..Script::default()
    })
    .await;

    socket
        .subscribe_metrics([
            "total/*".into(),
            TopicFilter::new("battery_*/voltage").max_frequency(Duration::from_secs(10)),
        ])
        .await
        .unwrap();
    drain(&mut socket).await;

    let join = server
        .received()
        .into_iter()
        .find(|frame| frame[3] == "phx_join")
        .expect("a join frame");
    assert_eq!(
        join[4],
        json!({
            "topics": [
                {"topic": "total/*"},
                {"topic": "battery_*/voltage", "max_frequency_s": 10},
            ]
        })
    );
}

#[tokio::test]
async fn no_filters_join_with_an_empty_payload() {
    let (server, mut socket) = connect(Script {
        close_after_join: true,
        ..Script::default()
    })
    .await;

    socket.subscribe_metrics([]).await.unwrap();
    drain(&mut socket).await;

    let join = server
        .received()
        .into_iter()
        .find(|frame| frame[3] == "phx_join")
        .expect("a join frame");
    assert_eq!(join[4], json!({}));
}

#[tokio::test]
async fn a_system_snapshot_arrives_self_contained() {
    let (_server, mut socket) = connect(Script {
        push_frames: vec![json!([
            "1",
            null,
            METRICS_CHANNEL,
            "system",
            {"metrics": [
                {"topic": "system/site_id", "device": "system", "group": "Info", "name": "Site ID", "value": 3, "unit": ""},
                {"topic": "system/cpu_temperature", "device": "system", "group": "Status", "name": "CPU temperature", "value": 47, "unit": "°C"},
            ]},
        ])],
        close_after_join: true,
        ..Script::default()
    })
    .await;

    socket.join(METRICS_CHANNEL).await.unwrap();
    let events = drain(&mut socket).await;

    let snapshot = events
        .iter()
        .find_map(|event| match event {
            Event::SystemMetrics(metrics) => Some(metrics),
            _ => None,
        })
        .expect("a system event");
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot[0].topic, "system/site_id");
    assert_eq!(snapshot[0].as_i64(), Some(3));
    assert_eq!(snapshot[0].name, "Site ID");
    assert_eq!(snapshot[1].unit, "°C");
    assert_eq!(snapshot[1].group, "Status");
}

#[tokio::test]
async fn unrecognised_frames_come_through_undecoded() {
    let (_server, mut socket) = connect(Script {
        push_frames: vec![json!(["1", null, "weather", "update", {"temp": 21}])],
        close_after_join: true,
        ..Script::default()
    })
    .await;

    socket.join(METRICS_CHANNEL).await.unwrap();
    let events = drain(&mut socket).await;

    let raw: Vec<(&str, &str)> = events
        .iter()
        .filter_map(|event| match event {
            Event::Message(message) => Some((message.topic.as_str(), message.event.as_str())),
            _ => None,
        })
        .collect();
    assert!(raw.contains(&("weather", "update")), "{raw:?}");
}

#[tokio::test]
async fn malformed_frames_are_dropped_without_taking_the_stream_down() {
    let (_server, mut socket) = connect(Script {
        push_frames: vec![
            json!(["1", "2", "short"]),                            // too few elements
            json!({"not": "an array"}),                            // not a frame
            json!(["1", null, "weather", "update", {"temp": 21}]), // valid
        ],
        close_after_join: true,
        ..Script::default()
    })
    .await;

    socket.join(METRICS_CHANNEL).await.unwrap();
    let events = drain(&mut socket).await;

    // The join reply and the one valid frame, nothing else.
    assert_eq!(events.len(), 2, "{events:?}");
}

#[tokio::test]
async fn a_crashed_channel_surfaces_as_an_error() {
    let (_server, mut socket) = connect(Script {
        push_frames: vec![json!(["1", null, METRICS_CHANNEL, "phx_error", {}])],
        ..Script::default()
    })
    .await;

    socket.join(METRICS_CHANNEL).await.unwrap();

    let error = loop {
        match socket.next_event().await.expect("the stream is still open") {
            Ok(_) => {}
            Err(error) => break error,
        }
    };
    assert!(matches!(error, Error::Channel { .. }), "{error}");
}

#[tokio::test]
async fn a_refused_join_surfaces_as_an_error() {
    let (_server, mut socket) = connect(Script {
        join_status: "error".to_owned(),
        join_response: json!({"reason": "unauthorized"}),
        ..Script::default()
    })
    .await;

    socket.subscribe_metrics([]).await.unwrap();

    let error = socket.next_event().await.unwrap().unwrap_err();
    assert!(error.to_string().contains("unauthorized"), "{error}");
}

#[tokio::test]
async fn writing_a_setting_joins_first_then_sets() {
    let (server, mut socket) = connect(Script::default()).await;

    socket
        .set_setting("inverter_1/power_mode", "Off grid")
        .await
        .unwrap();
    socket.close().await.unwrap();

    assert_eq!(server.received_events(), ["phx_join", "set"]);
    assert_eq!(
        server.received()[1][4],
        json!({"topic": "inverter_1/power_mode", "value": "Off grid"})
    );
}

#[tokio::test]
async fn writing_a_setting_reuses_an_existing_subscription() {
    // Re-joining would drop the topic filters the caller subscribed with.
    let (server, mut socket) = connect(Script::default()).await;

    socket.subscribe_metrics(["total/*".into()]).await.unwrap();
    socket.set_setting("inverter_1/x", "1").await.unwrap();
    socket.close().await.unwrap();

    assert_eq!(server.received_events(), ["phx_join", "set"]);
}

#[tokio::test]
async fn a_rejected_setting_reports_the_reason() {
    let (_server, mut socket) = connect(Script {
        set_result: "error".to_owned(),
        set_message: Some("value rejected".to_owned()),
        ..Script::default()
    })
    .await;

    let error = socket.set_setting("inverter_1/x", "bad").await.unwrap_err();

    assert!(matches!(error, Error::SettingRejected { .. }), "{error}");
    assert!(error.to_string().contains("value rejected"), "{error}");
}

#[tokio::test]
async fn metrics_that_arrive_during_a_write_are_not_lost() {
    // The Python client reads and discards frames while waiting for the
    // `set_result`; here they are buffered and handed back afterwards.
    let (_server, mut socket) = connect(Script {
        definitions: Some(json!([definition("total/pv_power")])),
        data: Some(json!([{"topic": "total/pv_power", "value": 1234}])),
        ..Script::default()
    })
    .await;

    socket.set_setting("inverter_1/x", "1").await.unwrap();

    let mut values = Vec::new();
    while let Some(Ok(event)) = socket.next_event().await {
        if let Event::Metrics(metrics) = event {
            values.extend(metrics.into_iter().filter_map(|metric| metric.as_i64()));
            break;
        }
    }
    assert_eq!(values, [1234]);
}

#[tokio::test]
async fn the_credential_is_redacted_in_the_debug_log() {
    let server = Server::start(Script::default()).await;
    let host = server.host();

    let logs = support::capture_logs(async {
        let socket = Socket::connect(Options::local(host, Auth::token("supersecret")))
            .await
            .unwrap();
        socket.close().await.unwrap();
    })
    .await;

    assert!(logs.contains("[REDACTED]"), "{logs}");
    assert!(!logs.contains("supersecret"), "{logs}");
}

#[tokio::test]
async fn the_channel_is_kept_alive_with_heartbeats() {
    let server = Server::start(Script::default()).await;
    let mut socket = Socket::connect(
        Options::local(server.host(), Auth::password("x"))
            .heartbeat_interval(Duration::from_millis(20)),
    )
    .await
    .unwrap();

    // Each heartbeat draws a `phx_reply`, so two events mean two beats.
    for _ in 0..2 {
        socket.next_event().await.unwrap().unwrap();
    }
    socket.close().await.unwrap();

    let heartbeats = server
        .received_events()
        .into_iter()
        .filter(|event| event == "heartbeat")
        .count();
    assert!(heartbeats >= 2, "{:?}", server.received_events());
}
