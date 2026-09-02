//! Real-time metrics over a SolarAssistant unit's WebSocket.
//!
//! The unit speaks [Phoenix Channels](https://hexdocs.pm/phoenix/js/) v2.0.0:
//! every frame is a JSON array `[join_ref, ref, topic, event, payload]`. This
//! module handles the framing, the reference counter, the 30-second heartbeat,
//! and the `definition` -> `data` merge, and hands you metrics as a stream.
//!
//! ```no_run
//! use futures_util::StreamExt;
//! use rs_solar_assistant::{Auth, Socket, socket::Options};
//!
//! # async fn example() -> rs_solar_assistant::Result<()> {
//! let mut socket = Socket::connect(Options::local(
//!     "192.168.1.100",
//!     Auth::password("<web-password>"),
//! ))
//! .await?;
//! socket.subscribe_metrics([]).await?;
//!
//! let mut metrics = socket.metrics();
//! while let Some(metric) = metrics.next().await {
//!     let metric = metric?;
//!     println!("{} = {} {}", metric.name, metric.value, metric.unit);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Streams instead of callbacks
//!
//! The Python and Go clients register handlers and block in `listen()`. Here
//! you own the loop: [`Socket::metrics`] yields merged metrics,
//! [`Socket::events`] yields everything the channel carries, and
//! [`Socket::next_event`] is the single-step form both are built on. That
//! composes with `select!`, cancellation, and backpressure, and it means a
//! write such as [`Socket::set_setting`] can share the connection with a
//! reader without dropping the frames that arrive in between.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    time::Duration,
};

use futures_util::{
    SinkExt, Stream, StreamExt,
    stream::{SplitSink, SplitStream},
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Map, Value, json};
use tokio::{
    net::TcpStream,
    time::{Instant, Interval, MissedTickBehavior, interval_at, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Error as WsError, Message as WsMessage,
        client::IntoClientRequest,
        http::{HeaderValue, Request},
    },
};

use crate::{
    Auth, Metric,
    error::{Error, Result},
    redact::{safe_query_url, safe_url},
};

/// How often a heartbeat frame is sent to keep the channel alive.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Time allowed to reach a unit on the local network before falling back.
///
/// Deliberately short: a local address that is not answering is usually a unit
/// that is elsewhere, and the cloud proxy is the real destination.
pub const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// Time allowed to reach the cloud proxy.
pub const CLOUD_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Time allowed for the unit to answer a [`Socket::set_setting`].
pub const SET_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Channel the unit publishes metrics on.
pub const METRICS_CHANNEL: &str = "metrics";

const WEBSOCKET_PATH: &str = "/api/websocket";
const PROTOCOL_VERSION: &str = "2.0.0";
const HEARTBEAT_CHANNEL: &str = "phoenix";

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Where to dial a unit, and with what credential.
///
/// Two shapes, matching the two ways a unit is reachable:
///
/// - [`Options::local`] - straight to the unit on your own network.
/// - [`Options::cloud`] - through the cloud proxy, with a token from the
///   cloud client's `authorize_site`.
///
/// Combine them with [`Options::host`] to try the local network first and fall
/// back to the proxy; an `AuthorizeResponse` converts into exactly that
/// arrangement. There is no way to build options with nowhere to dial.
#[derive(Clone, Debug)]
pub struct Options {
    target: Target,
    auth: Auth,
    heartbeat: Duration,
}

/// Address, or addresses, to try.
#[derive(Clone, Debug)]
enum Target {
    Local(String),
    Cloud(String),
    LocalThenCloud { local_ip: String, host: String },
}

impl Options {
    /// Dials `local_ip` directly. No cloud account involved.
    pub fn local(local_ip: impl Into<String>, auth: Auth) -> Self {
        Self {
            target: Target::Local(local_ip.into()),
            auth,
            heartbeat: HEARTBEAT_INTERVAL,
        }
    }

    /// Dials the cloud proxy at `host`.
    pub fn cloud(host: impl Into<String>, auth: Auth) -> Self {
        Self {
            target: Target::Cloud(host.into()),
            auth,
            heartbeat: HEARTBEAT_INTERVAL,
        }
    }

    /// Adds a cloud proxy to fall back to when the local address does not
    /// answer within [`LOCAL_CONNECT_TIMEOUT`].
    #[must_use]
    pub fn host(mut self, host: impl Into<String>) -> Self {
        let host = host.into();
        self.target = match self.target {
            Target::Local(local_ip) | Target::LocalThenCloud { local_ip, .. } => {
                Target::LocalThenCloud { local_ip, host }
            }
            Target::Cloud(_) => Target::Cloud(host),
        };
        self
    }

    /// How often to send a heartbeat frame. Defaults to
    /// [`HEARTBEAT_INTERVAL`], which is what the unit expects; shorten it when
    /// something in between drops idle connections sooner.
    #[must_use]
    pub fn heartbeat_interval(mut self, every: Duration) -> Self {
        self.heartbeat = every;
        self
    }

    /// Adds a local address to try before the cloud proxy.
    #[must_use]
    pub fn local_ip(mut self, local_ip: impl Into<String>) -> Self {
        let local_ip = local_ip.into();
        self.target = match self.target {
            Target::Cloud(host) | Target::LocalThenCloud { host, .. } => {
                Target::LocalThenCloud { local_ip, host }
            }
            Target::Local(_) => Target::Local(local_ip),
        };
        self
    }
}

#[cfg(feature = "cloud")]
#[cfg_attr(docsrs, doc(cfg(feature = "cloud")))]
impl From<&crate::AuthorizeResponse> for Options {
    /// Local-first, cloud-fallback, using the site's short-lived token.
    fn from(authorization: &crate::AuthorizeResponse) -> Self {
        let options = Self::cloud(&authorization.host, Auth::from(authorization));
        if authorization.local_ip.is_empty() {
            options
        } else {
            options.local_ip(&authorization.local_ip)
        }
    }
}

/// A topic the server should push, optionally throttled.
///
/// Without any filter the server picks a curated default set: `total/*`, plus
/// selected `battery_*` and `inverter_*` topics. Only metrics in the `Info`,
/// `Status`, and `Settings` groups are ever sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicFilter {
    topic: String,
    max_frequency: Option<Duration>,
}

impl TopicFilter {
    /// Subscribes to one topic or glob, e.g. `total/*`.
    pub fn new(topic: impl Into<String>) -> Self {
        Self {
            topic: topic.into(),
            max_frequency: None,
        }
    }

    /// Asks the server to push this topic at most once per `every`.
    ///
    /// Rounded down to whole seconds, which is the resolution the server
    /// accepts.
    #[must_use]
    pub fn max_frequency(mut self, every: Duration) -> Self {
        self.max_frequency = Some(every);
        self
    }

    fn to_payload(&self) -> Value {
        let mut payload = Map::new();
        payload.insert("topic".to_owned(), Value::from(self.topic.clone()));
        // Omitted when unthrottled: the server reads a zero as a real limit.
        if let Some(max_frequency) = self.max_frequency.filter(|every| every.as_secs() > 0) {
            payload.insert(
                "max_frequency_s".to_owned(),
                Value::from(max_frequency.as_secs()),
            );
        }
        Value::Object(payload)
    }
}

impl From<&str> for TopicFilter {
    fn from(topic: &str) -> Self {
        Self::new(topic)
    }
}

impl From<String> for TopicFilter {
    fn from(topic: String) -> Self {
        Self::new(topic)
    }
}

/// A raw Phoenix Channel frame: `[join_ref, ref, topic, event, payload]`.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Message {
    /// Reference of the join that opened the channel this frame belongs to.
    pub join_ref: String,
    /// Reference of this frame, echoed back in its reply.
    pub msg_ref: String,
    /// Channel topic, e.g. `metrics`.
    pub topic: String,
    /// Event name, e.g. `data`, `phx_reply`, `set_result`.
    pub event: String,
    /// Event payload.
    pub payload: Map<String, Value>,
}

/// Something the unit pushed.
///
/// [`Socket::metrics`] filters this down to [`Event::Metrics`]; use
/// [`Socket::events`] when the other variants matter.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// Metric values, with their definitions already merged in.
    Metrics(Vec<Metric>),
    /// Metric definitions, sent once on join. Kept internally for the merge
    /// and surfaced here so discovery can run before the first value arrives.
    Definitions(Vec<Metric>),
    /// A snapshot of the unit's own metrics: site ID, software version, CPU
    /// temperature, free storage. Self-contained, so nothing is merged.
    SystemMetrics(Vec<Metric>),
    /// Any other frame, undecoded.
    Message(Message),
}

/// A connected SolarAssistant WebSocket.
///
/// Owns the connection: reading, writing, and the heartbeat all happen on the
/// task that drives it, so there is no background task to leak and no shared
/// mutable socket to guard.
///
/// The heartbeat therefore rides along with whatever is reading - it goes out
/// while you await [`next_event`](Self::next_event), either stream, or
/// [`set_setting`](Self::set_setting). A socket nobody polls sends nothing, so
/// keep one polled (in a `tokio::spawn`, or a `select!` arm) if you need it to
/// stay open while idle.
pub struct Socket {
    sink: SplitSink<Ws, WsMessage>,
    stream: SplitStream<Ws>,
    heartbeat: Interval,
    /// Definitions by topic, merged into every `data` row.
    definitions: HashMap<String, Metric>,
    /// Events decoded while waiting for a specific reply, kept so a write does
    /// not swallow the metrics that arrive alongside it.
    pending: VecDeque<Event>,
    /// Join reference of the metrics channel, once joined.
    metrics_join_ref: Option<String>,
    next_ref: u64,
    connected_host: String,
}

impl Socket {
    /// Dials a unit and returns a ready socket.
    ///
    /// With both a local address and a host set, the local address is tried
    /// first with a [short timeout](LOCAL_CONNECT_TIMEOUT) and the cloud proxy
    /// picks up the failure. A cloud token works for a local connection too, so
    /// the fallback needs no second credential.
    pub async fn connect(options: Options) -> Result<Self> {
        let Options {
            target,
            auth,
            heartbeat,
        } = options;
        match target {
            Target::Local(local_ip) => {
                Self::dial(&local_ip, &auth, false, LOCAL_CONNECT_TIMEOUT, heartbeat)
                    .await
                    .map_err(|error| Error::Connect {
                        status: error.status(),
                        message: format!("could not connect to {local_ip}: {error}"),
                    })
            }
            Target::Cloud(host) => Self::dial_cloud(&host, &auth, heartbeat).await,
            Target::LocalThenCloud { local_ip, host } => {
                match Self::dial(&local_ip, &auth, false, LOCAL_CONNECT_TIMEOUT, heartbeat).await {
                    Ok(socket) => Ok(socket),
                    Err(error) => {
                        tracing::debug!(
                            target: "rs_solar_assistant::socket",
                            "local connection to {local_ip} failed ({error}), trying the cloud",
                        );
                        Self::dial_cloud(&host, &auth, heartbeat).await
                    }
                }
            }
        }
    }

    /// Dials the cloud proxy, which only ever accepts a token.
    async fn dial_cloud(host: &str, auth: &Auth, heartbeat: Duration) -> Result<Self> {
        if !auth.is_usable_via_cloud() {
            return Err(Error::Connect {
                status: None,
                message: "the cloud proxy needs a token, not a web password".to_owned(),
            });
        }
        Self::dial(host, auth, true, CLOUD_CONNECT_TIMEOUT, heartbeat).await
    }

    /// Host this socket is connected to, local address or cloud proxy.
    #[must_use]
    pub fn connected_host(&self) -> &str {
        &self.connected_host
    }

    /// Joins the metrics channel, asking for `filters`.
    ///
    /// Pass an empty list for the server's curated default set:
    ///
    /// ```no_run
    /// # use std::time::Duration;
    /// # use rs_solar_assistant::{Socket, TopicFilter};
    /// # async fn example(socket: &mut Socket) -> rs_solar_assistant::Result<()> {
    /// socket.subscribe_metrics([]).await?;
    ///
    /// socket
    ///     .subscribe_metrics([
    ///         "total/*".into(),
    ///         TopicFilter::new("battery_*/voltage").max_frequency(Duration::from_secs(10)),
    ///     ])
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn subscribe_metrics<I>(&mut self, filters: I) -> Result<()>
    where
        I: IntoIterator<Item = TopicFilter>,
    {
        let topics: Vec<Value> = filters.into_iter().map(|f| f.to_payload()).collect();
        let payload = if topics.is_empty() {
            json!({})
        } else {
            json!({ "topics": topics })
        };

        let join_ref = self.join_with_payload(METRICS_CHANNEL, payload).await?;
        self.metrics_join_ref = Some(join_ref);
        Ok(())
    }

    /// Sends a `phx_join` for a channel, returning its join reference.
    pub async fn join(&mut self, topic: &str) -> Result<String> {
        self.join_with_payload(topic, json!({})).await
    }

    /// Sends a `phx_join` carrying a custom payload.
    pub async fn join_with_payload(&mut self, topic: &str, payload: Value) -> Result<String> {
        let join_ref = self.take_ref();
        let msg_ref = self.take_ref();
        self.send(&join_ref, &msg_ref, topic, "phx_join", &payload)
            .await?;
        Ok(join_ref)
    }

    /// Waits for the next event.
    ///
    /// `None` means the connection closed. An error is not necessarily fatal -
    /// a [`Error::Channel`] describes one channel - but the connection is
    /// usually gone with it.
    pub async fn next_event(&mut self) -> Option<Result<Event>> {
        if let Some(event) = self.pending.pop_front() {
            return Some(Ok(event));
        }
        match self.recv().await? {
            Ok(message) => Some(Ok(self.classify(message))),
            Err(error) => Some(Err(error)),
        }
    }

    /// Everything the unit pushes, as a stream.
    ///
    /// The stream is pinned on the heap once, so it can be polled with
    /// [`futures_util::StreamExt::next`] without the caller
    /// pinning it.
    pub fn events(&mut self) -> impl Stream<Item = Result<Event>> + Unpin + '_ {
        Box::pin(futures_util::stream::unfold(self, |socket| async move {
            socket.next_event().await.map(|event| (event, socket))
        }))
    }

    /// Metric values as they arrive, with definitions merged in.
    ///
    /// Join the channel with [`subscribe_metrics`](Self::subscribe_metrics)
    /// first, or nothing will be pushed. Definitions, system snapshots, and
    /// channel bookkeeping are consumed silently; use
    /// [`events`](Self::events) to see them.
    pub fn metrics(&mut self) -> impl Stream<Item = Result<Metric>> + Unpin + '_ {
        Box::pin(futures_util::stream::unfold(
            (self, VecDeque::new()),
            |(socket, mut ready): (&mut Self, VecDeque<Metric>)| async move {
                loop {
                    if let Some(metric) = ready.pop_front() {
                        return Some((Ok(metric), (socket, ready)));
                    }
                    match socket.next_event().await? {
                        Ok(Event::Metrics(metrics)) => ready.extend(metrics),
                        Ok(_) => {}
                        Err(error) => return Some((Err(error), (socket, ready))),
                    }
                }
            },
        ))
    }

    /// Writes a setting and waits for the unit to confirm it.
    ///
    /// Joins the metrics channel first if
    /// [`subscribe_metrics`](Self::subscribe_metrics) has not already done so;
    /// an existing subscription is reused, so its topic filters survive. Frames
    /// that arrive while waiting are buffered, not dropped, and reach the next
    /// [`next_event`](Self::next_event).
    ///
    /// A rejection is an [`Error::SettingRejected`]; silence for
    /// [`SET_REPLY_TIMEOUT`] is an [`Error::Channel`] rather than a hang.
    pub async fn set_setting(&mut self, topic: &str, value: &str) -> Result<()> {
        let join_ref = if let Some(join_ref) = &self.metrics_join_ref {
            join_ref.clone()
        } else {
            self.subscribe_metrics([]).await?;
            self.await_join().await?;
            self.metrics_join_ref.clone().unwrap_or_default()
        };

        let msg_ref = self.take_ref();
        self.send(
            &join_ref,
            &msg_ref,
            METRICS_CHANNEL,
            "set",
            &json!({ "topic": topic, "value": value }),
        )
        .await?;

        self.await_set_result(topic).await
    }

    /// Closes the connection.
    pub async fn close(mut self) -> Result<()> {
        match self.sink.close().await {
            Ok(()) | Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Waits for the metrics channel's join to be acknowledged.
    ///
    /// A failed join arrives as an [`Error::Channel`] from [`Self::recv`], so
    /// only the success case has to be recognised here.
    async fn await_join(&mut self) -> Result<()> {
        loop {
            match self.recv().await {
                None => {
                    return Err(Error::Channel {
                        topic: METRICS_CHANNEL.to_owned(),
                        message: "the connection closed before the join was acknowledged"
                            .to_owned(),
                    });
                }
                Some(Err(error)) => return Err(error),
                Some(Ok(message)) => {
                    if message.topic == METRICS_CHANNEL && message.event == "phx_reply" {
                        return Ok(());
                    }
                    let event = self.classify(message);
                    self.pending.push_back(event);
                }
            }
        }
    }

    /// Waits for the `set_result` naming `topic`, buffering everything else.
    async fn await_set_result(&mut self, topic: &str) -> Result<()> {
        let deadline = Instant::now() + SET_REPLY_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Ok(next) = timeout(remaining, self.recv()).await else {
                return Err(Error::Channel {
                    topic: METRICS_CHANNEL.to_owned(),
                    message: format!("timed out waiting for the reply to `{topic}`"),
                });
            };

            match next {
                None => {
                    return Err(Error::Channel {
                        topic: METRICS_CHANNEL.to_owned(),
                        message: format!("the connection closed before `{topic}` was confirmed"),
                    });
                }
                Some(Err(error)) => return Err(error),
                Some(Ok(message)) => {
                    if message.event == "set_result"
                        && message.payload.get("topic").and_then(Value::as_str) == Some(topic)
                    {
                        return set_outcome(topic, &message.payload);
                    }
                    let event = self.classify(message);
                    self.pending.push_back(event);
                }
            }
        }
    }

    /// Reads the next frame, keeping the heartbeat going while it waits.
    ///
    /// Returns `None` once the connection is closed. Malformed frames are
    /// dropped rather than surfaced: the server versions its payloads, and a
    /// frame this build cannot parse is not the caller's problem.
    async fn recv(&mut self) -> Option<Result<Message>> {
        loop {
            // Both branches borrow one field each, and neither body touches
            // `self`, so the heartbeat write below cannot overlap the read.
            let step = tokio::select! {
                _ = self.heartbeat.tick() => Step::Beat,
                frame = self.stream.next() => match frame {
                    None
                    | Some(
                        Ok(WsMessage::Close(_))
                        | Err(WsError::ConnectionClosed | WsError::AlreadyClosed),
                    ) => Step::Closed,
                    Some(Ok(WsMessage::Text(text))) => Step::Text(text.to_string()),
                    Some(Ok(_)) => Step::Ignored,
                    Some(Err(error)) => Step::Failed(error),
                },
            };

            match step {
                Step::Beat => {
                    if let Err(error) = self.send_heartbeat().await {
                        return Some(Err(error));
                    }
                }
                Step::Text(text) => {
                    tracing::debug!(target: "rs_solar_assistant::socket", "< {text}");
                    if let Some(message) = decode(&text) {
                        return Some(match channel_error(&message) {
                            Some(error) => Err(error),
                            None => Ok(message),
                        });
                    }
                }
                Step::Ignored => {}
                Step::Closed => return None,
                Step::Failed(error) => return Some(Err(error.into())),
            }
        }
    }

    /// Turns a frame into an event, folding definitions into the merge table.
    fn classify(&mut self, message: Message) -> Event {
        match message.event.as_str() {
            "definition" => {
                let definitions = parse_metrics(&message.payload, "definitions");
                for definition in &definitions {
                    self.definitions
                        .insert(definition.topic.clone(), definition.clone());
                }
                Event::Definitions(definitions)
            }
            "data" => Event::Metrics(self.merge(&message.payload)),
            "system" => Event::SystemMetrics(parse_metrics(&message.payload, "metrics")),
            _ => Event::Message(message),
        }
    }

    /// Rebuilds full metrics from `data` rows, which carry only topic and value.
    fn merge(&self, payload: &Map<String, Value>) -> Vec<Metric> {
        parse_metrics(payload, "metrics")
            .into_iter()
            .map(|row| match self.definitions.get(&row.topic) {
                Some(definition) => Metric {
                    value: row.value,
                    ..definition.clone()
                },
                None => row,
            })
            .collect()
    }

    async fn send_heartbeat(&mut self) -> Result<()> {
        let msg_ref = self.take_ref();
        self.send("", &msg_ref, HEARTBEAT_CHANNEL, "heartbeat", &json!({}))
            .await
    }

    async fn send(
        &mut self,
        join_ref: &str,
        msg_ref: &str,
        topic: &str,
        event: &str,
        payload: &Value,
    ) -> Result<()> {
        let frame = encode(join_ref, msg_ref, topic, event, payload);
        tracing::debug!(target: "rs_solar_assistant::socket", "> {frame}");
        self.sink.send(WsMessage::text(frame)).await?;
        Ok(())
    }

    /// Next Phoenix reference. They only have to be unique per connection.
    fn take_ref(&mut self) -> String {
        self.next_ref += 1;
        self.next_ref.to_string()
    }

    /// Opens one connection, mapping every failure to a message a user can act
    /// on.
    async fn dial(
        host: &str,
        auth: &Auth,
        via_cloud: bool,
        budget: Duration,
        heartbeat_interval: Duration,
    ) -> Result<Self> {
        let request = build_request(host, auth, via_cloud)?;
        tracing::debug!(
            target: "rs_solar_assistant::socket",
            "> WS {} headers={:?}",
            safe_query_url(&request.uri().to_string()),
            request.headers().keys().collect::<Vec<_>>(),
        );

        let Ok(connected) = timeout(budget, connect_async(request)).await else {
            return Err(Error::Connect {
                status: None,
                message: format!(
                    "connection to {} timed out - is it reachable?",
                    safe_url(host)
                ),
            });
        };
        let (ws, _response) = connected.map_err(|error| connect_error(&error))?;

        let (sink, stream) = ws.split();
        let mut heartbeat = interval_at(Instant::now() + heartbeat_interval, heartbeat_interval);
        // A late heartbeat is worth nothing, and a burst of them even less.
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);

        Ok(Self {
            sink,
            stream,
            heartbeat,
            definitions: HashMap::new(),
            pending: VecDeque::new(),
            metrics_join_ref: None,
            next_ref: 0,
            connected_host: host.to_owned(),
        })
    }
}

/// Builds the upgrade request: `wss` and routing headers for the proxy, plain
/// `ws` and the bare credential for a unit on the local network.
fn build_request(host: &str, auth: &Auth, via_cloud: bool) -> Result<Request<()>> {
    let scheme = if via_cloud { "wss" } else { "ws" };
    let (credential, secret) = match auth {
        Auth::Password(password) => ("password", password),
        Auth::Token { token, .. } => ("token", token),
    };

    let url = format!(
        "{scheme}://{host}{WEBSOCKET_PATH}?vsn={PROTOCOL_VERSION}&{credential}={}",
        utf8_percent_encode(secret, NON_ALPHANUMERIC)
    );
    let mut request = url
        .into_client_request()
        .map_err(|error| connect_error(&error))?;

    // Routing headers only mean something to the proxy; a unit on the local
    // network is reached directly and is never sent them.
    if via_cloud
        && let Auth::Token {
            site_id, site_key, ..
        } = auth
    {
        let headers = request.headers_mut();
        if let Some(site_id) = site_id {
            headers.insert("site-id", HeaderValue::from(*site_id));
        }
        if let Some(site_key) = site_key {
            let value = HeaderValue::from_str(site_key).map_err(|_| Error::Connect {
                status: None,
                message: "the site key is not a valid HTTP header value".to_owned(),
            })?;
            headers.insert("site-key", value);
        }
    }
    Ok(request)
}

impl fmt::Debug for Socket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Socket")
            .field("connected_host", &self.connected_host)
            .field("definitions", &self.definitions.len())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

/// What one turn of the read loop produced.
enum Step {
    Beat,
    Text(String),
    Ignored,
    Closed,
    Failed(WsError),
}

/// Reads a `set_result` payload.
fn set_outcome(topic: &str, payload: &Map<String, Value>) -> Result<()> {
    if payload.get("result").and_then(Value::as_str) == Some("ok") {
        return Ok(());
    }
    Err(Error::SettingRejected {
        topic: topic.to_owned(),
        message: payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_owned(),
    })
}

/// Recognises the two ways the server reports a channel failure.
fn channel_error(message: &Message) -> Option<Error> {
    if message.event == "phx_error" {
        return Some(Error::Channel {
            topic: message.topic.clone(),
            message: "the channel crashed (phx_error)".to_owned(),
        });
    }
    if message.event == "phx_reply"
        && message.payload.get("status").and_then(Value::as_str) == Some("error")
    {
        let reason = message
            .payload
            .get("response")
            .map_or_else(|| "no reason given".to_owned(), ToString::to_string);
        return Some(Error::Channel {
            topic: message.topic.clone(),
            message: format!("join failed: {reason}"),
        });
    }
    None
}

/// Maps a handshake failure to something a user can act on, keeping the HTTP
/// status for callers that would rather match structurally.
fn connect_error(error: &WsError) -> Error {
    let WsError::Http(response) = error else {
        return Error::Connect {
            status: None,
            message: format!("connection failed: {error}"),
        };
    };

    let status = response.status().as_u16();
    let message = match status {
        401 | 403 => format!("authentication failed (HTTP {status}) - check your credentials"),
        404 => "the WebSocket endpoint is missing (HTTP 404) - the unit may be running a build older than 2026-03-24".to_owned(),
        502..=504 => format!("the site is offline or unreachable (HTTP {status})"),
        _ => format!("connection rejected (HTTP {status})"),
    };
    Error::Connect {
        status: Some(status),
        message,
    }
}

/// Serialises a Phoenix frame.
fn encode(join_ref: &str, msg_ref: &str, topic: &str, event: &str, payload: &Value) -> String {
    json!([join_ref, msg_ref, topic, event, payload]).to_string()
}

/// Parses a Phoenix frame, or `None` if it is not one.
fn decode(raw: &str) -> Option<Message> {
    let Ok(Value::Array(frame)) = serde_json::from_str::<Value>(raw) else {
        return None;
    };
    if frame.len() < 5 {
        return None;
    }

    Some(Message {
        join_ref: as_string(&frame[0]),
        msg_ref: as_string(&frame[1]),
        topic: as_string(&frame[2]),
        event: as_string(&frame[3]),
        payload: match &frame[4] {
            Value::Object(payload) => payload.clone(),
            _ => Map::new(),
        },
    })
}

/// Phoenix sends `null` refs on server pushes; they read as empty here.
fn as_string(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// Reads the metric rows out of a payload, skipping any that are not objects.
fn parse_metrics(payload: &Map<String, Value>, key: &str) -> Vec<Metric> {
    let Some(Value::Array(rows)) = payload.get(key) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| serde_json::from_value(row.clone()).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(event: &str, payload: &Value) -> Message {
        Message {
            event: event.to_owned(),
            topic: METRICS_CHANNEL.to_owned(),
            payload: payload.as_object().cloned().unwrap_or_default(),
            ..Message::default()
        }
    }

    #[test]
    fn encodes_a_frame_as_a_phoenix_array() {
        assert_eq!(
            encode("1", "2", "metrics", "phx_join", &json!({})),
            r#"["1","2","metrics","phx_join",{}]"#
        );
    }

    #[test]
    fn decodes_a_frame_with_a_null_ref() {
        let message = decode(r#"["1",null,"metrics","data",{"metrics":[]}]"#).unwrap();
        assert_eq!(message.join_ref, "1");
        assert_eq!(message.msg_ref, "");
        assert_eq!(message.topic, "metrics");
        assert_eq!(message.event, "data");
    }

    #[test]
    fn drops_frames_that_are_not_phoenix_frames() {
        assert!(decode("not json").is_none());
        assert!(decode(r#"{"not": "an array"}"#).is_none());
        assert!(decode(r#"["1","2","short"]"#).is_none());
    }

    #[test]
    fn a_non_object_payload_reads_as_empty() {
        let message = decode(r#"["1","2","metrics","data","surprise"]"#).unwrap();
        assert!(message.payload.is_empty());
    }

    #[test]
    fn a_throttle_is_only_sent_when_it_is_set() {
        assert_eq!(
            TopicFilter::new("total/*").to_payload(),
            json!({"topic": "total/*"})
        );
        assert_eq!(
            TopicFilter::new("battery_*/voltage")
                .max_frequency(Duration::from_secs(10))
                .to_payload(),
            json!({"topic": "battery_*/voltage", "max_frequency_s": 10})
        );
        assert_eq!(
            TopicFilter::new("total/*")
                .max_frequency(Duration::ZERO)
                .to_payload(),
            json!({"topic": "total/*"})
        );
    }

    #[test]
    fn a_crashed_channel_and_a_failed_join_both_raise() {
        let crashed = channel_error(&message("phx_error", &json!({}))).unwrap();
        assert!(crashed.to_string().contains("phx_error"), "{crashed}");

        let refused = channel_error(&message(
            "phx_reply",
            &json!({"status": "error", "response": {"reason": "unauthorized"}}),
        ))
        .unwrap();
        assert!(refused.to_string().contains("unauthorized"), "{refused}");

        assert!(channel_error(&message("phx_reply", &json!({"status": "ok"}))).is_none());
        assert!(channel_error(&message("data", &json!({}))).is_none());
    }

    #[test]
    fn a_rejected_setting_carries_the_reason() {
        let payload =
            json!({"topic": "inverter_1/x", "result": "error", "message": "value rejected"});
        let error = set_outcome("inverter_1/x", payload.as_object().unwrap()).unwrap_err();
        assert!(error.to_string().contains("value rejected"), "{error}");

        let payload = json!({"topic": "inverter_1/x", "result": "error"});
        let error = set_outcome("inverter_1/x", payload.as_object().unwrap()).unwrap_err();
        assert!(error.to_string().contains("unknown error"), "{error}");

        let payload = json!({"topic": "inverter_1/x", "result": "ok"});
        assert!(set_outcome("inverter_1/x", payload.as_object().unwrap()).is_ok());
    }

    #[test]
    fn a_local_dial_sends_the_credential_and_no_routing_headers() {
        let request = build_request("192.168.1.100", &Auth::password("we b/pw"), false).unwrap();

        assert_eq!(
            request.uri().to_string(),
            "ws://192.168.1.100/api/websocket?vsn=2.0.0&password=we%20b%2Fpw"
        );
        assert!(!request.headers().contains_key("site-id"));
        assert!(!request.headers().contains_key("site-key"));
    }

    #[test]
    fn a_cloud_dial_sends_the_token_and_the_routing_headers() {
        // The header path only runs over `wss`, which needs a TLS server, so
        // it is checked on the request rather than over a connection.
        let request =
            build_request("proxy.example", &Auth::proxy("jwt", 42, "skey"), true).unwrap();

        assert_eq!(
            request.uri().to_string(),
            "wss://proxy.example/api/websocket?vsn=2.0.0&token=jwt"
        );
        assert_eq!(request.headers()["site-id"], "42");
        assert_eq!(request.headers()["site-key"], "skey");
    }

    #[test]
    fn a_token_without_routing_sends_no_site_headers() {
        let request = build_request("proxy.example", &Auth::token("jwt"), true).unwrap();
        assert!(!request.headers().contains_key("site-id"));
        assert!(!request.headers().contains_key("site-key"));
    }

    #[test]
    fn a_logged_upgrade_url_hides_the_credential() {
        let request = build_request("192.168.1.100", &Auth::token("supersecret"), false).unwrap();
        let logged = safe_query_url(&request.uri().to_string());
        assert!(!logged.contains("supersecret"), "{logged}");
        assert!(logged.contains(crate::redact::REDACTED), "{logged}");
        assert!(logged.contains("vsn=2.0.0"), "{logged}");
    }

    #[test]
    fn adding_a_host_or_a_local_address_keeps_both() {
        let both = Options::local("192.168.1.100", Auth::token("jwt")).host("proxy.example");
        assert!(matches!(both.target, Target::LocalThenCloud { .. }));

        let both = Options::cloud("proxy.example", Auth::token("jwt")).local_ip("192.168.1.100");
        assert!(matches!(both.target, Target::LocalThenCloud { .. }));

        let local = Options::local("192.168.1.100", Auth::token("jwt")).local_ip("10.0.0.1");
        assert!(matches!(local.target, Target::Local(ref ip) if ip == "10.0.0.1"));
    }

    #[test]
    fn handshake_failures_keep_their_status() {
        for (status, expected) in [
            (401, "authentication failed"),
            (403, "authentication failed"),
            (404, "older than"),
            (502, "offline or unreachable"),
            (500, "connection rejected"),
        ] {
            let response = tokio_tungstenite::tungstenite::http::Response::builder()
                .status(status)
                .body(None)
                .unwrap();
            let error = connect_error(&WsError::Http(Box::new(response)));
            assert_eq!(error.status(), Some(status));
            assert!(error.to_string().contains(expected), "{status}: {error}");
        }
    }
}
