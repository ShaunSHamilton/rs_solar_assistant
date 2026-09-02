//! Rust client for [SolarAssistant](https://solar-assistant.io): the cloud API,
//! a unit's REST API, and its real-time WebSocket.
//!
//! Also available in [Python](https://github.com/Solar-Assistant/py_solar_assistant)
//! and [Go](https://github.com/Solar-Assistant/go_solar_assistant).
//!
//! # Three surfaces
//!
//! | Module | Talks to | Use it for |
//! | ------ | -------- | ---------- |
//! | `cloud` | `solar-assistant.io` | Listing sites, minting a short-lived site token |
//! | `device` | A unit, directly or through the cloud proxy | Reading and writing metrics on demand |
//! | `socket` | A unit's Phoenix Channels WebSocket | Streaming metrics as they change |
//!
//! # Feature flags
//!
//! | Feature | Default | Brings in |
//! | ------- | ------- | --------- |
//! | `cloud` | yes | the cloud REST client, on `reqwest` |
//! | `device` | yes | the device REST client, on `reqwest` |
//! | `websocket` | yes | the WebSocket client, on `tokio-tungstenite` |
//! | `rustls-tls` | yes | TLS through `rustls` |
//! | `native-tls` | no | TLS through the platform's TLS stack |
//!
//! Pick exactly one TLS backend. A REST-only consumer can drop the WebSocket
//! dependency tree with `default-features = false, features = ["device", "rustls-tls"]`.
//!
//! # Logging
//!
//! Every request, frame, and reply is logged through [`tracing`] at `DEBUG`.
//! Credentials are masked first: URLs lose their `user:pass@` userinfo, and
//! `token`, `site_key`, `api_key`, and `password` values are replaced with
//! `[REDACTED]`.

#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod auth;
mod error;
mod metric;
mod redact;
mod serde_util;

pub use crate::{
    auth::Auth,
    error::{Error, Result},
    metric::Metric,
};
