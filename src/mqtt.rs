//! MQTT publisher for per-screen sensor values, with Home Assistant
//! discovery emitted on startup. The eventloop runs in a background tokio
//! task so request handlers never block on broker availability — state
//! publishes use `try_publish` and silently drop if the send buffer is full.

use std::time::Duration;

use rumqttc::{AsyncClient, MqttOptions, QoS};

use crate::config::{MqttConfig, ScreenConfig};

#[derive(Clone)]
pub struct Publisher {
    client: AsyncClient,
    state_prefix: String,
}

#[derive(Copy, Clone)]
struct Sensor {
    /// Topic suffix and discovery `object_id`.
    key: &'static str,
    /// Human-readable name shown in Home Assistant.
    name: &'static str,
    /// HA `device_class`. For `enum` sensors this is the literal `"enum"`.
    device_class: &'static str,
    /// Numeric sensors set this to `Some(unit)`; HA discovery then also gets
    /// `state_class: "measurement"`.
    unit: Option<&'static str>,
    /// Enum sensors (`device_class = "enum"`) list their permitted values
    /// here so HA can validate states and show a chooser.
    options: Option<&'static [&'static str]>,
}

const BATTERY_PCT: Sensor = Sensor {
    key: "battery_pct",
    name: "Battery",
    device_class: "battery",
    unit: Some("%"),
    options: None,
};
const BATTERY_MV: Sensor = Sensor {
    key: "battery_mv",
    name: "Battery voltage",
    device_class: "voltage",
    unit: Some("mV"),
    options: None,
};
const TEMPERATURE: Sensor = Sensor {
    key: "temperature",
    name: "Temperature",
    device_class: "temperature",
    unit: Some("°C"),
    options: None,
};
const HUMIDITY: Sensor = Sensor {
    key: "humidity",
    name: "Humidity",
    device_class: "humidity",
    unit: Some("%"),
    options: None,
};
const POWER: Sensor = Sensor {
    key: "power",
    name: "Power",
    device_class: "enum",
    unit: None,
    options: Some(&["battery", "charging", "full", "fault"]),
};

fn enabled_sensors(cfg: &ScreenConfig) -> Vec<Sensor> {
    let mut s = Vec::with_capacity(5);
    if cfg.publish_battery {
        s.push(BATTERY_PCT);
        s.push(BATTERY_MV);
    }
    if cfg.publish_temperature {
        s.push(TEMPERATURE);
    }
    if cfg.publish_humidity {
        s.push(HUMIDITY);
    }
    if cfg.publish_power {
        s.push(POWER);
    }
    s
}

/// HA's discovery `node_id` and entity `unique_id` only allow `[a-z0-9_]`,
/// so any other character in a screen name (e.g. the hyphen in
/// `living-room`) is mapped to `_`. Topics tolerate hyphens, so the
/// state-topic uses the original screen name verbatim.
fn slug(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect()
}

impl Publisher {
    /// Connects to the broker, spawns the eventloop in the background, and
    /// publishes a Home Assistant discovery config for every enabled sensor
    /// on every screen. Discovery messages queue up if the broker isn't
    /// reachable yet — they'll be sent once the eventloop connects.
    pub fn connect(cfg: &MqttConfig, screens: &[ScreenConfig]) -> Self {
        let mut opts = MqttOptions::new(&cfg.client_id, &cfg.broker, cfg.port);
        if let (Some(u), Some(p)) = (&cfg.username, &cfg.password) {
            opts.set_credentials(u, p);
        }
        opts.set_keep_alive(Duration::from_secs(60));

        let (client, mut eventloop) = AsyncClient::new(opts, 256);
        tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "mqtt eventloop error, sleeping before retry");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });

        let publisher = Self {
            client,
            state_prefix: cfg.state_prefix.clone(),
        };
        for screen in screens {
            for sensor in enabled_sensors(screen) {
                publisher.publish_discovery(cfg, &screen.name, sensor);
            }
        }
        publisher
    }

    fn publish_discovery(&self, cfg: &MqttConfig, screen: &str, sensor: Sensor) {
        let slug = slug(screen);
        let topic = format!(
            "{}/sensor/epd_photoframe_{}/{}/config",
            cfg.discovery_prefix, slug, sensor.key
        );
        let mut payload = serde_json::json!({
            "name": sensor.name,
            "unique_id": format!("epd_photoframe_{}_{}", slug, sensor.key),
            "state_topic": format!("{}/{}/{}", self.state_prefix, screen, sensor.key),
            "device_class": sensor.device_class,
            "device": {
                "identifiers": [format!("epd_photoframe_{}", slug)],
                "name": screen,
                "manufacturer": "epd-photoframe-server",
                "model": "ePaper photo frame",
            },
        });
        if let Some(unit) = sensor.unit {
            payload["unit_of_measurement"] = unit.into();
            payload["state_class"] = "measurement".into();
        }
        if let Some(options) = sensor.options {
            payload["options"] = serde_json::json!(options);
        }
        if let Err(e) =
            self.client.try_publish(&topic, QoS::AtLeastOnce, true, payload.to_string())
        {
            tracing::warn!(topic = %topic, error = %e, "mqtt discovery publish failed");
        }
    }

    /// Publishes a single state value. Fire-and-forget; logs at warn if the
    /// outbound queue is full.
    pub fn publish(&self, screen: &str, key: &str, value: impl ToString) {
        let topic = format!("{}/{}/{}", self.state_prefix, screen, key);
        if let Err(e) =
            self.client.try_publish(&topic, QoS::AtMostOnce, true, value.to_string())
        {
            tracing::warn!(topic = %topic, error = %e, "mqtt state publish failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_lowercases_and_replaces_punctuation() {
        assert_eq!(slug("living-room"), "living_room");
        assert_eq!(slug("E1002-Landscape"), "e1002_landscape");
        assert_eq!(slug("foo.bar"), "foo_bar");
        assert_eq!(slug("plain"), "plain");
    }

    fn screen_with(battery: bool, temp: bool, humidity: bool, power: bool) -> ScreenConfig {
        toml::from_str(&format!(
            r#"
            name = "x"
            width = 800
            height = 480
            share_url = "https://example.com"
            publish_battery = {battery}
            publish_temperature = {temp}
            publish_humidity = {humidity}
            publish_power = {power}
            "#
        ))
        .unwrap()
    }

    #[test]
    fn enabled_sensors_battery_includes_both_mv_and_pct() {
        let s = enabled_sensors(&screen_with(true, false, false, false));
        let keys: Vec<_> = s.iter().map(|s| s.key).collect();
        assert_eq!(keys, vec!["battery_pct", "battery_mv"]);
    }

    #[test]
    fn enabled_sensors_all_off_produces_nothing() {
        let s = enabled_sensors(&screen_with(false, false, false, false));
        assert!(s.is_empty());
    }

    #[test]
    fn enabled_sensors_all_on_produces_five() {
        let s = enabled_sensors(&screen_with(true, true, true, true));
        let keys: Vec<_> = s.iter().map(|s| s.key).collect();
        assert_eq!(
            keys,
            vec!["battery_pct", "battery_mv", "temperature", "humidity", "power"]
        );
    }

    #[test]
    fn power_sensor_is_an_enum_with_four_options() {
        assert_eq!(POWER.device_class, "enum");
        assert!(POWER.unit.is_none());
        assert_eq!(POWER.options, Some(&["battery", "charging", "full", "fault"][..]));
    }
}
