//! Credentials for a SolarAssistant unit.

use std::fmt;

use crate::redact::REDACTED;

/// How to authenticate against a unit, over REST or the WebSocket.
///
/// A unit accepts exactly one of two credentials, so they are variants rather
/// than a bag of optional fields: an invalid combination cannot be built.
///
/// - [`Auth::password`] - the unit's web password, for a direct local
///   connection on your own network.
/// - [`Auth::token`] - a short-lived JWT from the cloud API, for a local
///   connection without knowing the web password.
/// - [`Auth::proxy`] - the same JWT plus the site routing headers, required
///   when reaching a unit through the cloud proxy.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Auth {
    /// HTTP Basic `admin:<web password>`, or `?password=` on the WebSocket.
    Password(String),
    /// Bearer JWT, with the routing headers a cloud proxy needs.
    Token {
        /// Short-lived JWT.
        token: String,
        /// Site the proxy should route to. Unused on a local connection.
        site_id: Option<u64>,
        /// Key the proxy authenticates the site with. Unused locally.
        site_key: Option<String>,
    },
}

impl Auth {
    /// Local web password of the unit (`admin:<password>`).
    pub fn password(password: impl Into<String>) -> Self {
        Self::Password(password.into())
    }

    /// Bearer JWT without cloud-proxy routing, for reaching a unit directly.
    pub fn token(token: impl Into<String>) -> Self {
        Self::Token {
            token: token.into(),
            site_id: None,
            site_key: None,
        }
    }

    /// Bearer JWT with the `site-id` / `site-key` routing a cloud proxy needs.
    pub fn proxy(token: impl Into<String>, site_id: u64, site_key: impl Into<String>) -> Self {
        Self::Token {
            token: token.into(),
            site_id: Some(site_id),
            site_key: Some(site_key.into()),
        }
    }

    /// Whether this credential can reach a unit through the cloud proxy.
    ///
    /// A web password is only ever accepted on the local network.
    #[must_use]
    pub fn is_usable_via_cloud(&self) -> bool {
        matches!(self, Self::Token { .. })
    }
}

/// Prints the shape of the credential, never its secret.
impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password(_) => f.debug_tuple("Password").field(&REDACTED).finish(),
            Self::Token {
                site_id, site_key, ..
            } => f
                .debug_struct("Token")
                .field("token", &REDACTED)
                .field("site_id", site_id)
                .field("site_key", &site_key.as_ref().map(|_| REDACTED))
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_a_credential() {
        let password = format!("{:?}", Auth::password("hunter2"));
        assert!(!password.contains("hunter2"), "{password}");

        let proxy = format!("{:?}", Auth::proxy("jwt-value", 7, "site-key-value"));
        assert!(!proxy.contains("jwt-value"), "{proxy}");
        assert!(!proxy.contains("site-key-value"), "{proxy}");
        assert!(proxy.contains('7'), "{proxy}");
    }

    #[test]
    fn only_tokens_work_through_the_cloud() {
        assert!(!Auth::password("pw").is_usable_via_cloud());
        assert!(Auth::token("jwt").is_usable_via_cloud());
        assert!(Auth::proxy("jwt", 1, "k").is_usable_via_cloud());
    }
}
