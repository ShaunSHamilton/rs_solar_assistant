//! The crate's error type.

use crate::redact::safe_url;

/// Convenience alias for results returned by this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Anything that can go wrong talking to SolarAssistant.
///
/// [`Error::status`] recovers the HTTP status when the server answered with
/// one, so callers can branch structurally (`404` for an outdated device
/// build, `401`/`403` for bad credentials) instead of matching on message text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The server answered with a non-success status.
    ///
    /// The response body is deliberately dropped: it can carry credentials in
    /// free text, which key-based redaction cannot scrub.
    #[error("API error {status}: {method} {url}")]
    Api {
        /// HTTP status code of the response.
        status: u16,
        /// HTTP method of the request that failed.
        method: &'static str,
        /// Requested URL, with any userinfo stripped.
        url: String,
    },

    /// The server answered `200` with a body this client cannot use.
    #[error("invalid response: {0}")]
    InvalidResponse(String),

    /// The request never produced a response (DNS, connect, timeout, TLS).
    #[cfg(feature = "rest")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "cloud", feature = "device"))))]
    #[error("request failed: {0}")]
    Transport(#[from] reqwest::Error),

    /// The WebSocket could not be established.
    #[cfg(feature = "websocket")]
    #[cfg_attr(docsrs, doc(cfg(feature = "websocket")))]
    #[error("{message}")]
    Connect {
        /// HTTP status of a rejected upgrade, when the server sent one.
        status: Option<u16>,
        /// Human-readable explanation, suitable for surfacing to a user.
        message: String,
    },

    /// The server reported a channel failure mid-stream.
    #[cfg(feature = "websocket")]
    #[cfg_attr(docsrs, doc(cfg(feature = "websocket")))]
    #[error("channel `{topic}`: {message}")]
    Channel {
        /// Phoenix channel topic that failed.
        topic: String,
        /// Reason reported by the server.
        message: String,
    },

    /// The device refused a setting written over the WebSocket.
    #[cfg(feature = "websocket")]
    #[cfg_attr(docsrs, doc(cfg(feature = "websocket")))]
    #[error("setting `{topic}` rejected: {message}")]
    SettingRejected {
        /// Metric topic that was written.
        topic: String,
        /// Reason reported by the device.
        message: String,
    },

    /// The WebSocket transport failed after a successful handshake.
    #[cfg(feature = "websocket")]
    #[cfg_attr(docsrs, doc(cfg(feature = "websocket")))]
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
}

impl Error {
    /// HTTP status behind this error, when there is one.
    ///
    /// `None` means the failure has no HTTP status of its own - a malformed
    /// body, a transport failure, or a channel error - and never that the
    /// request succeeded.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } => Some(*status),
            #[cfg(feature = "websocket")]
            Self::Connect { status, .. } => *status,
            _ => None,
        }
    }

    /// Builds an [`Error::Api`], stripping userinfo from `url`.
    pub(crate) fn api(method: &'static str, url: &str, status: u16) -> Self {
        Self::Api {
            status,
            method,
            url: safe_url(url).into_owned(),
        }
    }

    /// Builds an [`Error::InvalidResponse`], stripping userinfo from `url`.
    pub(crate) fn invalid_response(what: &str, url: &str) -> Self {
        Self::InvalidResponse(format!("{what} from {}", safe_url(url)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_reports_its_status_and_hides_userinfo() {
        let err = Error::api("GET", "http://admin:secret@device/api/v1/metrics", 401);
        assert_eq!(err.status(), Some(401));
        let message = err.to_string();
        assert!(message.contains("GET"), "{message}");
        assert!(message.contains("/api/v1/metrics"), "{message}");
        assert!(!message.contains("secret"), "{message}");
    }

    #[test]
    fn malformed_body_has_no_status_and_no_literal_none() {
        let err = Error::invalid_response("invalid JSON", "http://device/api/v1/system");
        assert_eq!(err.status(), None);
        assert!(!err.to_string().contains("None"), "{err}");
    }
}
