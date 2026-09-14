//! Local files synced between envs, ports rewritten: at `ramet new`, and on
//! demand with `ramet sync`.
//!
//! A `.env` is ignored by git: `git worktree add` does not copy it, and the
//! new env's application would not start. Copying it verbatim would be worse:
//! its ports would point at the source env, and the application would write
//! into its neighbour's database without a word.

use std::assert_matches;
use std::fs;
use std::path::PathBuf;

use ramet::commands::{Outcome, sync};
use ramet::env::Env;
use ramet::error::{Error, Result};
use serde_json::json;

use crate::new::create;
use crate::support::{Fixture, git, port, write_file, write_settings};

/// A main env publishing web:80 on 8080, with a `.env` pointing at it.
fn fixture() -> Fixture {
    let fx = Fixture::with_main_env();
    fs::write(
        fx.clone.join(".env"),
        "DATABASE_URL=postgres://u:p@localhost:8080/app\nOTHER=value\n",
    )
    .unwrap();
    let mut main = fx.load("main");
    main.ports.map.insert("web:80".into(), 8080);
    fx.save_env(main);
    fx.runner
        .set_config(json!({"services": {"web": {"ports": [port(80, 8080)]}}}));
    fx
}

fn target(fx: &Fixture, relative: &str) -> PathBuf {
    fx.base.join("app.wt/feat-a").join(relative)
}

fn new_port(fx: &Fixture) -> u16 {
    fx.load("feat-a").ports.map["web:80"]
}

// ------------------------------------------------------------ at `ramet new`

#[test]
fn the_env_file_is_copied_with_its_ports_rewritten() {
    let fx = fixture();
    create(&fx, "feat-a", |_| {}).unwrap();
    let content = fs::read_to_string(target(&fx, ".env")).unwrap();
    assert!(
        content.contains(&format!("localhost:{}/app", new_port(&fx))),
        "{content}"
    );
    assert!(!content.contains("localhost:8080"));
    assert!(
        content.contains("OTHER=value"),
        "the rest of the file is intact"
    );
    assert!(fx.stdout().contains("`.env` copied, 1 port(s) rewritten"));
}

#[test]
fn every_env_file_git_does_not_track_is_synced_by_default() {
    let fx = fixture();
    fs::write(
        fx.clone.join(".gitignore"),
        "node_modules/\n.env*\n!.env.example\n",
    )
    .unwrap();
    write_file(&fx.clone, ".env.example", "tracked\n");
    git(&fx.clone, &["add", ".gitignore", ".env.example"]);
    git(&fx.clone, &["commit", "-qm", "ignore"]);
    for relative in [
        ".env.local",
        "apps/api/.env",
        "docker/compose/.env",
        "node_modules/pkg/.env",
    ] {
        write_file(&fx.clone, relative, "X=1\n");
    }
    fs::write(fx.clone.join(".env.example"), "local change\n").unwrap();
    create(&fx, "feat-a", |_| {}).unwrap();

    for relative in [".env", ".env.local", "apps/api/.env", "docker/compose/.env"] {
        assert!(target(&fx, relative).is_file(), "{relative}");
    }
    assert!(
        !target(&fx, "node_modules").exists(),
        "a dependency's files are not the project's"
    );
    assert_eq!(
        fs::read_to_string(target(&fx, ".env.example")).unwrap(),
        "tracked\n",
        "a tracked file comes from git only"
    );
}

#[test]
fn an_explicit_sync_replaces_the_default() {
    let fx = fixture();
    write_file(
        &fx.clone,
        "config/local.toml",
        "url = \"http://localhost:8080\"\n",
    );
    write_settings(&fx.clone, &json!({"sync": ["config/local.toml"]}));
    create(&fx, "feat-a", |_| {}).unwrap();
    assert!(!target(&fx, ".env").exists());
    assert!(
        fs::read_to_string(target(&fx, "config/local.toml"))
            .unwrap()
            .contains(&format!("localhost:{}", new_port(&fx)))
    );
    assert!(
        target(&fx, ".ramet.json").is_file(),
        "an untracked .ramet.json always comes along"
    );
}

#[test]
fn an_empty_sync_copies_nothing_else() {
    let fx = fixture();
    write_settings(&fx.clone, &json!({"sync": []}));
    create(&fx, "feat-a", |_| {}).unwrap();
    assert!(!target(&fx, ".env").exists());
    assert!(target(&fx, ".ramet.json").is_file());
}

#[test]
fn a_whole_directory_is_synced() {
    let fx = fixture();
    write_settings(&fx.clone, &json!({"sync": ["config"]}));
    write_file(
        &fx.clone,
        "config/a.toml",
        "url = \"http://localhost:8080/a\"\n",
    );
    write_file(
        &fx.clone,
        "config/deep/b.toml",
        "url = \"http://localhost:8080/b\"\n",
    );
    fs::write(fx.clone.join("config/cert.pem"), b"\x00binary\xff").unwrap();
    create(&fx, "feat-a", |_| {}).unwrap();

    let port = new_port(&fx);
    let copied = target(&fx, "config");
    assert!(
        fs::read_to_string(copied.join("a.toml"))
            .unwrap()
            .contains(&format!("localhost:{port}/a"))
    );
    assert!(
        fs::read_to_string(copied.join("deep/b.toml"))
            .unwrap()
            .contains(&format!("localhost:{port}/b")),
        "the copy recurses"
    );
    assert_eq!(
        fs::read(copied.join("cert.pem")).unwrap(),
        b"\x00binary\xff",
        "binaries are copied verbatim"
    );
    let out = fx.stdout();
    assert!(
        out.contains("`config/cert.pem` copied (binary, ports not rewritten)"),
        "{out}"
    );
}

#[test]
fn an_ignored_directory_is_searched_when_a_pattern_names_it() {
    let fx = fixture();
    fs::write(fx.clone.join(".gitignore"), "certs/\nvendor/\n").unwrap();
    git(&fx.clone, &["add", ".gitignore"]);
    git(&fx.clone, &["commit", "-qm", "ignore"]);
    write_file(&fx.clone, "certs/sub/b.pem", "b\n");
    write_file(&fx.clone, "vendor/x/k.pem", "k\n");
    write_file(&fx.clone, "vendor/x/other.txt", "o\n");
    write_settings(&fx.clone, &json!({"sync": ["certs", "vendor/**/*.pem"]}));
    create(&fx, "feat-a", |_| {}).unwrap();
    assert!(target(&fx, "certs/sub/b.pem").is_file());
    assert!(target(&fx, "vendor/x/k.pem").is_file());
    assert!(!target(&fx, "vendor/x/other.txt").exists());
}

#[test]
fn a_file_tracked_by_git_is_never_synced() {
    // Its local changes belong to git: `git worktree add` brings the
    // committed version, and ramet writes over nothing git tracks.
    let fx = fixture();
    write_settings(&fx.clone, &json!({"sync": ["config"]}));
    write_file(&fx.clone, "config/a.toml", "tracked\n");
    git(&fx.clone, &["add", "config/a.toml"]);
    git(&fx.clone, &["commit", "-qm", "track a.toml"]);
    fs::write(fx.clone.join("config/a.toml"), "local change\n").unwrap();
    fs::write(fx.clone.join("config/b.toml"), "source\n").unwrap();

    create(&fx, "feat-a", |_| {}).unwrap();
    assert_eq!(
        fs::read_to_string(target(&fx, "config/a.toml")).unwrap(),
        "tracked\n"
    );
    assert_eq!(
        fs::read_to_string(target(&fx, "config/b.toml")).unwrap(),
        "source\n"
    );
}

#[test]
fn a_file_the_branch_already_has_is_left_and_reported() {
    // Untracked in the source, committed in the branch the worktree checks out.
    let fx = fixture();
    git(&fx.clone, &["checkout", "-q", "-b", "other"]);
    fs::write(fx.clone.join(".env"), "COMMITTED=1\n").unwrap();
    git(&fx.clone, &["add", "-f", ".env"]);
    git(&fx.clone, &["commit", "-qm", "commit .env"]);
    git(&fx.clone, &["checkout", "-q", "main"]);
    fs::write(fx.clone.join(".env"), "LOCAL=1\n").unwrap();

    create(&fx, "feat-a", |args| args.branch = Some("other".into())).unwrap();
    assert_eq!(
        fs::read_to_string(target(&fx, ".env")).unwrap(),
        "COMMITTED=1\n"
    );
    assert!(
        fx.stdout()
            .contains("`.env` already exists in the new worktree: left as it is")
    );
}

#[test]
fn a_dotenv_setting_compose_file_is_there_before_compose_resolves() {
    // Compose reads `COMPOSE_FILE` from the `.env` of the worktree: without
    // it, the new worktree would have no compose file compose could find.
    let fx = fixture();
    git(&fx.clone, &["mv", "compose.yml", "docker-app.yml"]);
    git(&fx.clone, &["commit", "-qm", "move"]);
    fs::write(fx.clone.join(".env"), "COMPOSE_FILE=docker-app.yml\n").unwrap();
    assert_eq!(create(&fx, "feat-a", |_| {}).unwrap(), Outcome::Done);
}

#[test]
fn a_pattern_outside_the_worktree_is_refused_before_anything_is_created() {
    let fx = fixture();
    write_settings(&fx.clone, &json!({"sync": ["../elsewhere"]}));
    let err = create(&fx, "feat-a", |_| {}).unwrap_err();
    assert_matches!(err, Error::InvalidSettings { .. });
    assert_eq!(fx.env_names(), ["main"]);
}

#[test]
fn a_tracked_symlink_cannot_smuggle_a_file_out_of_the_worktree() {
    let fx = fixture();
    write_settings(&fx.clone, &json!({"sync": ["config"]}));
    let outside = fx.base.join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, fx.clone.join("config")).unwrap();
    git(&fx.clone, &["add", "config"]);
    git(&fx.clone, &["commit", "-qm", "link"]);
    git(&fx.clone, &["rm", "-q", "--cached", "config"]);
    // The source has a real directory to sync; the new worktree gets the link.
    fs::remove_file(fx.clone.join("config")).unwrap();
    write_file(&fx.clone, "config/x", "data");
    let err = create(&fx, "feat-a", |_| {}).unwrap_err();
    assert_matches!(err, Error::SyncedPathOutsideWorktree { .. });
    assert!(!outside.join("x").exists(), "nothing was written outside");
}

#[test]
fn a_branch_cannot_redirect_the_rewrite_of_its_env_file_through_a_link() {
    let fx = fixture();
    let victim = fx.base.join("bashrc");
    fs::write(&victim, "precious\n").unwrap();
    // Where ramet used to write before renaming over `.env`.
    for name in [".env.ramet-tmp", ".env.tmp"] {
        std::os::unix::fs::symlink(&victim, fx.clone.join(name)).unwrap();
    }
    git(&fx.clone, &["add", "-f", ".env.ramet-tmp", ".env.tmp"]);
    git(&fx.clone, &["commit", "-qm", "trap"]);

    create(&fx, "feat-a", |_| {}).unwrap();
    assert_eq!(fs::read_to_string(&victim).unwrap(), "precious\n");
    let content = fs::read_to_string(target(&fx, ".env")).unwrap();
    assert!(
        content.contains(&format!("localhost:{}/app", new_port(&fx))),
        "{content}"
    );
}

#[test]
fn a_link_in_the_source_worktree_is_never_followed() {
    let fx = fixture();
    let secret = fx.base.join("id_ed25519");
    fs::write(&secret, "private key\n").unwrap();
    // A container mounting the worktree can leave such a link behind.
    std::os::unix::fs::symlink(&secret, fx.clone.join(".env.local")).unwrap();
    create(&fx, "feat-a", |_| {}).unwrap();
    assert!(
        !target(&fx, ".env.local").exists(),
        "the key was not copied"
    );
    assert!(target(&fx, ".env").exists(), "regular files still are");
}

// ------------------------------------------------------------- `ramet sync`

/// `fixture()` plus the env feat-a, cloned from main, publishing web:80 on 30000.
fn with_feat_a() -> (Fixture, Env) {
    let fx = fixture();
    let mut feat = fx.secondary_env("feat-a");
    feat.ports.map.insert("web:80".into(), 30_000);
    let feat = fx.save_env(feat);
    (fx, feat)
}

fn run_sync(fx: &Fixture, env: &Env, customize: impl FnOnce(&mut sync::Args)) -> Result<Outcome> {
    let mut args = sync::Args::default();
    customize(&mut args);
    sync::run(&fx.ctx_at(&env.worktree), &args)
}

#[test]
fn sync_copies_a_missing_file_with_its_ports_rewritten() {
    let (fx, feat) = with_feat_a();
    assert_eq!(run_sync(&fx, &feat, |_| {}).unwrap(), Outcome::Done);
    let content = fs::read_to_string(target(&fx, ".env")).unwrap();
    assert!(content.contains("localhost:30000/app"), "{content}");
    let out = fx.stdout();
    assert!(out.contains("sync feat-a from \"main\""), "{out}");
    assert!(out.contains("`ramet compose up -d`"), "{out}");
}

#[test]
fn sync_never_follows_a_link_in_the_source_worktree() {
    let (fx, feat) = with_feat_a();
    let secret = fx.base.join("id_ed25519");
    fs::write(&secret, "private key\n").unwrap();
    fs::create_dir_all(fx.clone.join("apps")).unwrap();
    std::os::unix::fs::symlink(&secret, fx.clone.join("apps/.env")).unwrap();
    run_sync(&fx, &feat, |_| {}).unwrap();
    assert!(!target(&fx, "apps/.env").exists(), "the key was not copied");
    assert!(target(&fx, ".env").exists(), "regular files still are");
}

#[test]
fn sync_leaves_an_identical_file_untouched() {
    let (fx, feat) = with_feat_a();
    run_sync(&fx, &feat, |_| {}).unwrap();
    let modified = fs::metadata(target(&fx, ".env"))
        .unwrap()
        .modified()
        .unwrap();
    run_sync(&fx, &feat, |_| {}).unwrap();
    assert_eq!(
        fs::metadata(target(&fx, ".env"))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    assert!(fx.stdout().contains("1 file(s) already up to date"));
}

#[test]
fn sync_asks_before_replacing_a_file_that_differs() {
    let (fx, feat) = with_feat_a();
    fs::write(target(&fx, ".env"), "EDITED=1\n").unwrap();
    write_file(&fx.clone, "apps/.env", "NEW=1\n");
    let err = run_sync(&fx, &feat, |_| {}).unwrap_err();
    assert_matches!(err, Error::ConfirmationRequired { .. });
    assert_eq!(
        fs::read_to_string(target(&fx, ".env")).unwrap(),
        "EDITED=1\n"
    );
    assert!(
        !target(&fx, "apps/.env").exists(),
        "refused before anything is written"
    );
}

#[test]
fn sync_yes_replaces_and_keeps_the_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (fx, feat) = with_feat_a();
    fs::write(target(&fx, ".env"), "EDITED=1\n").unwrap();
    fs::set_permissions(target(&fx, ".env"), fs::Permissions::from_mode(0o600)).unwrap();
    run_sync(&fx, &feat, |args| args.yes = true).unwrap();
    let content = fs::read_to_string(target(&fx, ".env")).unwrap();
    assert!(content.contains("localhost:30000"), "{content}");
    let mode = fs::metadata(target(&fx, ".env"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(fx.stdout().contains("`.env` replaced, 1 port(s) rewritten"));
}

#[test]
fn sync_declined_keeps_the_file_and_copies_the_missing_ones() {
    let (fx, feat) = with_feat_a();
    fs::write(target(&fx, ".env"), "EDITED=1\n").unwrap();
    write_file(&fx.clone, "apps/.env", "NEW=1\n");
    fx.answer("n\n");
    assert_eq!(run_sync(&fx, &feat, |_| {}).unwrap(), Outcome::Done);
    assert_eq!(
        fs::read_to_string(target(&fx, ".env")).unwrap(),
        "EDITED=1\n"
    );
    assert!(target(&fx, "apps/.env").is_file());
    let out = fx.stdout();
    assert!(out.contains("differs from \"main\":\n    .env"), "{out}");
    assert!(out.contains("1 file(s) left as they are"), "{out}");
}

#[test]
fn sync_declined_with_nothing_else_to_do_is_aborted() {
    let (fx, feat) = with_feat_a();
    fs::write(target(&fx, ".env"), "EDITED=1\n").unwrap();
    fx.answer("n\n");
    assert_eq!(run_sync(&fx, &feat, |_| {}).unwrap(), Outcome::Aborted);
}

#[test]
fn sync_from_takes_another_env_and_its_ports() {
    let (fx, feat) = with_feat_a();
    let mut feat_b = fx.secondary_env("feat-b");
    feat_b.ports.map.insert("web:80".into(), 40_000);
    let feat_b = fx.save_env(feat_b);
    fs::write(
        feat_b.worktree.join(".env"),
        "URL=http://localhost:40000/\n",
    )
    .unwrap();
    run_sync(&fx, &feat, |args| args.from = Some("feat-b".into())).unwrap();
    assert_eq!(
        fs::read_to_string(target(&fx, ".env")).unwrap(),
        "URL=http://localhost:30000/\n"
    );
}

#[test]
fn sync_never_writes_a_file_git_tracks_in_the_current_worktree() {
    let (fx, feat) = with_feat_a();
    fs::write(target(&fx, ".env"), "COMMITTED=1\n").unwrap();
    git(&feat.worktree, &["add", "-f", ".env"]);
    git(&feat.worktree, &["commit", "-qm", "commit .env"]);
    run_sync(&fx, &feat, |args| args.yes = true).unwrap();
    assert_eq!(
        fs::read_to_string(target(&fx, ".env")).unwrap(),
        "COMMITTED=1\n"
    );
    assert!(
        fx.stdout()
            .contains("`.env` is tracked by git in this worktree: left alone")
    );
}

#[test]
fn sync_never_writes_through_a_symbolic_link() {
    let (fx, feat) = with_feat_a();
    let elsewhere = fx.base.join("elsewhere.env");
    fs::write(&elsewhere, "OUTSIDE=1\n").unwrap();
    std::os::unix::fs::symlink(&elsewhere, target(&fx, ".env")).unwrap();
    run_sync(&fx, &feat, |args| args.yes = true).unwrap();
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "OUTSIDE=1\n");
}

#[test]
fn sync_on_the_main_env_needs_a_source() {
    let (fx, _) = with_feat_a();
    let main = fx.load("main");
    let err = run_sync(&fx, &main, |_| {}).unwrap_err();
    assert_matches!(err, Error::NoSyncSource { .. });
    assert!(
        err.hint().unwrap().contains("`ramet sync --from feat-a`"),
        "{:?}",
        err.hint()
    );
}

#[test]
fn sync_refuses_itself_and_unknown_envs() {
    let (fx, feat) = with_feat_a();
    let err = run_sync(&fx, &feat, |args| args.from = Some("feat-a".into())).unwrap_err();
    assert_matches!(err, Error::SyncFromItself { .. });
    let err = run_sync(&fx, &feat, |args| args.from = Some("ghost".into())).unwrap_err();
    assert_matches!(err, Error::UnknownEnvironment { .. });
}

#[test]
fn sync_says_when_there_is_nothing_to_sync() {
    let (fx, feat) = with_feat_a();
    fs::remove_file(fx.clone.join(".env")).unwrap();
    assert_eq!(run_sync(&fx, &feat, |_| {}).unwrap(), Outcome::Done);
    assert!(fx.stdout().contains("nothing to sync from \"main\""));
}
