const DEVICE_REST_USERNAME: &'static str = "admin";

pub struct DeviceMetric {
    pub topic: String,
    pub name: String,
    pub unit: String,
    pub value: serde_json::Value,
    pub group: String,
}
