//! Deserialization helpers shared by the response models.

use serde::{Deserialize, Deserializer};

/// Deserializes `null` as `T::default()`.
///
/// The SolarAssistant API sends `null` where it means "unset" for fields that
/// are otherwise strings, numbers, or objects. Combined with a container-level
/// `#[serde(default)]` this makes a field tolerant of both an absent key and an
/// explicit null, matching the Python client's `.get(key, default) or default`.
pub(crate) fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}
