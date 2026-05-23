use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Local;
use serde_json::{Map, Value, json};

pub fn info(event: &str, fields: &[(&str, Value)]) {
    write(Level::Info, event, fields);
}

pub fn warn(event: &str, fields: &[(&str, Value)]) {
    write(Level::Warn, event, fields);
}

pub fn error(event: &str, fields: &[(&str, Value)]) {
    write(Level::Error, event, fields);
}

pub fn normalize_level(raw: &str) -> Option<&'static str> {
    Level::parse(raw).map(Level::as_str)
}

fn write(level: Level, event: &str, fields: &[(&str, Value)]) {
    if !level.enabled(current_level()) {
        return;
    }

    let payload = log_payload(level, event, fields, unix_timestamp_ms());
    eprintln!("{}", format_log_line(&local_timestamp(), &payload));
}

fn log_payload(level: Level, event: &str, fields: &[(&str, Value)], ts_unix_ms: u128) -> Value {
    let mut object = Map::new();
    object.insert("ts_unix_ms".to_string(), json!(ts_unix_ms));
    object.insert("level".to_string(), json!(level.as_str()));
    object.insert("target".to_string(), json!("dotr"));
    object.insert("event".to_string(), json!(event));

    for (key, value) in fields {
        object.insert((*key).to_string(), value.clone());
    }

    Value::Object(object)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Level {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    fn enabled(self, current: Self) -> bool {
        self <= current
    }
}

fn current_level() -> Level {
    std::env::var("DOTR_LOG_LEVEL")
        .ok()
        .and_then(|raw| Level::parse(&raw))
        .unwrap_or(Level::Info)
}

fn unix_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn local_timestamp() -> String {
    Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string()
}

fn format_log_line(timestamp: &str, payload: &Value) -> String {
    format!("{timestamp}\t{payload}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_is_non_zero() {
        assert!(unix_timestamp_ms() > 0);
    }

    #[test]
    fn local_timestamp_is_first_log_column() {
        let line = format_log_line("2026-05-23T15:30:01.123+08:00", &json!({"event": "test"}));

        assert_eq!(line, "2026-05-23T15:30:01.123+08:00\t{\"event\":\"test\"}");
    }

    #[test]
    fn payload_fields_keep_semantic_order() {
        let payload = log_payload(
            Level::Info,
            "backup_completed",
            &[
                ("added", json!(0)),
                ("updated", json!(1)),
                ("deleted", json!(0)),
                ("unchanged", json!(2)),
                ("skipped", json!(3)),
                ("visited", json!(4)),
                ("cost", json!("43 ms")),
            ],
            123,
        );

        assert_eq!(
            payload.to_string(),
            "{\"ts_unix_ms\":123,\"level\":\"info\",\"target\":\"dotr\",\"event\":\"backup_completed\",\"added\":0,\"updated\":1,\"deleted\":0,\"unchanged\":2,\"skipped\":3,\"visited\":4,\"cost\":\"43 ms\"}"
        );
    }

    #[test]
    fn normalizes_log_levels() {
        assert_eq!(normalize_level("warning"), Some("warn"));
        assert_eq!(normalize_level("DEBUG"), Some("debug"));
        assert_eq!(normalize_level("nope"), None);
    }
}
