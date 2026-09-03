//! The metric row shared by the REST and WebSocket surfaces.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::serde_util::null_default;

/// One metric reported by a SolarAssistant unit.
///
/// The same shape arrives from three places, so one type covers them all:
/// `GET /api/v1/metrics`, `GET /api/v1/system`, and the WebSocket `data` and
/// `system` events.
///
/// # Discovery fields
///
/// `platform`, `device_class`, `state_class`, `unit_of_measurement`, `min`,
/// `max`, `options`, `payload_on`, and `payload_off` describe the metric for
/// Home Assistant discovery. Over REST they are populated when the request asks
/// for them, which is the default (`metrics().discovery(false)` opts out).
/// Over the WebSocket they come from the `definition` event and are merged into
/// every metric before it reaches you. Units running a build older than
/// 2026-05-07 leave them `None`.
///
/// # Values
///
/// `value` stays untyped: the server sends strings, numbers, and booleans
/// depending on the topic, and the topic set is server-driven. Use
/// [`Metric::as_f64`] and friends to read it.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Metric {
    /// MQTT-style topic, e.g. `total/pv_power`.
    #[serde(deserialize_with = "null_default")]
    pub topic: String,
    /// Human-readable name, e.g. `PV power`.
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    /// Unit the value is expressed in, e.g. `W`. Empty when unitless.
    #[serde(deserialize_with = "null_default")]
    pub unit: String,
    /// Current value: a string, number, or boolean depending on the topic.
    pub value: Value,
    /// Group the metric belongs to: `Info`, `Status`, or `Settings`.
    #[serde(deserialize_with = "null_default")]
    pub group: String,
    /// Device the metric belongs to, e.g. `inverter`, `battery`, `total`.
    #[serde(deserialize_with = "null_default")]
    pub device: String,
    /// Index of the device when there is more than one of its kind.
    pub number: Option<i64>,
    /// Home Assistant platform, e.g. `sensor`, `number`, `select`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Home Assistant device class, e.g. `power`, `voltage`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_class: Option<String>,
    /// Home Assistant state class, e.g. `measurement`, `total_increasing`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_class: Option<String>,
    /// Home Assistant unit of measurement, e.g. `W`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_of_measurement: Option<String>,
    /// Lowest value a writable metric accepts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Highest value a writable metric accepts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Values a `select`-style metric accepts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// Payload that means "on" for a switch-style metric.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_on: Option<String>,
    /// Payload that means "off" for a switch-style metric.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_off: Option<String>,
}

impl Metric {
    /// Value as a float, or `None` if it is not a number.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        self.value.as_f64()
    }

    /// Value as an integer, or `None` if it is not one.
    ///
    /// A float is not truncated: `47.8` reads as `None`, not `47`.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        self.value.as_i64()
    }

    /// Value as a string, or `None` if it is not one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        self.value.as_str()
    }

    /// Value as a bool, or `None` if it is not one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        self.value.as_bool()
    }

    /// Whether the unit reported the metric but has no value for it yet.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.value.is_null()
    }

    /// Device label, with the index appended when the unit has several of a
    /// kind: `battery #2`, or plain `total`.
    #[must_use]
    pub fn device_label(&self) -> String {
        match self.number {
            Some(number) => format!("{} #{number}", self.device),
            None => self.device.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_fields_fall_back_to_defaults() {
        let metric: Metric = serde_json::from_str(r#"{"topic": "x/y"}"#).unwrap();
        assert_eq!(metric.topic, "x/y");
        assert_eq!(metric.name, "");
        assert_eq!(metric.unit, "");
        assert!(metric.is_null());
        assert_eq!(metric.number, None);
        assert_eq!(metric.platform, None);
    }

    #[test]
    fn null_strings_become_empty_not_an_error() {
        let metric: Metric =
            serde_json::from_str(r#"{"topic": null, "name": null, "unit": null}"#).unwrap();
        assert_eq!(metric.topic, "");
        assert_eq!(metric.name, "");
        assert_eq!(metric.unit, "");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let metric: Metric =
            serde_json::from_str(r#"{"topic": "x/y", "brand_new_field": 1}"#).unwrap();
        assert_eq!(metric.topic, "x/y");
    }

    #[test]
    fn typed_accessors_do_not_coerce() {
        let metric: Metric = serde_json::from_str(r#"{"value": 47.8}"#).unwrap();
        assert_eq!(metric.as_f64(), Some(47.8));
        assert_eq!(metric.as_i64(), None);
        assert_eq!(metric.as_str(), None);
    }

    #[test]
    fn device_label_appends_the_index_when_there_is_one() {
        let metric: Metric = serde_json::from_str(r#"{"device": "battery", "number": 2}"#).unwrap();
        assert_eq!(metric.device_label(), "battery #2");

        let metric: Metric = serde_json::from_str(r#"{"device": "total"}"#).unwrap();
        assert_eq!(metric.device_label(), "total");
    }
}
