//! The resolved compose configuration, and its rewriting for an env.
//!
//! This is the heart of ramet: the configuration resolved by compose itself
//! is rewritten so that envs never share a volume, a network, a container or
//! a host port. Two traps are fatal and handled here: keeping the `name` of a
//! volume, and keeping the `name` of a network.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::error::{Error, Result};
use crate::ports::{PortMap, port_key};

/// Configuration printed by `docker compose config --format json`.
///
/// Kept as raw JSON: every key ramet does not rewrite must reach compose
/// untouched, including keys newer compose versions may introduce.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComposeConfig(Map<String, Value>);

/// Named volumes declared at the top level of a configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeclaredVolumes {
    /// Volumes ramet stores in the env's subvolume.
    pub managed: BTreeSet<String>,
    /// `external: true` volumes, left as they are and not isolated.
    pub external: BTreeSet<String>,
}

impl ComposeConfig {
    /// Parses the output of `docker compose config --format json`.
    pub fn from_json(text: &str) -> Result<Self> {
        serde_json::from_str(text)
            .map(Self)
            .map_err(Error::ComposeConfigUnreadable)
    }

    /// Wraps a JSON object.
    pub fn from_map(map: Map<String, Value>) -> Self {
        Self(map)
    }

    /// The configuration as JSON.
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    /// The top-level project name, when compose set one.
    pub fn name(&self) -> Option<&str> {
        self.0
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
    }

    fn section(&self, key: &str) -> impl Iterator<Item = (&String, &Value)> {
        self.0
            .get(key)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
    }

    /// Names of the services.
    pub fn service_names(&self) -> BTreeSet<String> {
        self.section("services")
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Top-level named volumes, split between managed and external ones.
    pub fn declared_volumes(&self) -> DeclaredVolumes {
        let mut volumes = DeclaredVolumes::default();
        for (name, spec) in self.section("volumes") {
            let set = if is_external(spec) {
                &mut volumes.external
            } else {
                &mut volumes.managed
            };
            set.insert(name.clone());
        }
        volumes
    }

    /// Refuses a volume ramet would store whose name could leave its directory
    /// (`..`), or whose docker name `docker volume inspect` would read as an
    /// option (`--help`).
    pub fn check_volume_names(&self) -> Result<()> {
        for volume in self.declared_volumes().managed {
            let docker = self.volume_name(&volume).map(str::to_owned);
            for name in std::iter::once(volume).chain(docker) {
                if !crate::util::name::is_plain(&name) {
                    return Err(Error::InvalidVolumeName { name });
                }
            }
        }
        Ok(())
    }

    /// The docker name compose gives a volume, when the configuration states it.
    pub fn volume_name(&self, volume: &str) -> Option<&str> {
        self.0
            .get("volumes")?
            .get(volume)?
            .get("name")?
            .as_str()
            .filter(|name| !name.is_empty())
    }

    /// Keys (`service:container_port`) of the ports published on a fixed host
    /// port, services in alphabetical order, without duplicates.
    ///
    /// Ports without a host port are left to compose, which picks one; so are
    /// host port ranges, in which compose picks a free port for each env.
    pub fn published_ports(&self) -> Vec<String> {
        let mut keys = Vec::new();
        for (service, port) in self.published() {
            let key = port_key(service, &container_port(port));
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }

    /// The host ports as the project publishes them: the identity mapping
    /// kept by the main env unless it was initialized with `--remap-ports`.
    pub fn original_port_map(&self) -> PortMap {
        self.published()
            .filter_map(|(service, port)| {
                let host = host_port(port)?;
                Some((port_key(service, &container_port(port)), host))
            })
            .collect()
    }

    /// Services whose container name is pinned by `container_name`.
    pub fn fixed_container_names(&self) -> BTreeMap<String, String> {
        self.section("services")
            .filter_map(|(service, definition)| {
                let name = definition.get("container_name")?.as_str()?;
                (!name.is_empty()).then(|| (service.clone(), name.to_owned()))
            })
            .collect()
    }

    /// Port specifications published on a fixed host port, paired with their
    /// service.
    fn published(&self) -> impl Iterator<Item = (&str, &Map<String, Value>)> {
        self.section("services").flat_map(|(service, definition)| {
            definition
                .get("ports")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_object)
                .filter(|port| is_fixed(port))
                .map(move |port| (service.as_str(), port))
        })
    }

    /// The configuration rewritten for an env whose named volumes live under
    /// `volumes_dir` and whose host ports are `ports`.
    ///
    /// - Named volumes become bind mounts into `volumes_dir`. The whole
    ///   definition is replaced: the `name` compose resolves would pin the
    ///   docker volume name and stop compose from prefixing it with the
    ///   project name, so every env would share the same volume.
    /// - Networks lose their `name`, for the same reason: kept, every env
    ///   would share one docker network and a `down` in one would cut the
    ///   others off.
    /// - Services lose `container_name`, which pins the container name and
    ///   would make two envs want the same container.
    /// - Published ports get the env's host port; `target`, `protocol`,
    ///   `mode` and `host_ip` are left intact, and `published` stays a string.
    /// - The top-level `name` goes: the project name is passed with `-p`.
    ///
    /// External volumes and networks, and bind mounts, are left as they are.
    #[must_use]
    pub fn for_env(&self, volumes_dir: &Path, ports: &PortMap) -> Self {
        let mut config = self.0.clone();
        config.remove("name");

        if let Some(volumes) = config.get_mut("volumes").and_then(Value::as_object_mut) {
            for (name, spec) in volumes.iter_mut().filter(|(_, spec)| !is_external(spec)) {
                *spec = json!({
                    "driver": "local",
                    "driver_opts": {
                        "type": "none",
                        "o": "bind",
                        "device": volumes_dir.join(name).to_string_lossy(),
                    },
                });
            }
        }

        if let Some(networks) = config.get_mut("networks").and_then(Value::as_object_mut) {
            for spec in networks.values_mut().filter(|spec| !is_external(spec)) {
                if let Some(spec) = spec.as_object_mut() {
                    spec.remove("name");
                }
            }
        }

        if let Some(services) = config.get_mut("services").and_then(Value::as_object_mut) {
            for (service, definition) in services.iter_mut() {
                let Some(definition) = definition.as_object_mut() else {
                    continue;
                };
                definition.remove("container_name");
                let published = definition
                    .get_mut("ports")
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_object_mut)
                    .filter(|port| is_fixed(port));
                for port in published {
                    let key = port_key(service, &container_port(port));
                    if let Some(host) = ports.get(&key) {
                        port.insert("published".to_owned(), Value::String(host.to_string()));
                    }
                }
            }
        }

        Self(config)
    }

    /// The configuration as pretty JSON with sorted keys, as written to disk.
    pub fn to_json(&self) -> String {
        crate::util::fs::to_json_pretty(&self.0)
    }
}

/// Whether a volume or network is declared `external`.
fn is_external(spec: &Value) -> bool {
    spec.get("external").is_some_and(is_truthy)
}

/// JSON truthiness as compose values use it: absent, null, false, 0 and ""
/// all mean "not set".
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(entries) => !entries.is_empty(),
    }
}

/// The container port of a port specification, as text.
fn container_port(port: &Map<String, Value>) -> String {
    match port.get("target") {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Whether a port specification publishes a single, fixed host port.
fn is_fixed(port: &Map<String, Value>) -> bool {
    port.get("published").is_some_and(is_truthy) && host_port(port).is_some()
}

/// The host port of a port specification, when it is a single port number.
fn host_port(port: &Map<String, Value>) -> Option<u16> {
    match port.get("published")? {
        Value::String(text) => text.parse().ok(),
        Value::Number(number) => number.as_u64().and_then(|n| u16::try_from(n).ok()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    /// What `docker compose config --format json` prints for the example project.
    fn example() -> ComposeConfig {
        let value = json!({
            "name": "example",
            "services": {
                "web": {
                    "image": "nginx:alpine",
                    "ports": [{"mode": "ingress", "target": 80, "published": "8080", "protocol": "tcp"}],
                    "volumes": [
                        {"type": "volume", "source": "uploads", "target": "/up"},
                        {"type": "bind", "source": "/home/alex/app/src", "target": "/src"},
                    ],
                },
                "db": {
                    "image": "postgres:16-alpine",
                    "ports": [{"mode": "ingress", "target": 5432, "published": "5432",
                               "protocol": "tcp", "host_ip": "127.0.0.1"}],
                },
            },
            "networks": {"default": {"name": "example_default"}},
            "volumes": {"pgdata": {"name": "example_pgdata"}, "uploads": {"name": "example_uploads"}},
        });
        from_value(value)
    }

    fn from_value(value: Value) -> ComposeConfig {
        match value {
            Value::Object(map) => ComposeConfig::from_map(map),
            other => panic!("not an object: {other}"),
        }
    }

    fn transform(config: &ComposeConfig, ports: &[(&str, u16)]) -> Value {
        let ports = ports
            .iter()
            .map(|&(key, port)| (key.to_owned(), port))
            .collect();
        Value::Object(
            config
                .for_env(Path::new("/srv/ramet/example/feat-a/volumes"), &ports)
                .as_map()
                .clone(),
        )
    }

    #[test]
    fn a_volume_becomes_a_bind_to_the_subvolume() {
        assert_eq!(
            transform(&example(), &[])["volumes"]["pgdata"],
            json!({"driver": "local", "driver_opts": {
                "type": "none", "o": "bind",
                "device": "/srv/ramet/example/feat-a/volumes/pgdata"}})
        );
    }

    #[test]
    fn the_volume_name_disappears() {
        assert!(
            transform(&example(), &[])["volumes"]["pgdata"]
                .get("name")
                .is_none()
        );
    }

    #[test]
    fn a_volume_name_that_is_no_plain_name_is_refused() {
        for (key, name) in [
            ("..", "demo_x"),
            (".", "demo_x"),
            ("-rf", "demo_x"),
            ("data", "--help"),
        ] {
            let config = ComposeConfig::from_map(
                json!({"volumes": {key: {"name": name}}})
                    .as_object()
                    .unwrap()
                    .clone(),
            );
            assert!(
                matches!(
                    config.check_volume_names(),
                    Err(Error::InvalidVolumeName { .. })
                ),
                "{key} {name}"
            );
        }
        let external = ComposeConfig::from_map(
            json!({"volumes": {"-shared": {"external": true, "name": "--x"}}})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert!(
            external.check_volume_names().is_ok(),
            "ramet leaves those alone"
        );
        assert!(example().check_volume_names().is_ok());
    }

    #[test]
    fn an_external_volume_is_left_alone() {
        let mut value = Value::Object(example().as_map().clone());
        value["volumes"]["shared"] = json!({"external": true, "name": "shared"});
        let result = transform(&from_value(value), &[]);
        assert_eq!(
            result["volumes"]["shared"],
            json!({"external": true, "name": "shared"})
        );
    }

    #[test]
    fn the_network_name_disappears() {
        assert!(
            transform(&example(), &[])["networks"]["default"]
                .get("name")
                .is_none()
        );
    }

    #[test]
    fn an_external_network_is_left_alone() {
        let mut value = Value::Object(example().as_map().clone());
        value["networks"]["outside"] = json!({"external": true, "name": "outside"});
        let result = transform(&from_value(value), &[]);
        assert_eq!(result["networks"]["outside"]["name"], "outside");
    }

    #[test]
    fn only_published_is_replaced() {
        let result = transform(&example(), &[("db:5432", 30_001)]);
        assert_eq!(
            result["services"]["db"]["ports"][0],
            json!({"mode": "ingress", "target": 5432, "published": "30001",
                   "protocol": "tcp", "host_ip": "127.0.0.1"})
        );
    }

    #[test]
    fn a_port_missing_from_the_mapping_is_untouched() {
        let result = transform(&example(), &[]);
        assert_eq!(result["services"]["web"]["ports"][0]["published"], "8080");
    }

    #[test]
    fn a_port_without_published_is_ignored() {
        let mut value = Value::Object(example().as_map().clone());
        value["services"]["web"]["ports"] =
            json!([{"mode": "ingress", "target": 80, "protocol": "tcp"}]);
        let result = transform(&from_value(value), &[("web:80", 30_000)]);
        assert!(
            result["services"]["web"]["ports"][0]
                .get("published")
                .is_none()
        );
    }

    #[test]
    fn container_name_disappears_from_the_copy_only() {
        let mut value = Value::Object(example().as_map().clone());
        value["services"]["db"]["container_name"] = json!("app_db");
        let config = from_value(value);
        let result = transform(&config, &[]);
        assert!(result["services"]["db"].get("container_name").is_none());
        assert_eq!(config.fixed_container_names()["db"], "app_db");
    }

    #[test]
    fn the_top_level_name_disappears() {
        assert!(transform(&example(), &[]).get("name").is_none());
    }

    #[test]
    fn bind_mounts_are_untouched() {
        let result = transform(&example(), &[]);
        assert_eq!(
            result["services"]["web"]["volumes"][1],
            json!({"type": "bind", "source": "/home/alex/app/src", "target": "/src"})
        );
    }

    #[test]
    fn the_original_is_not_modified() {
        let config = example();
        let _ = transform(&config, &[("web:80", 30_000)]);
        assert_eq!(config, example());
    }

    #[test]
    fn a_configuration_without_volumes_or_networks() {
        let _ = transform(&from_value(json!({"services": {}})), &[]);
    }

    #[test]
    fn splits_managed_and_external_volumes() {
        let mut value = Value::Object(example().as_map().clone());
        value["volumes"]["shared"] = json!({"external": true});
        let volumes = from_value(value).declared_volumes();
        assert_eq!(
            volumes.managed,
            BTreeSet::from(["pgdata".to_owned(), "uploads".to_owned()])
        );
        assert_eq!(volumes.external, BTreeSet::from(["shared".to_owned()]));
    }

    fn with_ports(ports: &Value) -> ComposeConfig {
        from_value(json!({"services": {"web": {"ports": ports}}}))
    }

    #[test]
    fn published_ports_are_sorted_by_service() {
        assert_eq!(example().published_ports(), vec!["db:5432", "web:80"]);
    }

    #[test]
    fn published_ports_skip_ports_without_a_host_port() {
        let config = with_ports(&json!([{"target": 80}, {"target": 443, "published": "8443"}]));
        assert_eq!(config.published_ports(), vec!["web:443"]);
    }

    #[test]
    fn a_host_port_range_is_left_to_compose() {
        // Compose picks a free port in the range, for every env alike.
        let config = with_ports(&json!([
            {"target": 80, "published": "8000-8010"}, {"target": 443, "published": "8443"}]));
        assert_eq!(config.published_ports(), vec!["web:443"]);
        assert_eq!(
            config.original_port_map(),
            PortMap::from([("web:443".to_owned(), 8443)])
        );
        let result = transform(&config, &[("web:80", 30_000), ("web:443", 30_001)]);
        assert_eq!(
            result["services"]["web"]["ports"][0]["published"],
            "8000-8010"
        );
        assert_eq!(result["services"]["web"]["ports"][1]["published"], "30001");
    }

    #[test]
    fn published_ports_are_deduplicated() {
        let config = with_ports(&json!([
            {"target": 80, "published": "8080"}, {"target": 80, "published": "9090"}]));
        assert_eq!(config.published_ports(), vec!["web:80"]);
    }

    #[test]
    fn service_without_ports_and_empty_configuration() {
        assert!(
            from_value(json!({"services": {"web": {}}}))
                .published_ports()
                .is_empty()
        );
        assert!(ComposeConfig::default().published_ports().is_empty());
    }

    #[test]
    fn original_port_map_is_the_identity() {
        assert_eq!(
            example().original_port_map(),
            PortMap::from([("db:5432".to_owned(), 5432), ("web:80".to_owned(), 8080)])
        );
    }

    #[test]
    fn rejects_output_that_is_not_an_object() {
        assert_matches!(
            ComposeConfig::from_json("[1, 2]"),
            Err(Error::ComposeConfigUnreadable(_))
        );
    }
}
