//! What `ds reset <service>` deletes, decided from the merged compose model.
//! Only keys are kept: no resolved string is ever printed.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::compose::model::{ComposeConfig, Mount};

const VOLUME_MOUNT: &str = "volume";
const BIND_MOUNT: &str = "bind";

/// The data of one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceReset {
    /// The keys of the named volumes the service mounts.
    pub volume_keys: Vec<String>,
    /// Whether the service has bind mounts, which are never reset.
    pub has_bind_mounts: bool,
}

/// Why a service cannot be reset alone.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResetRefusal {
    /// The compose model has no such service.
    #[error("the dev compose has no service named {service}")]
    UnknownService {
        /// The service that was asked for.
        service: String,
    },
    /// Another service mounts one of the volumes.
    #[error(
        "the volume {volume} of {service} is also mounted by {other}; reset them all with ds reset"
    )]
    SharedVolume {
        /// The volume key.
        volume: String,
        /// The service that was asked for.
        service: String,
        /// The other service that mounts it.
        other: String,
    },
}

/// What resetting `service` deletes, or why it cannot be done alone.
pub fn plan_service_reset(
    config: &ComposeConfig,
    service: &str,
) -> Result<ServiceReset, ResetRefusal> {
    let Some(model) = config.services.get(service) else {
        return Err(ResetRefusal::UnknownService {
            service: service.to_owned(),
        });
    };
    let volume_keys: Vec<String> = model
        .volumes
        .iter()
        .filter_map(named_volume)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for volume in &volume_keys {
        if let Some(other) = other_service_mounting(config, service, volume) {
            return Err(ResetRefusal::SharedVolume {
                volume: volume.clone(),
                service: service.to_owned(),
                other,
            });
        }
    }
    Ok(ServiceReset {
        volume_keys,
        has_bind_mounts: model.volumes.iter().any(|mount| mount.kind == BIND_MOUNT),
    })
}

fn named_volume(mount: &Mount) -> Option<String> {
    if mount.kind != VOLUME_MOUNT {
        return None;
    }
    mount.source.clone().filter(|source| !source.is_empty())
}

fn other_service_mounting(config: &ComposeConfig, service: &str, volume: &str) -> Option<String> {
    config
        .services
        .iter()
        .filter(|(name, _)| name.as_str() != service)
        .find(|(_, other)| {
            other
                .volumes
                .iter()
                .any(|mount| named_volume(mount).as_deref() == Some(volume))
        })
        .map(|(name, _)| name.clone())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn config(json: &str) -> ComposeConfig {
        ComposeConfig::from_json(json).unwrap()
    }

    const TWO_SERVICES: &str = r#"{"services":{
        "postgres":{"volumes":[
            {"type":"volume","source":"pg-data","target":"/data"},
            {"type":"bind","source":"/etc/x","target":"/x"},
            {"type":"volume","target":"/anonymous"}]},
        "redis":{"volumes":[{"type":"volume","source":"redis-data","target":"/data"}]}}}"#;

    #[test]
    fn should_collect_the_named_volume_keys_and_note_the_bind_mounts() {
        let plan = plan_service_reset(&config(TWO_SERVICES), "postgres").unwrap();

        assert_eq!(
            plan,
            ServiceReset {
                volume_keys: vec!["pg-data".to_owned()],
                has_bind_mounts: true,
            }
        );
    }

    #[test]
    fn should_plan_a_service_without_any_mount() {
        let plan = plan_service_reset(&config(r#"{"services":{"app":{}}}"#), "app").unwrap();

        assert_eq!(
            plan,
            ServiceReset {
                volume_keys: vec![],
                has_bind_mounts: false,
            }
        );
    }

    #[test]
    fn should_refuse_a_service_the_model_does_not_have() {
        let refusal = plan_service_reset(&config(TWO_SERVICES), "mysql").unwrap_err();

        assert_eq!(
            refusal,
            ResetRefusal::UnknownService {
                service: "mysql".to_owned()
            }
        );
    }

    #[test]
    fn should_refuse_and_name_the_volume_and_the_other_service_when_a_volume_is_shared() {
        let shared = r#"{"services":{
            "a":{"volumes":[{"type":"volume","source":"data","target":"/a"}]},
            "b":{"volumes":[{"type":"volume","source":"data","target":"/b"}]}}}"#;

        let refusal = plan_service_reset(&config(shared), "a").unwrap_err();

        assert_eq!(
            refusal,
            ResetRefusal::SharedVolume {
                volume: "data".to_owned(),
                service: "a".to_owned(),
                other: "b".to_owned(),
            }
        );
    }

    #[test]
    fn should_not_count_a_bind_mount_of_the_same_source_as_sharing() {
        let mixed = r#"{"services":{
            "a":{"volumes":[{"type":"volume","source":"data","target":"/a"}]},
            "b":{"volumes":[{"type":"bind","source":"data","target":"/b"}]}}}"#;

        assert!(plan_service_reset(&config(mixed), "a").is_ok());
    }
}
