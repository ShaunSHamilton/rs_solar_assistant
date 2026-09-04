//! Credential hygiene for anything that reaches a log line or an error message.
//!
//! Two rules, both inherited from the Python client and both security relevant:
//! URLs lose their `user:pass@` userinfo, and JSON bodies lose the values of
//! well-known credential keys, however deeply nested.

#[cfg(any(feature = "rest", feature = "websocket"))]
use std::borrow::Cow;

#[cfg(any(feature = "cloud", feature = "websocket"))]
use serde_json::Value;

/// Placeholder substituted for a credential value.
pub(crate) const REDACTED: &str = "[REDACTED]";

/// Keys whose values are masked before a body or parameter map is logged.
#[cfg(any(feature = "cloud", feature = "websocket"))]
pub(crate) const SENSITIVE_KEYS: &[&str] =
    &["token", "site_key", "site-key", "api_key", "password"];

/// Returns `url` with any `user:pass@` userinfo stripped.
///
/// Everything else - port, path, query, IPv6 brackets - is preserved verbatim,
/// so the result stays useful in a diagnostic.
#[cfg(any(feature = "rest", feature = "websocket"))]
pub(crate) fn safe_url(url: &str) -> Cow<'_, str> {
    let Some(scheme_end) = url.find("://") else {
        return Cow::Borrowed(url);
    };
    let authority_start = scheme_end + "://".len();
    let authority_end = url[authority_start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |offset| authority_start + offset);
    let authority = &url[authority_start..authority_end];

    match authority.rfind('@') {
        None => Cow::Borrowed(url),
        Some(at) => Cow::Owned(format!(
            "{}{}{}",
            &url[..authority_start],
            &authority[at + 1..],
            &url[authority_end..],
        )),
    }
}

/// Returns `url` with its userinfo stripped and any credential-bearing query
/// value masked, ready for a log line.
#[cfg(any(feature = "cloud", feature = "websocket"))]
pub(crate) fn safe_query_url(url: &str) -> String {
    let url = safe_url(url);
    let Some((base, query)) = url.split_once('?') else {
        return url.into_owned();
    };

    let masked: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, _)) if is_sensitive(key) => format!("{key}={REDACTED}"),
            _ => pair.to_owned(),
        })
        .collect();
    format!("{base}?{}", masked.join("&"))
}

/// Masks every credential value in `value`, in place and at any depth.
///
/// Objects and arrays are walked recursively, because a credential is as
/// likely to arrive as `{"data":{"token":…}}` or `[{"api_key":…}]` as it is at
/// the top level.
#[cfg(any(feature = "cloud", feature = "websocket"))]
pub(crate) fn redact_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if is_sensitive(key) {
                    *value = Value::from(REDACTED);
                } else {
                    redact_json(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_json),
        _ => {}
    }
}

/// Renders a JSON document for a debug log with credential values masked.
///
/// Anything that is not a JSON object or array is returned as trimmed text:
/// there are no keys to mask, and dropping the body entirely would defeat the
/// log.
#[cfg(feature = "cloud")]
pub(crate) fn safe_json(document: &[u8]) -> String {
    let text = String::from_utf8_lossy(document).trim().to_string();
    match serde_json::from_slice::<Value>(document) {
        Ok(mut json) if json.is_object() || json.is_array() => {
            redact_json(&mut json);
            json.to_string()
        }
        _ => text,
    }
}

/// Whether a key names a credential, compared case-insensitively.
#[cfg(any(feature = "cloud", feature = "websocket"))]
pub(crate) fn is_sensitive(key: &str) -> bool {
    SENSITIVE_KEYS
        .iter()
        .any(|sensitive| sensitive.eq_ignore_ascii_case(key))
}

// Every helper below is feature-gated, so with none of them compiled in there
// is nothing left to test and the module itself goes away.
#[cfg(all(test, any(feature = "rest", feature = "websocket")))]
mod tests {
    use super::*;

    #[test]
    fn strips_userinfo() {
        assert_eq!(
            safe_url("http://admin:secret@192.168.1.100/api/v1/system"),
            "http://192.168.1.100/api/v1/system"
        );
    }

    #[test]
    fn keeps_port_path_and_query() {
        assert_eq!(
            safe_url("http://admin:secret@192.168.1.100:8080/api/v1/metrics?topic=x"),
            "http://192.168.1.100:8080/api/v1/metrics?topic=x"
        );
    }

    #[test]
    fn leaves_url_without_userinfo_untouched() {
        let url = "http://192.168.1.100/api/v1/system";
        assert!(matches!(safe_url(url), Cow::Borrowed(_)));
        assert_eq!(safe_url(url), url);
    }

    #[test]
    fn keeps_ipv6_brackets() {
        assert_eq!(
            safe_url("http://admin:secret@[2001:db8::1]:8080/api/v1/system"),
            "http://[2001:db8::1]:8080/api/v1/system"
        );
        assert_eq!(
            safe_url("http://[2001:db8::1]:8080/api/v1/system"),
            "http://[2001:db8::1]:8080/api/v1/system"
        );
    }

    #[test]
    fn strips_only_the_last_at_in_the_authority() {
        assert_eq!(
            safe_url("http://user:p@ss@host/path?a=b@c"),
            "http://host/path?a=b@c"
        );
    }

    #[cfg(any(feature = "cloud", feature = "websocket"))]
    #[test]
    fn masks_credentials_in_a_query() {
        assert_eq!(
            safe_query_url("ws://admin:pw@unit/api/websocket?vsn=2.0.0&token=supersecret"),
            "ws://unit/api/websocket?vsn=2.0.0&token=[REDACTED]"
        );
        assert_eq!(
            safe_query_url("http://unit/api/v1/sites?q=home"),
            "http://unit/api/v1/sites?q=home"
        );
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn masks_credential_fields_in_a_json_body() {
        let body = br#"{"token": "supersecret", "site_key": "topsecret", "host": "h"}"#;
        let redacted = safe_json(body);
        assert!(redacted.contains(REDACTED), "{redacted}");
        assert!(!redacted.contains("supersecret"), "{redacted}");
        assert!(!redacted.contains("topsecret"), "{redacted}");
        assert!(redacted.contains(r#""host":"h""#), "{redacted}");
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn masks_credentials_nested_in_an_object() {
        let redacted = safe_json(br#"{"data": {"inner": {"token": "supersecret"}}}"#);
        assert!(!redacted.contains("supersecret"), "{redacted}");
        assert!(redacted.contains(REDACTED), "{redacted}");
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn masks_credentials_inside_a_top_level_array() {
        let redacted = safe_json(br#"[{"api_key": "supersecret"}, {"host": "h"}]"#);
        assert!(!redacted.contains("supersecret"), "{redacted}");
        assert!(redacted.contains(REDACTED), "{redacted}");
        assert!(redacted.contains(r#""host":"h""#), "{redacted}");
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn masks_a_credential_whose_value_is_not_a_string() {
        let redacted = safe_json(br#"{"password": ["a", "b"], "list": [1, 2]}"#);
        assert!(!redacted.contains(r#"["a","b"]"#), "{redacted}");
        assert!(redacted.contains(r#""list":[1,2]"#), "{redacted}");
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn passes_non_json_bodies_through() {
        assert_eq!(safe_json(b"  <html>nope</html>  "), "<html>nope</html>");
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn passes_scalar_json_bodies_through() {
        assert_eq!(safe_json(b"  42  "), "42");
    }
}
