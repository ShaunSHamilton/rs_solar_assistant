//! A scripted Phoenix Channels server, standing in for a SolarAssistant unit.
//!
//! Mirrors what a unit does on `GET /api/websocket`: reply to `phx_join`, push
//! whatever the script says, answer `set` with a `set_result`, and answer
//! `heartbeat`. Every frame it receives is recorded so a test can assert on
//! what the client sent.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message as WsMessage,
        handshake::server::{ErrorResponse, Request, Response},
        http::Response as HttpResponse,
    },
};

/// What the server should do once a client connects.
#[derive(Clone, Debug)]
pub struct Script {
    /// `status` of the `phx_reply` sent for a `phx_join`.
    pub join_status: String,
    /// `response` of that reply.
    pub join_response: Value,
    /// Pushed as a `definition` event after the join, when set.
    pub definitions: Option<Value>,
    /// Pushed as a `data` event after the join, when set.
    pub data: Option<Value>,
    /// Pushed verbatim after the join.
    pub push_frames: Vec<Value>,
    /// `result` of the `set_result` sent for a `set`.
    pub set_result: String,
    /// `message` of that reply, when set.
    pub set_message: Option<String>,
    /// Reject the upgrade with this HTTP status instead of accepting it.
    pub upgrade_status: Option<u16>,
    /// Close the connection once the join has been answered.
    pub close_after_join: bool,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            join_status: "ok".to_owned(),
            join_response: json!({}),
            definitions: None,
            data: None,
            push_frames: Vec::new(),
            set_result: "ok".to_owned(),
            set_message: None,
            upgrade_status: None,
            close_after_join: false,
        }
    }
}

/// The upgrade request the client sent.
#[derive(Clone, Debug, Default)]
pub struct Handshake {
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
}

/// A running scripted server.
///
/// Shuts down when dropped, so tests keep it alive for as long as they need
/// the connection.
#[derive(Debug)]
pub struct Server {
    host: String,
    state: Arc<State>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Default)]
struct State {
    handshake: Mutex<Handshake>,
    received: Mutex<Vec<Value>>,
}

impl Server {
    /// Starts a server on an ephemeral loopback port.
    pub async fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let state = Arc::new(State::default());

        let task = tokio::spawn({
            let state = Arc::clone(&state);
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    serve(stream, script.clone(), Arc::clone(&state)).await;
                }
            }
        });

        Self { host, state, task }
    }

    /// `host:port` to pass as a local address.
    pub fn host(&self) -> String {
        self.host.clone()
    }

    /// The upgrade request the client sent.
    pub fn handshake(&self) -> Handshake {
        self.state.handshake.lock().unwrap().clone()
    }

    /// Every frame the client sent, in order.
    pub fn received(&self) -> Vec<Value> {
        self.state.received.lock().unwrap().clone()
    }

    /// The `event` field of every frame the client sent.
    pub fn received_events(&self) -> Vec<String> {
        self.received()
            .iter()
            .map(|frame| frame[3].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the shape tungstenite's callback requires"
)]
async fn serve(stream: TcpStream, script: Script, state: Arc<State>) {
    let upgrade_status = script.upgrade_status;
    let accepted = accept_hdr_async(stream, |request: &Request, response: Response| {
        *state.handshake.lock().unwrap() = record(request);
        match upgrade_status {
            None => Ok(response),
            Some(status) => Err(ErrorResponse::from(
                HttpResponse::builder()
                    .status(status)
                    .body(None)
                    .expect("a rejection response"),
            )),
        }
    })
    .await;

    let Ok(mut ws) = accepted else { return };

    while let Some(Ok(message)) = ws.next().await {
        let WsMessage::Text(text) = message else {
            continue;
        };
        let frame: Value = serde_json::from_str(&text).expect("a JSON frame");
        state.received.lock().unwrap().push(frame.clone());

        let join_ref = frame[0].clone();
        let msg_ref = frame[1].clone();
        let topic = frame[2].clone();
        let payload = frame[4].clone();

        match frame[3].as_str().unwrap_or_default() {
            "phx_join" => {
                send(
                    &mut ws,
                    json!([
                        join_ref,
                        msg_ref,
                        topic,
                        "phx_reply",
                        {"status": script.join_status, "response": script.join_response},
                    ]),
                )
                .await;

                if let Some(definitions) = &script.definitions {
                    send(
                        &mut ws,
                        json!([join_ref, null, topic, "definition", {"definitions": definitions}]),
                    )
                    .await;
                }
                if let Some(data) = &script.data {
                    send(
                        &mut ws,
                        json!([join_ref, null, topic, "data", {"metrics": data}]),
                    )
                    .await;
                }
                for pushed in &script.push_frames {
                    send(&mut ws, pushed.clone()).await;
                }
                if script.close_after_join {
                    let _ = ws.close(None).await;
                }
            }
            "set" => {
                let mut reply = json!({"topic": payload["topic"], "result": script.set_result});
                if let Some(message) = &script.set_message {
                    reply["message"] = json!(message);
                }
                send(&mut ws, json!([join_ref, null, topic, "set_result", reply])).await;
            }
            "heartbeat" => {
                send(
                    &mut ws,
                    json!([null, msg_ref, "phoenix", "phx_reply", {"status": "ok", "response": {}}]),
                )
                .await;
            }
            _ => {}
        }
    }
}

async fn send(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>, frame: Value) {
    let _ = ws.send(WsMessage::text(frame.to_string())).await;
}

fn record(request: &Request) -> Handshake {
    let query = request
        .uri()
        .query()
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| {
            let decoded = percent_encoding::percent_decode_str(value)
                .decode_utf8_lossy()
                .into_owned();
            (key.to_owned(), decoded)
        })
        .collect();
    let headers = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();

    Handshake {
        path: request.uri().path().to_owned(),
        query,
        headers,
    }
}
