//! Git worktrees, and what git knows about their files.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::process::{Cmd, Runner, RunnerExt};

/// One entry of `git worktree list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    /// Root directory of the worktree.
    pub path: PathBuf,
    /// Commit checked out, when git reports one.
    pub head: Option<String>,
    /// Branch checked out, or `None` for a detached HEAD.
    pub branch: Option<String>,
}

/// Git operations through the `git` command line tool.
pub struct Git<'a> {
    runner: &'a dyn Runner,
}

impl<'a> Git<'a> {
    /// Runs `git` through `runner`.
    pub fn new(runner: &'a dyn Runner) -> Self {
        Self { runner }
    }

    fn git(dir: &Path) -> Cmd {
        Cmd::new("git").current_dir(dir)
    }

    fn query(&self, dir: &Path, args: &[&str]) -> Option<String> {
        // A worktree deleted by hand must read as "unknown", not as an error.
        if !dir.is_dir() {
            return None;
        }
        let output = self.runner.run_unchecked(&Self::git(dir).args(args));
        output.success().then(|| output.stdout_trimmed().to_owned())
    }

    /// Root of the worktree containing `dir`, or `None` outside a repository.
    pub fn current_worktree(&self, dir: &Path) -> Option<PathBuf> {
        self.query(dir, &["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)
    }

    /// Worktrees of the repository containing `dir`. The first one is always
    /// the main clone.
    pub fn worktrees(&self, dir: &Path) -> Vec<Worktree> {
        self.query(dir, &["worktree", "list", "--porcelain"])
            .map(|listing| parse_worktree_list(&listing))
            .unwrap_or_default()
    }

    /// The commit checked out in `worktree`.
    pub fn head_sha(&self, worktree: &Path) -> Option<String> {
        self.query(worktree, &["rev-parse", "HEAD"])
    }

    /// The branch checked out in `worktree`, or `None` when HEAD is detached
    /// or the worktree is gone. A repository without commits is on the branch
    /// its first commit will create.
    pub fn current_branch(&self, worktree: &Path) -> Option<String> {
        self.query(worktree, &["symbolic-ref", "--quiet", "--short", "HEAD"])
    }

    /// Local branches of the repository containing `dir`.
    pub fn local_branches(&self, dir: &Path) -> Vec<String> {
        self.query(
            dir,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )
        .map(|listing| listing.lines().map(str::to_owned).collect())
        .unwrap_or_default()
    }

    /// The repository's default branch, the one its main line of work lives
    /// on: `origin`'s default branch; else the only one of `main` and `master`
    /// that exists; else the only branch there is. `None` when nothing
    /// designates one.
    pub fn default_branch(&self, dir: &Path) -> Option<String> {
        let remote_head = self.query(
            dir,
            &[
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
        );
        if let Some(branch) = remote_head
            .as_deref()
            .and_then(|head| head.strip_prefix("origin/"))
        {
            return Some(branch.to_owned());
        }
        let branches = self.local_branches(dir);
        let conventional: Vec<&str> = branches
            .iter()
            .map(String::as_str)
            .filter(|branch| ["main", "master"].contains(branch))
            .collect();
        match (conventional.as_slice(), branches.as_slice()) {
            ([branch], _) => Some((*branch).to_owned()),
            ([], [branch]) => Some(branch.clone()),
            // Before the first commit there is no branch yet, only the one
            // HEAD names.
            ([], []) => self.current_branch(dir),
            _ => None,
        }
    }

    /// Whether the local branch `branch` exists.
    pub fn branch_exists(&self, dir: &Path, branch: &str) -> bool {
        let reference = format!("refs/heads/{branch}");
        self.query(dir, &["show-ref", "--verify", "--quiet", &reference])
            .is_some()
    }

    /// Whether `revision` names a commit; `HEAD` does not before the first one.
    pub fn has_commit(&self, dir: &Path, revision: &str) -> bool {
        let commit = format!("{revision}^{{commit}}");
        self.query(dir, &["rev-parse", "--verify", "--quiet", &commit])
            .is_some()
    }

    /// Whether the commit `revision` holds the file `path`, relative to the
    /// root of the repository.
    pub fn commit_has_file(&self, dir: &Path, revision: &str, path: &str) -> bool {
        let object = format!("{revision}:{path}");
        self.query(dir, &["cat-file", "-e", &object]).is_some()
    }

    /// The content of `path` in the commit `revision`, or `None` when the
    /// commit does not hold it.
    pub fn file_at(&self, dir: &Path, revision: &str, path: &str) -> Option<String> {
        let object = format!("{revision}:{path}");
        self.query(dir, &["show", &object])
    }

    /// Files of `worktree` git does not track, ignored ones included, that
    /// match one of `pathspecs`; relative to the worktree.
    pub fn untracked_files(&self, worktree: &Path, pathspecs: &[String]) -> Result<Vec<String>> {
        if pathspecs.is_empty() {
            return Ok(Vec::new());
        }
        let cmd = Self::git(worktree)
            .args(["ls-files", "--others", "-z", "--"])
            .args(pathspecs);
        Ok(split_nul(&self.runner.run_checked(&cmd)?.stdout))
    }

    /// Among `paths`, relative to `worktree`, those its ignore rules exclude.
    /// A directory is written with a final `/`, as the rules see it.
    pub fn ignored(&self, worktree: &Path, paths: &[String]) -> Result<BTreeSet<String>> {
        if paths.is_empty() {
            return Ok(BTreeSet::new());
        }
        let input: String = paths.iter().flat_map(|path| [path, "\0"]).collect();
        let cmd = Self::git(worktree)
            .args(["check-ignore", "--stdin", "-z"])
            .input(input);
        let output = self.runner.run_unchecked(&cmd);
        match output.code {
            0 => Ok(split_nul(&output.stdout).into_iter().collect()),
            // Nothing is ignored.
            1 => Ok(BTreeSet::new()),
            _ => Err(Error::CommandFailed {
                command: cmd.to_string(),
                detail: output.failure_detail(),
            }),
        }
    }

    /// Among `paths`, relative to `worktree`, those git tracks there.
    pub fn tracked(&self, worktree: &Path, paths: &[String]) -> Result<BTreeSet<String>> {
        if paths.is_empty() {
            return Ok(BTreeSet::new());
        }
        let cmd = Self::git(worktree)
            .args(["ls-files", "-z", "--"])
            .args(paths.iter().map(|path| format!(":(literal){path}")));
        Ok(split_nul(&self.runner.run_checked(&cmd)?.stdout)
            .into_iter()
            .collect())
    }

    /// Adds a worktree at `path` on `branch`, creating the branch if needed.
    pub fn add_worktree(&self, repository: &Path, path: &Path, branch: &str) -> Result<()> {
        let mut cmd = Self::git(repository).args(["worktree", "add"]).arg(path);
        cmd = if self.branch_exists(repository, branch) {
            cmd.arg(branch)
        } else {
            cmd.args(["-b", branch])
        };
        self.runner.run_checked(&cmd).map(drop)
    }

    /// Removes the worktree at `path`, even with local changes.
    pub fn remove_worktree(&self, main_clone: &Path, path: &Path) -> Result<()> {
        let cmd = Self::git(main_clone)
            .args(["worktree", "remove", "--force"])
            .arg(path);
        self.runner.run_checked(&cmd).map(drop)
    }

    /// Forgets worktrees whose directory has disappeared.
    pub fn prune_worktrees(&self, main_clone: &Path) {
        self.runner
            .run_unchecked(&Self::git(main_clone).args(["worktree", "prune"]));
    }
}

/// The entries of a `-z` listing.
fn split_nul(listing: &str) -> Vec<String> {
    listing
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Parses the output of `git worktree list --porcelain`.
fn parse_worktree_list(listing: &str) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;
    for line in listing.lines().chain(std::iter::once("")) {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "" => worktrees.extend(current.take()),
            "worktree" => {
                worktrees.extend(current.take());
                current = Some(Worktree {
                    path: PathBuf::from(value),
                    head: None,
                    branch: None,
                });
            }
            "HEAD" if let Some(worktree) = current.as_mut() => {
                worktree.head = Some(value.to_owned());
            }
            "branch" if let Some(worktree) = current.as_mut() => {
                let branch = value.strip_prefix("refs/heads/").unwrap_or(value);
                worktree.branch = Some(branch.to_owned());
            }
            // `detached`, `locked`, `prunable`… and lines before any `worktree`.
            _ => {}
        }
    }
    worktrees
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_porcelain_listing() {
        let listing = "worktree /code/app\nHEAD 1111\nbranch refs/heads/main\n\n\
                       worktree /code/app.wt/feat-a\nHEAD 2222\ndetached\n";
        assert_eq!(
            parse_worktree_list(listing),
            vec![
                Worktree {
                    path: PathBuf::from("/code/app"),
                    head: Some("1111".into()),
                    branch: Some("main".into()),
                },
                Worktree {
                    path: PathBuf::from("/code/app.wt/feat-a"),
                    head: Some("2222".into()),
                    branch: None,
                },
            ]
        );
    }

    #[test]
    fn parses_an_empty_listing() {
        assert!(parse_worktree_list("").is_empty());
    }
}
