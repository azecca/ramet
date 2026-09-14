//! `.ramet.json`: how a project runs under ramet, versioned with it.
//!
//! ```json
//! {
//!   "compose": {
//!     "files": ["docker/compose/base.yml", "docker/compose/dev.yml"],
//!     "profiles": ["dev"]
//!   },
//!   "ports": {"web": "traefik:80"},
//!   "sync": [".env", "apps/*/.env", "config/local.toml"]
//! }
//! ```
//!
//! The file and each of its keys are optional. Without `compose.files`,
//! compose finds its files by itself; without `sync`, the `.env` files git
//! does not track are synced. An unknown key is refused rather than ignored,
//! so that a typo (`"synk"`) never silently turns a setting off.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path};

use serde::Deserialize;

use crate::error::{Error, Result};

/// Name of the file, at the root of a worktree.
pub const FILE_NAME: &str = ".ramet.json";

/// The sync patterns of a project whose `.ramet.json` says nothing: every
/// `.env` file git does not track, wherever it is.
pub const DEFAULT_SYNC: [&str; 2] = ["**/.env", "**/.env.*"];

/// Prefix of the variable holding a named port: `web` is `RAMET_PORT_WEB`.
pub const PORT_VARIABLE_PREFIX: &str = "RAMET_PORT_";

/// The content of a `.ramet.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// How compose runs the project.
    #[serde(default)]
    pub compose: ComposeSettings,
    /// Names given to published ports, each a `service:container_port`.
    #[serde(default)]
    ports: BTreeMap<String, String>,
    /// Paths and glob patterns of the local files synced between envs,
    /// relative to the worktree; `None` when the file does not say.
    #[serde(default)]
    sync: Option<Vec<String>>,
}

/// The `compose` section of a `.ramet.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposeSettings {
    /// Compose files, relative to the worktree, in the order of `-f` options.
    #[serde(default)]
    pub files: Vec<String>,
    /// Compose profiles enabled for every command.
    #[serde(default)]
    pub profiles: Vec<String>,
}

impl Settings {
    /// Reads the `.ramet.json` of `worktree`, or the defaults when it has none.
    pub fn load(worktree: &Path) -> Result<Self> {
        let path = worktree.join(FILE_NAME);
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text, &path),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::io(&path, source)),
        }
    }

    /// Parses the content of a `.ramet.json`; `path` names it in errors.
    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        let invalid = |detail: String| Error::InvalidSettings {
            path: path.to_owned(),
            detail,
        };
        let settings: Self = serde_json::from_str(text).map_err(|err| invalid(err.to_string()))?;
        // Every env has its own worktree: a path leaving it would point into
        // one worktree for all of them.
        let paths = [
            ("compose.files", Some(&settings.compose.files)),
            ("sync", settings.sync.as_ref()),
        ];
        for (key, list) in paths {
            if let Some(path) = list.into_iter().flatten().find(|path| !stays_inside(path)) {
                return Err(invalid(format!(
                    "{key}: \"{path}\" must be a path relative to the worktree, inside it"
                )));
            }
        }
        let mut variables = BTreeMap::new();
        for (name, key) in &settings.ports {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(invalid(format!(
                    "ports: \"{name}\" must be made of letters, digits, `-` and `_`"
                )));
            }
            if !is_port_key(key) {
                return Err(invalid(format!(
                    "ports.{name}: \"{key}\" must be a published port, as `service:container_port`"
                )));
            }
            if let Some(other) = variables.insert(port_variable(name), name) {
                return Err(invalid(format!(
                    "ports: \"{other}\" and \"{name}\" are both {}",
                    port_variable(name)
                )));
            }
        }
        Ok(settings)
    }

    /// Each named port, with its `service:container_port`, sorted by name.
    pub fn named_ports(&self) -> impl Iterator<Item = (&str, &String)> {
        self.ports.iter().map(|(name, key)| (name.as_str(), key))
    }

    /// The variable of each named port, with its `service:container_port`,
    /// sorted by variable.
    pub fn port_variables(&self) -> Vec<(String, String)> {
        let mut variables: Vec<(String, String)> = self
            .ports
            .iter()
            .map(|(name, key)| (port_variable(name), key.clone()))
            .collect();
        variables.sort();
        variables
    }

    /// The sync patterns: the file's, or [`DEFAULT_SYNC`] when it says nothing.
    pub fn sync(&self) -> Vec<String> {
        self.sync.clone().unwrap_or_else(|| {
            DEFAULT_SYNC
                .iter()
                .map(|&pattern| pattern.to_owned())
                .collect()
        })
    }

    /// `profiles` followed by `extra`, without duplicates.
    pub fn profiles_with(&self, extra: &[String]) -> Vec<String> {
        let mut profiles = self.compose.profiles.clone();
        for profile in extra {
            if !profiles.contains(profile) {
                profiles.push(profile.clone());
            }
        }
        profiles
    }
}

/// The variable holding the port named `name`: `keycloak-db` is
/// `RAMET_PORT_KEYCLOAK_DB`.
pub fn port_variable(name: &str) -> String {
    let suffix: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{PORT_VARIABLE_PREFIX}{suffix}")
}

/// Whether `key` reads as `service:container_port`.
fn is_port_key(key: &str) -> bool {
    key.rsplit_once(':').is_some_and(|(service, port)| {
        !service.is_empty() && port.parse::<u16>().is_ok_and(|port| port > 0)
    })
}

/// Whether `pattern` is a non-empty relative path that never climbs out of
/// the worktree.
fn stays_inside(pattern: &str) -> bool {
    let path = Path::new(pattern);
    !pattern.trim().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::assert_matches;

    fn parse(text: &str) -> Result<Settings> {
        Settings::parse(text, Path::new("/code/app/.ramet.json"))
    }

    #[test]
    fn an_empty_object_means_the_defaults() {
        let settings = parse("{}").unwrap();
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.sync(), ["**/.env", "**/.env.*"]);
        assert!(settings.compose.files.is_empty() && settings.compose.profiles.is_empty());
    }

    #[test]
    fn reads_every_key() {
        let settings = parse(
            r#"{
              "compose": {"files": ["docker/compose/base.yml", "docker/compose/dev.yml"], "profiles": ["dev"]},
              "sync": [".env", "config/local.toml"]
            }"#,
        )
        .unwrap();
        assert_eq!(
            settings.compose.files,
            ["docker/compose/base.yml", "docker/compose/dev.yml"]
        );
        assert_eq!(settings.compose.profiles, ["dev"]);
        assert_eq!(settings.sync(), [".env", "config/local.toml"]);
    }

    #[test]
    fn an_explicit_sync_replaces_the_default() {
        assert!(parse(r#"{"sync": []}"#).unwrap().sync().is_empty());
        assert_eq!(
            parse(r#"{"sync": ["local.toml"]}"#).unwrap().sync(),
            ["local.toml"]
        );
    }

    #[test]
    fn a_typo_is_refused_rather_than_ignored() {
        let err = parse(r#"{"synk": [".env"]}"#).unwrap_err();
        assert_matches!(err, Error::InvalidSettings { .. });
        let message = err.to_string();
        assert!(
            message.contains("/code/app/.ramet.json") && message.contains("synk"),
            "{message}"
        );
        assert!(parse(r#"{"compose": {"file": ["a.yml"]}}"#).is_err());
    }

    #[test]
    fn malformed_json_is_refused_with_its_position() {
        let err = parse("{\"sync\": [\".env\",]}").unwrap_err();
        assert!(err.to_string().contains("line 1"), "{err}");
        assert!(parse(r#"{"sync": ".env"}"#).is_err(), "a list is expected");
    }

    #[test]
    fn a_pattern_leaving_the_worktree_is_refused() {
        for pattern in ["../shared/.env", "/etc/hosts", "config/../../x", ""] {
            let text = serde_json::json!({ "sync": [pattern] }).to_string();
            assert_matches!(
                parse(&text),
                Err(Error::InvalidSettings { .. }),
                "{pattern}"
            );
        }
        assert!(parse(r#"{"sync": ["./config", "apps/*/.env"]}"#).is_ok());
        let err = parse(r#"{"compose": {"files": ["/code/app/compose.yml"]}}"#).unwrap_err();
        assert!(err.to_string().contains("compose.files"), "{err}");
    }

    #[test]
    fn extra_profiles_are_appended_without_duplicates() {
        let settings = parse(r#"{"compose": {"profiles": ["debug"]}}"#).unwrap();
        assert_eq!(
            settings.profiles_with(&["tools".into(), "debug".into()]),
            ["debug", "tools"]
        );
    }

    #[test]
    fn named_ports_become_variables() {
        let settings =
            parse(r#"{"ports": {"web": "traefik:80", "keycloak-db": "keycloak-db:5432"}}"#)
                .unwrap();
        assert_eq!(
            settings.port_variables(),
            [
                (
                    "RAMET_PORT_KEYCLOAK_DB".to_owned(),
                    "keycloak-db:5432".to_owned()
                ),
                ("RAMET_PORT_WEB".to_owned(), "traefik:80".to_owned()),
            ]
        );
        assert!(parse("{}").unwrap().port_variables().is_empty());
    }

    #[test]
    fn a_malformed_named_port_is_refused() {
        for ports in [
            json!({"web": "traefik"}),
            json!({"web": "traefik:http"}),
            json!({"web": ":80"}),
            json!({"web": "traefik:70000"}),
            json!({"": "traefik:80"}),
            json!({"web port": "traefik:80"}),
            json!({"web": 80}),
        ] {
            let text = json!({ "ports": ports }).to_string();
            assert_matches!(parse(&text), Err(Error::InvalidSettings { .. }), "{text}");
        }
        let err = parse(r#"{"ports": {"web": "traefik"}}"#).unwrap_err();
        assert!(err.to_string().contains("ports.web"), "{err}");
    }

    #[test]
    fn two_names_giving_the_same_variable_are_refused() {
        let err = parse(r#"{"ports": {"key-db": "db:5432", "key_db": "db:5433"}}"#).unwrap_err();
        assert!(err.to_string().contains("RAMET_PORT_KEY_DB"), "{err}");
    }

    #[test]
    fn a_missing_file_means_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), Settings::default());
        fs::write(dir.path().join(FILE_NAME), r#"{"sync": []}"#).unwrap();
        assert!(Settings::load(dir.path()).unwrap().sync().is_empty());
    }
}
