//! Ports already assigned never move; new ones fit in the env's block, and no
//! two envs of the machine share a block, whatever their project.

use ramet::env::Env;
use ramet::ports::PortRange;
use serde_json::{Value, json};

use crate::support::{Fixture, port};

fn config(ports: &[(&str, u16, u16)]) -> Value {
    let services: serde_json::Map<String, Value> = ports
        .iter()
        .map(|&(service, target, published)| {
            (
                service.to_owned(),
                json!({"ports": [port(target, published)]}),
            )
        })
        .collect();
    json!({ "services": services })
}

fn extend(fx: &Fixture, env: &mut Env, ports: &[(&str, u16, u16)]) {
    let config = ramet::compose::ComposeConfig::from_json(&config(ports).to_string()).unwrap();
    let keys = config.published_ports();
    env.extend_ports(&fx.ctx(), &keys, &config).unwrap();
}

fn map(env: &Env) -> Vec<(String, u16)> {
    env.ports.map.iter().map(|(k, &v)| (k.clone(), v)).collect()
}

#[test]
fn the_main_env_keeps_the_project_ports() {
    let fx = Fixture::new();
    let mut env = fx.env("main", |_| {});
    extend(&fx, &mut env, &[("web", 80, 8080), ("db", 5432, 5432)]);
    assert_eq!(
        map(&env),
        [("db:5432".into(), 5432), ("web:80".into(), 8080)]
    );
}

#[test]
fn a_new_port_of_the_main_env_keeps_its_value_too() {
    let fx = Fixture::new();
    let mut env = fx.env("main", |env| {
        env.ports.map.insert("web:80".into(), 8080);
    });
    extend(&fx, &mut env, &[("web", 80, 8080), ("cache", 6379, 6379)]);
    assert_eq!(env.ports.map["cache:6379"], 6379);
}

#[test]
fn a_new_port_takes_a_free_slot_of_the_block() {
    let fx = Fixture::new();
    let mut env = fx.env("feat-a", |env| {
        env.ports.range = Some(PortRange {
            first: 30_000,
            last: 30_006,
        });
        env.ports.map.insert("web:80".into(), 30_000);
    });
    extend(&fx, &mut env, &[("web", 80, 8080), ("db", 5432, 5432)]);
    assert_eq!(
        env.ports.map["web:80"], 30_000,
        "an assigned port does not move"
    );
    assert!((30_000..=30_006).contains(&env.ports.map["db:5432"]));
}

#[test]
fn a_full_block_is_replaced_and_every_port_reassigned() {
    let fx = Fixture::new();
    let mut env = fx.env("feat-a", |env| {
        env.ports.range = Some(PortRange {
            first: 30_000,
            last: 30_001,
        });
        env.ports.map.insert("web:80".into(), 30_000);
        env.ports.map.insert("db:5432".into(), 30_001);
    });
    extend(
        &fx,
        &mut env,
        &[("web", 80, 8080), ("db", 5432, 5432), ("cache", 6379, 6379)],
    );
    let range = env.ports.range.unwrap();
    assert_ne!(
        range,
        PortRange {
            first: 30_000,
            last: 30_001
        }
    );
    assert_eq!(env.ports.map.len(), 3);
    assert!(
        env.ports
            .map
            .values()
            .all(|port| range.ports().contains(port))
    );
}

#[test]
fn host_ports_are_never_duplicated_within_an_env() {
    let fx = Fixture::new();
    let mut env = fx.env("feat-a", |env| {
        env.ports.range = Some(PortRange {
            first: 30_000,
            last: 30_006,
        });
    });
    extend(
        &fx,
        &mut env,
        &[("web", 80, 8080), ("db", 5432, 5432), ("cache", 6379, 6379)],
    );
    let distinct: std::collections::BTreeSet<u16> = env.ports.map.values().copied().collect();
    assert_eq!(distinct.len(), 3);
}

#[test]
fn a_new_block_avoids_the_blocks_of_other_envs() {
    let fx = Fixture::new();
    let natural = ramet::ports::allocate_range("feat-b", 2, &[], |_| true).unwrap();
    fx.save_env(fx.env("other", |env| env.ports.range = Some(natural)));
    let allocated =
        ramet::env::store::allocate_ports(&fx.ctx(), "demo", "feat-b", 2, None).unwrap();
    assert!(!allocated.overlaps(natural));
    let own =
        ramet::env::store::allocate_ports(&fx.ctx(), "demo", "feat-b", 2, Some("other")).unwrap();
    assert_eq!(
        own, natural,
        "an env ignores the block it is allowed to replace"
    );
}

#[test]
fn a_new_block_avoids_the_blocks_of_other_projects() {
    // Same env name, hence same natural block, in a stopped stack of another
    // project: only the recorded block can tell the two apart.
    let fx = Fixture::new();
    let natural = ramet::ports::allocate_range("feat-b", 2, &[], |_| true).unwrap();
    fx.save_env(fx.env("feat-b", |env| {
        env.project = "blog".into();
        env.ports.range = Some(natural);
    }));
    let allocated =
        ramet::env::store::allocate_ports(&fx.ctx(), "demo", "feat-b", 2, None).unwrap();
    assert!(
        !allocated.overlaps(natural),
        "{allocated} overlaps blog/feat-b {natural}"
    );
}

#[test]
fn the_replaceable_block_is_only_the_one_of_the_same_project() {
    let fx = Fixture::new();
    let natural = ramet::ports::allocate_range("feat-b", 2, &[], |_| true).unwrap();
    fx.save_env(fx.env("feat-b", |env| {
        env.project = "blog".into();
        env.ports.range = Some(natural);
    }));
    let allocated =
        ramet::env::store::allocate_ports(&fx.ctx(), "demo", "feat-b", 2, Some("feat-b")).unwrap();
    assert!(
        !allocated.overlaps(natural),
        "demo/feat-b may replace its own block, not the one of blog/feat-b"
    );
}
