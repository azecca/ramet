//! Environments: a git worktree, a btrfs subvolume and a compose project.
//!
//! An env is attached to its worktree, not to a branch: `git checkout` inside
//! a worktree leaves the data alone, like untracked files. Its metadata lives
//! in `env.json` at the root of its subvolume; how compose runs it lives in
//! the `.ramet.json` of its worktree, versioned with the project.

pub mod inventory;
pub mod store;
pub mod sync;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::compose::{ComposeConfig, Interpolation, Stack};
use crate::context::Context;
use crate::error::Result;
use crate::layout::Layout;
use crate::ports::{self, PortMap, PortRange};
use crate::settings::{Settings, port_variable};
use crate::util::fs::{create_dir_all, write_atomic, write_json};

/// The content of an `env.json` file.
///
/// Unknown keys are preserved, so that a file written by another version of
/// ramet survives being rewritten by this one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Env {
    /// Name of the env, unique within its project.
    pub name: String,
    /// Project the env belongs to.
    pub project: String,
    /// Env it was cloned from; `None` for the env of the main clone.
    pub parent: Option<String>,
    /// Root of its git worktree.
    pub worktree: PathBuf,
    /// Branch checked out when the env was created.
    #[serde(default)]
    pub branch_at_creation: Option<String>,
    /// Creation time, ISO 8601.
    #[serde(default)]
    pub created_at: Option<String>,
    /// Host ports.
    #[serde(default)]
    pub ports: Ports,
    /// Checkpoints, by label.
    #[serde(default)]
    pub checkpoints: BTreeMap<String, Checkpoint>,
    /// Keys this version does not know about.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Host ports of an env.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ports {
    /// Block reserved for the env; `None` when it keeps the project's own
    /// ports (the main env, unless initialized with `--remap-ports`).
    #[serde(default)]
    pub range: Option<PortRange>,
    /// Host port of each published container port.
    #[serde(default)]
    pub map: PortMap,
    /// Keys this version does not know about.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Metadata of a checkpoint.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Creation time, ISO 8601.
    #[serde(default)]
    pub created_at: Option<String>,
    /// Commit checked out in the worktree when the checkpoint was taken.
    #[serde(default)]
    pub head: Option<String>,
    /// Message given with `-m`.
    #[serde(default)]
    pub message: Option<String>,
    /// Keys this version does not know about.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Env {
    /// Compose project name of the env: `<project>-<env>`.
    pub fn compose_project(&self) -> String {
        format!("{}-{}", self.project, self.name)
    }

    /// Whether this is the env of the main clone, recognized by its missing
    /// parent rather than by its name, which `init --name` makes free.
    pub fn is_primary(&self) -> bool {
        self.parent.is_none()
    }

    /// The env's subvolume.
    pub fn dir(&self, layout: &Layout) -> PathBuf {
        layout.env_dir(&self.project, &self.name)
    }

    /// How docker compose addresses the env's stack.
    pub fn stack(&self, layout: &Layout) -> Stack {
        Stack {
            project_dir: self.worktree.clone(),
            name: self.compose_project(),
            file: layout.compose_file(&self.project, &self.name),
        }
    }

    /// Writes `env.json`.
    pub fn save(&self, layout: &Layout) -> Result<()> {
        write_json(&layout.env_file(&self.project, &self.name), self)
    }

    /// The `.ramet.json` of the env's worktree.
    pub fn settings(&self) -> Result<Settings> {
        Settings::load(&self.worktree)
    }

    /// The compose configuration of the env's worktree, with the files and
    /// profiles of its `.ramet.json` and, for this invocation only,
    /// `extra_profiles`.
    ///
    /// Read from the worktree every time, like the compose files themselves:
    /// a branch that reorganizes its compose files brings its own
    /// `.ramet.json` along. The files interpolate the env's compose project
    /// and named ports: see [`Env::interpolation`].
    pub fn resolve(&self, ctx: &Context, extra_profiles: &[String]) -> Result<ComposeConfig> {
        let settings = self.settings()?;
        self.resolve_with(ctx, &settings, extra_profiles)
    }

    fn resolve_with(
        &self,
        ctx: &Context,
        settings: &Settings,
        extra_profiles: &[String],
    ) -> Result<ComposeConfig> {
        ctx.compose().resolve(
            &self.worktree,
            &settings.profiles_with(extra_profiles),
            &settings.compose.files,
            &self.interpolation(settings),
        )
    }

    /// What the env's compose files interpolate on top of the developer's
    /// variables: `${COMPOSE_PROJECT_NAME}`, the env's compose project, and
    /// the variable of each port named in `settings`.
    ///
    /// A named port has a value only in an env with its own block of ports.
    /// Elsewhere, as for a developer without ramet, it is unset, whatever the
    /// shell says: `${RAMET_PORT_WEB:-80}` then gives the project's port, and
    /// `http://app.test${RAMET_PORT_WEB:+:$RAMET_PORT_WEB}` an address
    /// without one.
    pub fn interpolation(&self, settings: &Settings) -> Interpolation {
        Interpolation {
            project: Some(self.compose_project()),
            variables: settings
                .port_variables()
                .into_iter()
                .map(|(variable, key)| {
                    (variable, self.named_port(&key).map(|port| port.to_string()))
                })
                .collect(),
        }
    }

    /// The value of a port named after the published port `key`: its host
    /// port in an env with its own block, `None` elsewhere.
    pub fn named_port(&self, key: &str) -> Option<u16> {
        self.ports
            .range
            .and_then(|_| self.ports.map.get(key).copied())
    }

    /// Regenerates the env's compose configuration; called before every
    /// compose command. Returns the path of the generated file.
    ///
    /// The profiles of `.ramet.json` are replayed every time; `extra_profiles`
    /// are added for this invocation only (`ramet compose run --profile tools
    /// migrate` must not attach `tools` to the env). Without that replay,
    /// `checkpoint`, `new` and `restore` would regenerate the configuration
    /// without the profiled services, and without their named volumes, which
    /// `docker compose config` drops too.
    pub fn regenerate(&mut self, ctx: &Context, extra_profiles: &[String]) -> Result<PathBuf> {
        let settings = self.settings()?;
        let mut config = self.resolve_with(ctx, &settings, extra_profiles)?;
        let keys = config.published_ports();
        if keys.iter().any(|key| !self.ports.map.contains_key(key)) {
            let before = self.interpolation(&settings);
            {
                // Until env.json records the ports: no other env may pick them.
                let _ports = crate::lock::ports(ctx)?;
                self.extend_ports(ctx, &keys, &config)?;
                self.save(ctx.layout())?;
            }
            // The files were read without the value of a port just given.
            if self.interpolation(&settings) != before {
                config = self.resolve_with(ctx, &settings, extra_profiles)?;
            }
        }
        warn_unpublished_named_ports(ctx, &settings, &keys);

        let layout = ctx.layout();
        let volumes_dir = layout.volumes_dir(&self.project, &self.name);
        // An empty directory is enough: images running under a dedicated uid
        // (postgres) fix the owner themselves on first start.
        for volume in config.declared_volumes().managed {
            create_dir_all(&volumes_dir.join(volume))?;
        }
        let path = layout.compose_file(&self.project, &self.name);
        write_if_changed(
            &path,
            &config.for_env(&volumes_dir, &self.ports.map).to_json(),
        )?;
        Ok(path)
    }

    /// Gives a host port to every published port in `keys` that has none yet.
    ///
    /// Ports already assigned never move. An env keeping the project's ports
    /// keeps the project's value for new ports too. Otherwise new ports take
    /// the free slots of the env's block; when the block is full, the env gets
    /// a new block and every port is reassigned.
    pub fn extend_ports(
        &mut self,
        ctx: &Context,
        keys: &[String],
        config: &ComposeConfig,
    ) -> Result<()> {
        let Some(range) = self.ports.range else {
            for (key, port) in config.original_port_map() {
                self.ports.map.entry(key).or_insert(port);
            }
            return Ok(());
        };
        let missing: Vec<&String> = keys
            .iter()
            .filter(|key| !self.ports.map.contains_key(*key))
            .collect();
        let used: BTreeSet<u16> = self.ports.map.values().copied().collect();
        let free: Vec<u16> = range.ports().filter(|port| !used.contains(port)).collect();
        if missing.len() > free.len() {
            let fresh = store::allocate_ports(
                ctx,
                &self.project,
                &self.name,
                keys.len(),
                Some(&self.name),
            )?;
            self.ports.range = Some(fresh);
            self.ports.map = ports::sequential_map(keys, fresh);
            return Ok(());
        }
        for (key, port) in missing.into_iter().zip(free) {
            self.ports.map.insert(key.clone(), port);
        }
        Ok(())
    }
}

/// What is wrong with each named port of `settings` that is not among the
/// published `keys`: its variable stays unset. A service of a profile left
/// off is one; a typo is another.
pub fn unpublished_named_ports(settings: &Settings, keys: &[String]) -> Vec<String> {
    settings
        .named_ports()
        .filter(|(_, key)| !keys.contains(key))
        .map(|(name, key)| {
            format!(
                "ports.{name}: {key} is not published, {} is not set",
                port_variable(name)
            )
        })
        .collect()
}

/// Warns about [`unpublished_named_ports`] on standard error, which keeps
/// the output of a compose command clean.
fn warn_unpublished_named_ports(ctx: &Context, settings: &Settings, keys: &[String]) {
    let ui = ctx.ui();
    for problem in unpublished_named_ports(settings, keys) {
        ui.err(format!("{} {problem}", ui.style().yellow("!")));
    }
}

/// Writes the generated configuration, unless the file already holds exactly
/// that content: the cache then keeps its modification time.
fn write_if_changed(path: &Path, contents: &str) -> Result<()> {
    if fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    write_atomic(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> Env {
        serde_json::from_str(
            r#"{"name": "main", "project": "demo", "parent": null, "worktree": "/code/app"}"#,
        )
        .unwrap()
    }

    #[test]
    fn older_files_without_ports_read_as_empty() {
        assert_eq!(minimal().ports, Ports::default());
    }

    #[test]
    fn unknown_keys_survive_a_round_trip() {
        let mut value = serde_json::to_value(minimal()).unwrap();
        value["future_key"] = serde_json::json!({"kept": true});
        value["ports"]["future_port_key"] = serde_json::json!(1);
        let env: Env = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&env).unwrap(), value);
    }

    #[test]
    fn reads_a_file_written_by_earlier_versions() {
        let env: Env = serde_json::from_str(
            r#"{
              "branch_at_creation": "feat-a",
              "carried_files": [".env"],
              "checkpoints": {"c1": {"created_at": "2026-01-01T00:00:00+00:00", "head": "abc", "message": null}},
              "compose_files": [],
              "created_at": "2026-01-01T00:00:00+00:00",
              "name": "feat-a",
              "parent": "main",
              "ports": {"map": {"db:5432": 50813, "web:80": 50812}, "range": [50812, 50817]},
              "profiles": ["debug"],
              "project": "example",
              "worktree": "/code/example.wt/feat-a"
            }"#,
        )
        .unwrap();
        assert_eq!(
            env.ports.range,
            Some(PortRange {
                first: 50_812,
                last: 50_817
            })
        );
        assert_eq!(env.ports.map["web:80"], 50_812);
        assert_eq!(env.checkpoints["c1"].head.as_deref(), Some("abc"));
        assert!(!env.is_primary());
    }

    #[test]
    fn writes_null_for_the_parent_and_range_of_the_main_env() {
        let value = serde_json::to_value(minimal()).unwrap();
        assert!(value["parent"].is_null());
        assert!(value["ports"]["range"].is_null());
    }

    #[test]
    fn compose_project_joins_project_and_env() {
        let mut env = minimal();
        env.name = "feat-a".into();
        env.project = "shop".into();
        assert_eq!(env.compose_project(), "shop-feat-a");
    }
}
