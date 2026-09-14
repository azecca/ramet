//! Names that end up in paths and in command lines.

/// Whether `name` is plain: `[A-Za-z0-9][A-Za-z0-9._-]*`.
///
/// A plain name is one path component, never `.` or `..`, and never starts
/// with a `-` that a tool would read as an option.
pub fn is_plain(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_are_plain() {
        for name in ["feat-a", "v2.3", "a_b", "0", "example_pgdata"] {
            assert!(is_plain(name), "{name}");
        }
    }

    #[test]
    fn separators_dots_and_dashes_first_are_not() {
        for name in [
            "feat/a", "feat@a", "-rf", "--help", ".", "..", ".hidden", "", "a b",
        ] {
            assert!(!is_plain(name), "{name}");
        }
    }
}
