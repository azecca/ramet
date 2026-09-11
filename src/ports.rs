//! Host ports published by each env.
//!
//! Ports are stable rather than random: an agent that wrote down a URL must
//! find it again after a restart. Each env gets a block of consecutive ports,
//! whose position derives from a hash of the env name.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// First port ramet may allocate.
pub const PORT_MIN: u16 = 20_000;
/// Last port ramet may allocate.
pub const PORT_MAX: u16 = 59_999;
/// Spare ports reserved in each block, so that a new published port rarely
/// forces a whole new block.
pub const PORT_MARGIN: usize = 4;

/// Hosts through which a configuration file can reach a published port.
pub const LOCAL_HOSTS: [&str; 4] = ["localhost", "127.0.0.1", "0.0.0.0", "[::1]"];

/// Host port of each published container port, keyed by `service:container_port`.
pub type PortMap = BTreeMap<String, u16>;

/// The key identifying a published container port.
pub fn port_key(service: &str, container_port: &str) -> String {
    format!("{service}:{container_port}")
}

/// An inclusive block of host ports, stored in JSON as `[first, last]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "[u16; 2]", into = "[u16; 2]")]
pub struct PortRange {
    /// First port of the block.
    pub first: u16,
    /// Last port of the block.
    pub last: u16,
}

impl PortRange {
    /// Every port of the block.
    pub fn ports(self) -> RangeInclusive<u16> {
        self.first..=self.last
    }

    /// Whether the two blocks share at least one port.
    pub fn overlaps(self, other: Self) -> bool {
        self.first <= other.last && other.first <= self.last
    }
}

impl From<[u16; 2]> for PortRange {
    fn from([first, last]: [u16; 2]) -> Self {
        Self { first, last }
    }
}

impl From<PortRange> for [u16; 2] {
    fn from(range: PortRange) -> Self {
        [range.first, range.last]
    }
}

impl fmt::Display for PortRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.first, self.last)
    }
}

/// Finds a block for `count` published ports plus [`PORT_MARGIN`].
///
/// The search starts at an offset derived from a hash of `env_name`, so the
/// same env always gets the same block, and moves forward one block at a
/// time while the candidate overlaps a `reserved` block or holds a port that
/// `is_free` reports as taken.
///
/// # Panics
///
/// Never: every candidate block lies within [`PORT_MIN`]`..=`[`PORT_MAX`].
pub fn allocate_range(
    env_name: &str,
    count: usize,
    reserved: &[PortRange],
    is_free: impl Fn(u16) -> bool,
) -> Result<PortRange> {
    let span = count.saturating_add(PORT_MARGIN);
    let capacity = usize::from(PORT_MAX - PORT_MIN) + 1;
    let width = capacity
        .checked_sub(span)
        .filter(|&width| width > 0)
        .ok_or(Error::TooManyPorts {
            count,
            first: PORT_MIN,
            last: PORT_MAX,
        })?;
    let offset = name_hash(env_name) % width as u64;
    // `offset < width <= capacity`, which fits in a u16 range.
    let offset = usize::try_from(offset).expect("the offset is below the port capacity");
    for step in 0..=width / span {
        let first = usize::from(PORT_MIN) + (offset + step * span) % width;
        let candidate = PortRange {
            first: to_port(first),
            last: to_port(first + span - 1),
        };
        if reserved.iter().any(|other| other.overlaps(candidate)) {
            continue;
        }
        if candidate.ports().all(&is_free) {
            return Ok(candidate);
        }
    }
    Err(Error::NoFreePortRange {
        span,
        first: PORT_MIN,
        last: PORT_MAX,
    })
}

/// The first 64 bits of the SHA-256 of `name`, big-endian.
fn name_hash(name: &str) -> u64 {
    let digest = Sha256::digest(name.as_bytes());
    let mut prefix = [0; 8];
    prefix.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(prefix)
}

fn to_port(value: usize) -> u16 {
    u16::try_from(value).expect("allocated ports stay within PORT_MIN..=PORT_MAX")
}

/// Assigns consecutive ports of `range` to `keys`, in order.
pub fn sequential_map(keys: &[String], range: PortRange) -> PortMap {
    keys.iter().cloned().zip(range.ports()).collect()
}

/// Port numbers to rewrite when a file moves from the env mapped by `before`
/// to the env mapped by `after`: `(old, new)` pairs for the ports that changed.
pub fn substitutions(before: &PortMap, after: &PortMap) -> Vec<(u16, u16)> {
    let pairs: BTreeSet<(u16, u16)> = before
        .iter()
        .filter_map(|(key, &old)| after.get(key).map(|&new| (old, new)))
        .filter(|(old, new)| old != new)
        .collect();
    pairs.into_iter().collect()
}

/// Result of [`rewrite_local_ports`].
#[derive(Debug, PartialEq, Eq)]
pub struct Rewrite {
    /// The rewritten text.
    pub text: String,
    /// How many distinct ports were rewritten.
    pub rewritten: usize,
}

/// Replaces `<local host>:<old port>` by the new port, and nothing else.
///
/// A port only counts when it follows one of [`LOCAL_HOSTS`] and a colon,
/// and is not followed by a letter or a digit: a bare `5432`, a `5432x` or a
/// port on a remote host is left alone. All pairs are applied in a single
/// pass, so `localhost:5672` can never corrupt `localhost:15672`, and a port
/// that was just rewritten is never rewritten again.
pub fn rewrite_local_ports(text: &str, substitutions: &[(u16, u16)]) -> Rewrite {
    let replacements: BTreeMap<String, String> = substitutions
        .iter()
        .map(|(old, new)| (old.to_string(), new.to_string()))
        .collect();
    let mut output = String::with_capacity(text.len());
    let mut rewritten = BTreeSet::new();
    let mut rest = text;
    while let Some(next) = rest.chars().next() {
        if let Some((prefix, port, replacement)) = match_local_port(rest, &replacements) {
            output.push_str(prefix);
            output.push_str(replacement);
            rewritten.insert(port);
            rest = &rest[prefix.len() + port.len()..];
        } else {
            output.push(next);
            rest = &rest[next.len_utf8()..];
        }
    }
    Rewrite {
        text: output,
        rewritten: rewritten.len(),
    }
}

/// Matches `<local host>:<port>` at the start of `text`, for a port listed in
/// `replacements`. Returns the `host:` prefix, the port and its replacement.
fn match_local_port<'t, 'r>(
    text: &'t str,
    replacements: &'r BTreeMap<String, String>,
) -> Option<(&'t str, &'t str, &'r str)> {
    LOCAL_HOSTS.iter().find_map(|host| {
        let after_host = text.strip_prefix(host)?.strip_prefix(':')?;
        let prefix = &text[..=host.len()];
        let length = after_host
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(after_host.len());
        let port = &after_host[..length];
        replacements
            .get(port)
            .map(|replacement| (prefix, port, replacement.as_str()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    fn allocate(name: &str, count: usize, reserved: &[PortRange]) -> Result<PortRange> {
        allocate_range(name, count, reserved, |_| true)
    }

    fn natural_range(name: &str, count: usize) -> PortRange {
        let span = count + PORT_MARGIN;
        let width = usize::from(PORT_MAX - PORT_MIN) + 1 - span;
        let first =
            usize::from(PORT_MIN) + usize::try_from(name_hash(name) % width as u64).unwrap();
        PortRange {
            first: to_port(first),
            last: to_port(first + span - 1),
        }
    }

    fn map(pairs: &[(&str, u16)]) -> PortMap {
        pairs
            .iter()
            .map(|&(key, port)| (key.to_owned(), port))
            .collect()
    }

    #[test]
    fn allocation_is_stable_for_a_name() {
        assert_eq!(
            allocate("feat-a", 2, &[]).unwrap(),
            allocate("feat-a", 2, &[]).unwrap()
        );
    }

    #[test]
    fn allocation_follows_the_specified_formula() {
        // A name keeps its block from one version to the next, so that an env
        // removed and created again gets its ports back: the offset is the
        // first 8 bytes of sha256("feat-a"), big-endian, modulo 40000 - 6.
        assert_eq!(name_hash("feat-a"), 0xf4ff_5ff4_2e07_23a6);
        assert_eq!(
            allocate("feat-a", 2, &[]).unwrap(),
            PortRange {
                first: 50_812,
                last: 50_817
            }
        );
    }

    #[test]
    fn block_size_is_the_port_count_plus_the_margin() {
        let range = allocate("feat-a", 3, &[]).unwrap();
        assert_eq!(range.ports().count(), 3 + PORT_MARGIN);
    }

    #[test]
    fn block_stays_within_bounds() {
        for name in ["a", "feat-a", "feat-b", "a-much-longer-env-name"] {
            let range = allocate(name, 2, &[]).unwrap();
            assert!(
                range.first >= PORT_MIN && range.last <= PORT_MAX,
                "{name}: {range}"
            );
        }
    }

    #[test]
    fn avoids_a_block_reserved_by_another_env() {
        // A reservation right on the natural block of "feat-a": without the
        // avoidance, the test would pass by chance.
        let natural = natural_range("feat-a", 2);
        let obtained = allocate("feat-a", 2, &[natural]).unwrap();
        assert!(!obtained.overlaps(natural), "{obtained} overlaps {natural}");
    }

    #[test]
    fn avoids_ports_in_use() {
        let natural = natural_range("feat-a", 2);
        let obtained = allocate_range("feat-a", 2, &[], |port| port != natural.first).unwrap();
        assert!(!obtained.ports().any(|port| port == natural.first));
    }

    #[test]
    fn refuses_more_ports_than_the_range_holds() {
        assert_matches!(
            allocate("feat-a", 99_999, &[]),
            Err(Error::TooManyPorts { .. })
        );
    }

    #[test]
    fn fails_when_every_port_is_taken() {
        assert_matches!(
            allocate_range("feat-a", 2, &[], |_| false),
            Err(Error::NoFreePortRange { .. })
        );
    }

    #[test]
    fn sequential_map_follows_key_order() {
        let keys = ["db:5432".to_owned(), "web:80".to_owned()];
        let range = PortRange {
            first: 30_000,
            last: 30_005,
        };
        assert_eq!(
            sequential_map(&keys, range),
            map(&[("db:5432", 30_000), ("web:80", 30_001)])
        );
    }

    #[test]
    fn port_range_serializes_as_a_pair() {
        let range = PortRange {
            first: 30_000,
            last: 30_006,
        };
        assert_eq!(serde_json::to_string(&range).unwrap(), "[30000,30006]");
        assert_eq!(
            serde_json::from_str::<PortRange>("[30000,30006]").unwrap(),
            range
        );
    }

    #[test]
    fn substitutions_ignore_unchanged_and_unknown_ports() {
        let before = map(&[("db:5432", 5432), ("web:80", 8080), ("gone:1", 1)]);
        let after = map(&[("db:5432", 5432), ("web:80", 30_001)]);
        assert_eq!(substitutions(&before, &after), vec![(8080, 30_001)]);
    }

    #[test]
    fn rewrites_local_hosts() {
        let text = "DATABASE_URL=postgres://u:p@localhost:5432/app\n\
                    AMQP=amqp://127.0.0.1:5672/\n\
                    MGMT=http://localhost:15672/\n";
        let rewrite = rewrite_local_ports(text, &[(5432, 30_000), (5672, 30_001), (15672, 30_002)]);
        assert!(rewrite.text.contains("localhost:30000/app"));
        assert!(rewrite.text.contains("127.0.0.1:30001/"));
        assert!(
            rewrite.text.contains("localhost:30002/"),
            "5672 must not corrupt 15672"
        );
        assert_eq!(rewrite.rewritten, 3);
    }

    #[test]
    fn leaves_non_ports_alone() {
        let text = "SIZE=5432\nTHRESHOLD=localhost:5432x\nZERO=localhost:05432\n";
        let rewrite = rewrite_local_ports(text, &[(5432, 30_000)]);
        assert_eq!(rewrite.text, text);
        assert_eq!(rewrite.rewritten, 0);
    }

    #[test]
    fn leaves_remote_hosts_alone() {
        let text = "URL=postgres://db.example.com:5432/app\n";
        assert_eq!(rewrite_local_ports(text, &[(5432, 30_000)]).text, text);
    }

    #[test]
    fn never_rewrites_a_port_twice() {
        // 8080 -> 5432 then 5432 -> 30000 would chain in a sequential rewrite.
        let rewrite = rewrite_local_ports("localhost:8080", &[(5432, 30_000), (8080, 5432)]);
        assert_eq!(rewrite.text, "localhost:5432");
    }

    #[test]
    fn keeps_non_ascii_text_intact() {
        let text = "# café → localhost:8080 ✓\n";
        assert_eq!(
            rewrite_local_ports(text, &[(8080, 30_000)]).text,
            "# café → localhost:30000 ✓\n"
        );
    }
}
