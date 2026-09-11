//! Glob patterns over relative paths, as git's `:(glob)` pathspecs read them.
//!
//! `*` matches any run of characters within one path component, `?` one
//! character, and a `**` component any number of components, none included.
//! A pattern matches a path, or a directory above it: `certs` matches
//! `certs/a.pem`, `**/.env` matches `.env` and `apps/api/.env`.

/// A parsed glob pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    components: Vec<String>,
}

/// A path split into its components, `.` and empty components dropped.
fn components(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect()
}

impl Pattern {
    /// Parses `pattern`, a path relative to the worktree.
    pub fn new(pattern: &str) -> Self {
        Self {
            components: components(pattern).into_iter().map(str::to_owned).collect(),
        }
    }

    /// Whether the pattern matches `path`, or one of the directories above it.
    pub fn matches(&self, path: &str) -> bool {
        let path = components(path);
        (1..=path.len()).any(|len| matches_components(&self.components, &path[..len]))
    }

    /// Whether the pattern names the directory `dir` explicitly rather than
    /// through `**`: `node_modules/**/.env` and `node_modules` do, `**/.env`
    /// does not.
    ///
    /// Directories git ignores as a whole, such as `node_modules/` or
    /// `target/`, are only entered by patterns that name them: `**/.env`
    /// means the project's own files, not those of its dependencies.
    pub fn names(&self, dir: &str) -> bool {
        let dir = components(dir);
        self.matches(&dir.join("/"))
            || (self.components.len() > dir.len()
                && self
                    .components
                    .iter()
                    .zip(&dir)
                    .all(|(pattern, name)| pattern != "**" && wildcard(pattern, name)))
    }
}

fn matches_components(pattern: &[String], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=path.len()).any(|skip| matches_components(rest, &path[skip..]))
        }
        Some((first, rest)) => path
            .split_first()
            .is_some_and(|(name, tail)| wildcard(first, name) && matches_components(rest, tail)),
    }
}

/// Matches one component against a pattern made of `*`, `?` and literal
/// characters.
fn wildcard(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    // Where the last `*` was, and the name position it currently stands for.
    let mut backtrack: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                backtrack = Some((p, n));
                p += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match backtrack {
                // Let the last `*` swallow one more character.
                Some((star, from)) => {
                    backtrack = Some((star, from + 1));
                    p = star + 1;
                    n = from + 1;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, path: &str) -> bool {
        Pattern::new(pattern).matches(path)
    }

    #[test]
    fn a_literal_path_matches_itself_and_what_is_below() {
        assert!(matches(".env", ".env"));
        assert!(matches("config/local.toml", "config/local.toml"));
        assert!(matches("certs", "certs/a.pem"));
        assert!(matches("certs/", "certs/sub/b.pem"));
        assert!(matches("./certs", "certs/a.pem"));
        assert!(!matches(".env", "apps/.env"), "anchored at the root");
        assert!(!matches("cert", "certs/a.pem"));
    }

    #[test]
    fn a_star_stays_within_one_component() {
        assert!(matches("apps/*/.env", "apps/api/.env"));
        assert!(!matches("apps/*/.env", "apps/api/deep/.env"));
        assert!(matches("*.pem", "key.pem"));
        assert!(matches(".env.*", ".env.local"));
        assert!(!matches(".env.*", ".env"));
        assert!(matches("a*b*c", "axxbyyc"));
        assert!(!matches("a*b*c", "axxbyy"));
    }

    #[test]
    fn a_question_mark_is_one_character() {
        assert!(matches("v?.txt", "v1.txt"));
        assert!(!matches("v?.txt", "v10.txt"));
    }

    #[test]
    fn a_double_star_spans_any_number_of_components() {
        for path in [".env", "apps/.env", "apps/api/.env"] {
            assert!(matches("**/.env", path), "{path}");
        }
        assert!(!matches("**/.env", "apps/api/.env.local"));
        assert!(matches("**/.env.*", "apps/api/.env.local"));
        assert!(matches("vendor/**/*.pem", "vendor/x/y/k.pem"));
        assert!(matches("vendor/**/*.pem", "vendor/k.pem"));
    }

    #[test]
    fn a_directory_is_named_only_without_double_star() {
        assert!(!Pattern::new("**/.env").names("node_modules"));
        assert!(Pattern::new("node_modules/**/.env").names("node_modules"));
        assert!(Pattern::new("certs").names("certs"));
        assert!(
            Pattern::new("certs").names("certs/sub"),
            "inside a named directory"
        );
        assert!(Pattern::new("vendor/*/k.pem").names("vendor/x"));
        assert!(!Pattern::new("vendor/*/k.pem").names("vendor/x/y"));
        assert!(!Pattern::new("config/local.toml").names("certs"));
    }
}
