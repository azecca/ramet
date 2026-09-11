//! Finding the docker volumes an existing project keeps its data in.
//!
//! Docker volumes carry the compose project name as a prefix, and that prefix
//! depends on how compose was started: a Makefile running
//! `-f docker/compose/base.yml` makes `docker/compose` the project directory,
//! hence `compose_pgdata` instead of `myproject_pgdata`. ramet resolves the
//! project the way the declared files say, so it expects the same names; but
//! data created with another command sleeps under another prefix, and the
//! choice cannot be automatic: on a developer's machine, six volumes can end
//! in `_postgres_data`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::docker::Docker;
use crate::util::size::human_bytes;

/// Most prefixes named in the note about unrelated volumes.
const NOTE_PREFIXES: usize = 3;

/// The name compose derives from `project_dir` when nothing names the project:
/// the directory's name, lowercased, restricted to alphanumerics, `_` and `-`.
///
/// A resolved name that differs was chosen on purpose (a top-level `name:`,
/// `COMPOSE_PROJECT_NAME`); one that equals it only says where the files are.
pub(crate) fn implicit_project_name(project_dir: &Path) -> String {
    let name: String = project_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
        .chars()
        .filter(|&c| c.is_alphanumeric() || c == '_' || c == '-')
        .collect();
    name.trim_start_matches(['_', '-']).to_owned()
}

/// The prefix of a docker volume named `<prefix>_<volume>`.
fn prefix_of<'n>(docker_name: &'n str, volume: &str) -> &'n str {
    let suffix_len = volume.len() + 1;
    &docker_name[..docker_name.len().saturating_sub(suffix_len)]
}

/// Prefixes of `candidates` that hold *every* volume of the project, among the
/// `existing` docker volumes.
///
/// Many unrelated projects share a suffix; only proposing the prefixes that
/// cover the whole set makes the choice decidable. The whole set is every
/// managed volume, not only the missing ones: the chosen prefix becomes the
/// source of all of them, and a prefix lacking one would have it copied from
/// nothing while its real data is left behind.
pub(crate) fn complete_prefixes(
    candidates: &BTreeMap<String, Vec<String>>,
    volumes: &BTreeSet<String>,
    existing: &BTreeSet<&str>,
) -> Vec<String> {
    let prefixes: BTreeSet<&str> = candidates
        .iter()
        .flat_map(|(volume, names)| names.iter().map(move |name| prefix_of(name, volume)))
        .collect();
    prefixes
        .into_iter()
        .filter(|prefix| {
            volumes
                .iter()
                .all(|volume| existing.contains(format!("{prefix}_{volume}").as_str()))
        })
        .map(str::to_owned)
        .collect()
}

/// Sizes of docker volumes, each measured once: measuring starts a container.
pub(crate) struct VolumeSizes<'a> {
    docker: Docker<'a>,
    known: RefCell<BTreeMap<String, u64>>,
}

impl<'a> VolumeSizes<'a> {
    pub(crate) fn new(docker: Docker<'a>) -> Self {
        Self {
            docker,
            known: RefCell::default(),
        }
    }

    /// Bytes occupied by the docker volume `name`; 0 when it cannot be measured.
    pub(crate) fn of(&self, name: &str) -> u64 {
        if let Some(&size) = self.known.borrow().get(name) {
            return size;
        }
        let size = self.docker.volume_size(name);
        self.known.borrow_mut().insert(name.to_owned(), size);
        size
    }
}

/// What migrating from `prefix` would take: which volume, from where, how much.
///
/// That is the decision the user has to read, rather than an inventory of
/// candidates where the right volume and an unrelated 100 GB one look alike.
pub(crate) fn migration_plan(
    prefix: &str,
    volumes: &BTreeSet<String>,
    sizes: &VolumeSizes<'_>,
) -> String {
    let volume_width = volumes
        .iter()
        .map(|volume| volume.chars().count())
        .max()
        .unwrap_or(0);
    let source_width = volume_width + prefix.chars().count() + 1;
    let rows: Vec<(String, u64)> = volumes
        .iter()
        .map(|volume| {
            let source = format!("{prefix}_{volume}");
            let size = sizes.of(&source);
            (
                format!("{volume:<volume_width$}  ←  {source:<source_width$}"),
                size,
            )
        })
        .collect();
    let width = rows
        .iter()
        .map(|(column, _)| column.chars().count())
        .max()
        .unwrap_or(0);
    let total: u64 = rows.iter().map(|&(_, size)| size).sum();
    let mut lines = vec![format!(
        "  prefix \"{prefix}\" holds the {} volume(s) of the project:",
        volumes.len()
    )];
    for (column, size) in &rows {
        lines.push(format!(
            "      {column:<width$}  {:>10}",
            human_bytes(Some(*size))
        ));
    }
    lines.push(format!(
        "      {:>width$}  {:>10}",
        "total",
        human_bytes(Some(total))
    ));
    lines.join("\n")
}

/// One note about the volumes sharing a suffix that will not be touched.
pub(crate) fn other_candidates_note(
    candidates: &BTreeMap<String, Vec<String>>,
    retained: &[String],
) -> Option<String> {
    let retained: Vec<String> = retained.iter().map(|prefix| format!("{prefix}_")).collect();
    // The prefix is what precedes `_<volume>`, not the last underscore:
    // `shop-keycloak_postgres_data` has the prefix `shop-keycloak`.
    let others: BTreeMap<&str, &str> = candidates
        .iter()
        .flat_map(|(volume, names)| {
            names
                .iter()
                .map(move |name| (name.as_str(), prefix_of(name, volume)))
        })
        .filter(|(name, _)| {
            !retained
                .iter()
                .any(|prefix| name.starts_with(prefix.as_str()))
        })
        .collect();
    if others.is_empty() {
        return None;
    }
    let prefixes: BTreeSet<&str> = others.values().copied().collect();
    let shown: Vec<&str> = prefixes.into_iter().take(NOTE_PREFIXES).collect();
    Some(format!(
        "  {} other volume(s) share a suffix without covering the project\n  ({}…): they will not be touched.",
        others.len(),
        shown.join(", ")
    ))
}

/// Why no prefix could be chosen, and the options that settle it.
///
/// `candidates` are the missing volumes and the docker volumes sharing their
/// suffix; `sources` names every managed volume under the expected prefix.
pub(crate) fn choice_required(
    candidates: &BTreeMap<String, Vec<String>>,
    sources: &BTreeMap<String, String>,
    complete: &[String],
    sizes: &VolumeSizes<'_>,
) -> crate::error::Error {
    let expected: Vec<&str> = candidates
        .keys()
        .filter_map(|volume| sources.get(volume).map(String::as_str))
        .collect();
    let summary = format!(
        "{} declared volume(s) cannot be found under the expected name ({})",
        candidates.len(),
        expected.join(", ")
    );
    // `--migrate-from` takes every volume from the prefix: show them all.
    let volumes: BTreeSet<String> = sources.keys().cloned().collect();
    let mut lines = vec![
        "the prefix is the compose project name, and it changes with the way compose".to_owned(),
        "was started (a `-f docker/compose/base.yml` makes `docker/compose` the project".to_owned(),
        "directory, hence `compose`).".to_owned(),
    ];
    if complete.is_empty() {
        lines.push("No prefix holds every volume: check with `docker volume ls`.".to_owned());
    }
    for prefix in complete {
        lines.push(String::new());
        lines.push(migration_plan(prefix, &volumes, sizes));
        lines.push(format!("    ramet init --migrate-from {prefix} …"));
    }
    if let Some(note) = other_candidates_note(candidates, complete) {
        lines.push(String::new());
        lines.push(note);
    }
    lines.push(String::new());
    lines.push("Or start on empty volumes:".to_owned());
    lines.push("    ramet init --no-migrate …".to_owned());
    crate::error::Error::MigrationChoiceRequired {
        summary,
        options: lines.join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(volume, names)| {
                (
                    (*volume).to_owned(),
                    names.iter().map(|&n| n.to_owned()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn the_implicit_name_is_the_normalized_directory_name() {
        assert_eq!(
            implicit_project_name(Path::new("/code/app/docker/compose")),
            "compose"
        );
        assert_eq!(implicit_project_name(Path::new("/code/app")), "app");
        assert_eq!(implicit_project_name(Path::new("/code/_My.App")), "myapp");
    }

    fn set<'s>(items: &[&'s str]) -> BTreeSet<&'s str> {
        items.iter().copied().collect()
    }

    fn volumes(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|&name| name.to_owned()).collect()
    }

    #[test]
    fn only_prefixes_covering_every_volume_are_complete() {
        let found = candidates(&[
            (
                "postgres_data",
                &["compose_postgres_data", "vortex_postgres_data"],
            ),
            ("minio_data", &["compose_minio_data"]),
        ]);
        let existing = set(&[
            "compose_postgres_data",
            "vortex_postgres_data",
            "compose_minio_data",
        ]);
        assert_eq!(
            complete_prefixes(
                &found,
                &volumes(&["postgres_data", "minio_data"]),
                &existing
            ),
            vec!["compose"]
        );
    }

    #[test]
    fn a_prefix_must_also_hold_the_volumes_that_are_not_missing() {
        // `shop_pgdata` exists, `shop_uploads` does not: `legacy` holds the
        // missing volume only, and taking it would copy `pgdata` from nothing.
        let found = candidates(&[("uploads", &["legacy_uploads"])]);
        let existing = set(&["shop_pgdata", "legacy_uploads"]);
        assert!(complete_prefixes(&found, &volumes(&["pgdata", "uploads"]), &existing).is_empty());
    }

    #[test]
    fn the_note_names_prefixes_not_volumes() {
        let found = candidates(&[(
            "postgres_data",
            &["compose_postgres_data", "shop-keycloak_postgres_data"],
        )]);
        let note = other_candidates_note(&found, &["compose".to_owned()]).unwrap();
        assert!(note.contains("shop-keycloak…"), "{note}");
        assert!(!note.contains("shop-keycloak_postgres"), "{note}");
    }

    #[test]
    fn no_note_when_every_candidate_is_retained() {
        let found = candidates(&[("pgdata", &["compose_pgdata"])]);
        assert_eq!(other_candidates_note(&found, &["compose".to_owned()]), None);
    }
}
