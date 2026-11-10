//! The part of `docker compose config --format json` devslot reads.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};

/// The merged configuration of a set of compose files.
#[derive(Debug, Default, Deserialize)]
pub struct ComposeConfig {
    /// The services by name.
    #[serde(default, deserialize_with = "null_as_default")]
    pub services: BTreeMap<String, Service>,
    /// The top-level volumes by key.
    #[serde(default, deserialize_with = "null_as_default")]
    pub volumes: BTreeMap<String, Resource>,
    /// The top-level networks by key.
    #[serde(default, deserialize_with = "null_as_default")]
    pub networks: BTreeMap<String, Resource>,
}

/// One service.
#[derive(Debug, Default, Deserialize)]
pub struct Service {
    /// `container_name`, when set.
    #[serde(default)]
    pub container_name: Option<String>,
    /// The published ports.
    #[serde(default, deserialize_with = "null_as_default")]
    pub ports: Vec<PortMapping>,
    /// The mounts.
    #[serde(default, deserialize_with = "null_as_default")]
    pub volumes: Vec<Mount>,
    /// The `env_file` entries.
    #[serde(default, deserialize_with = "null_as_default")]
    pub env_file: Vec<EnvFileEntry>,
    /// The `environment` map; a `None` value passes the caller's variable through.
    #[serde(default, deserialize_with = "null_as_default")]
    pub environment: BTreeMap<String, Option<String>>,
}

impl Service {
    /// Whether `environment` gives `name` a value of its own.
    pub fn pins(&self, name: &str) -> bool {
        matches!(self.environment.get(name), Some(Some(_)))
    }
}

/// A published port.
#[derive(Debug, Deserialize)]
pub struct PortMapping {
    /// The container port, when it is a single number.
    #[serde(default)]
    pub target: Option<u64>,
    /// The host side: a number, a range, or empty for a random port.
    #[serde(default, deserialize_with = "text_or_number")]
    pub published: Option<String>,
}

/// A mount of a service.
#[derive(Debug, Deserialize)]
pub struct Mount {
    /// `bind`, `volume`, `tmpfs`...
    #[serde(rename = "type", default)]
    pub kind: String,
    /// The host path of a bind mount, or the volume key.
    #[serde(default)]
    pub source: Option<String>,
}

/// An `env_file` entry: a bare path, or the object form with `path`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum EnvFileEntry {
    /// `- .env`
    Path(String),
    /// `- path: .env`
    Described {
        /// The absolute path of the file.
        path: String,
    },
}

impl EnvFileEntry {
    /// The path of the file.
    pub fn path(&self) -> &str {
        match self {
            Self::Path(path) | Self::Described { path } => path,
        }
    }
}

/// A top-level volume or network.
#[derive(Debug, Default, Deserialize)]
pub struct Resource {
    /// The resolved name; Compose gives every resource one.
    #[serde(default)]
    pub name: Option<String>,
    /// `external`: `true`, or an object.
    #[serde(default)]
    pub external: Option<serde_json::Value>,
}

impl Resource {
    /// Whether the resource is declared `external`.
    pub fn is_external(&self) -> bool {
        match &self.external {
            Some(serde_json::Value::Bool(value)) => *value,
            Some(serde_json::Value::Object(_)) => true,
            _ => false,
        }
    }
}

impl ComposeConfig {
    /// Parses the JSON Compose printed.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TextOrNumber {
    Text(String),
    Number(u64),
}

fn text_or_number<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(
        Option::<TextOrNumber>::deserialize(deserializer)?.map(|value| match value {
            TextOrNumber::Text(text) => text,
            TextOrNumber::Number(number) => number.to_string(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn parse(text: &str) -> ComposeConfig {
        ComposeConfig::from_json(text).unwrap()
    }

    #[test]
    fn should_read_the_published_port_as_text_when_compose_prints_a_string() {
        let config = parse(
            r#"{"services":{"db":{"ports":[{"target":5432,"published":"61000","protocol":"tcp"}]}}}"#,
        );

        assert_eq!(
            config.services["db"].ports[0].published.as_deref(),
            Some("61000")
        );
    }

    #[test]
    fn should_read_the_published_port_as_text_when_compose_prints_a_number() {
        let config = parse(r#"{"services":{"db":{"ports":[{"target":5432,"published":61000}]}}}"#);

        assert_eq!(
            config.services["db"].ports[0].published.as_deref(),
            Some("61000")
        );
    }

    #[test]
    fn should_default_to_no_ports_when_the_service_has_null_ports() {
        let config = parse(r#"{"services":{"smtp":{"ports":null}}}"#);

        assert!(config.services["smtp"].ports.is_empty());
    }

    #[test]
    fn should_read_both_forms_of_env_file_entry() {
        let config = parse(
            r#"{"services":{"app":{"env_file":[{"path":"/r/.env","required":true},"/r/other"]}}}"#,
        );

        let paths: Vec<&str> = config.services["app"]
            .env_file
            .iter()
            .map(EnvFileEntry::path)
            .collect();
        assert_eq!(paths, ["/r/.env", "/r/other"]);
    }

    #[test]
    fn should_not_count_a_passthrough_variable_as_pinned_when_its_value_is_null() {
        let config =
            parse(r#"{"services":{"app":{"environment":{"PORT":null,"HOST":"0.0.0.0"}}}}"#);

        let service = &config.services["app"];
        assert_eq!((service.pins("PORT"), service.pins("HOST")), (false, true));
    }

    #[test]
    fn should_tell_external_resources_apart_when_external_is_true_or_an_object() {
        let config = parse(
            r#"{"volumes":{"a":{"name":"x","external":true},"b":{"name":"y","external":{}},"c":{"name":"z"},"d":{"name":"w","external":false}}}"#,
        );

        let flags: Vec<bool> = ["a", "b", "c", "d"]
            .iter()
            .map(|key| config.volumes[*key].is_external())
            .collect();
        assert_eq!(flags, [true, true, false, false]);
    }

    #[test]
    fn should_read_bind_mounts_with_their_source() {
        let config = parse(
            r#"{"services":{"app":{"volumes":[{"type":"bind","source":"/etc/x","target":"/x"}]}}}"#,
        );

        assert_eq!(
            config.services["app"].volumes[0].source.as_deref(),
            Some("/etc/x")
        );
    }
}
