//! Set gitattributes via patterns and key-value pairs.
//!
//! # Overview
//!
//! When checking attribute values for a given path, multiple locations are
//! checked by Git:
//!
//! 1. the Git installation;
//! 2. system Git configuration files;
//! 3. user Git configuration files;
//! 4. repository Git configuration files;
//! 5. local (untracked, unreplicable) Git configuration files in `.git/`.
//!
//! When *writing* attribute values, users typically write to repository
//! configuration files.

pub use git2::{Error, Repository};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

/// A trait which provides methods for settings attributes in a Git repository.
pub trait SetAttr {
    /// Set attributes in the appropriate `.gitattributes` file.
    ///
    /// The `.gitattributes` file in the current directory is used if one exists;
    /// otherwise, the `.gitattributes` file found first while
    /// walking up the directory tree from the current directory to the
    /// repository's root directory is used.
    fn set_attr(
        &self,
        pattern: &str,
        attributes: &[&str],
        gitattributes: Option<&Path>,
    ) -> Result<(), Error>;
}

impl SetAttr for Repository {
    fn set_attr(
        &self,
        pattern: &str,
        attributes: &[&str],
        gitattributes: Option<&Path>,
    ) -> Result<(), Error> {
        let gitattributes_path = if let Some(path) = gitattributes {
            path.to_path_buf()
        } else {
            find_gitattributes_file(self)?
        };

        validate_attributes(attributes)?;

        let mut lines = if gitattributes_path.exists() {
            let file = fs::File::open(&gitattributes_path)
                .map_err(|e| Error::from_str(&format!("Failed to open .gitattributes: {e}")))?;
            let reader = BufReader::new(file);
            reader
                .lines()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| Error::from_str(&format!("Failed to read .gitattributes: {e}")))?
        } else {
            Vec::new()
        };

        let new_attrs = filter_new_attributes(pattern, attributes, &lines);

        if !new_attrs.is_empty() {
            let attr_line = format_attribute_line(pattern, &new_attrs);
            lines.push(attr_line);
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&gitattributes_path)
            .map_err(|e| {
                Error::from_str(&format!("Failed to open .gitattributes for writing: {e}"))
            })?;

        for line in lines {
            writeln!(file, "{line}")
                .map_err(|e| Error::from_str(&format!("Failed to write to .gitattributes: {e}")))?;
        }

        file.flush()
            .map_err(|e| Error::from_str(&format!("Failed to flush .gitattributes: {e}")))?;

        Ok(())
    }
}

/// Filter out attributes that already exist for the given pattern.
///
/// Parses every existing line that matches `pattern` and collects its
/// attribute name/state pairs, then returns only those entries from
/// `attributes` whose state differs (or that are completely new).
fn filter_new_attributes(pattern: &str, attributes: &[&str], lines: &[String]) -> Vec<String> {
    use std::collections::HashMap;

    let mut existing_attrs: HashMap<String, String> = HashMap::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let line_pattern = parts.next().unwrap_or("");

        if line_pattern == pattern {
            for attr_str in parts {
                let (name, state) = parse_attribute_string(attr_str);
                existing_attrs.insert(name, state);
            }
        }
    }

    let mut new_attrs = Vec::new();
    for attr_str in attributes {
        let attr_str = attr_str.trim();
        if attr_str.is_empty() {
            continue;
        }

        let (name, state) = parse_attribute_string(attr_str);

        if existing_attrs.get(&name) != Some(&state) {
            new_attrs.push(attr_str.to_string());
        }
    }

    new_attrs
}

/// Parse an attribute string to extract name and state.
///
/// Returns `(name, state_string)` where `state_string` uniquely identifies
/// the state:
///
/// | Syntax        | Name     | State            |
/// |---------------|----------|------------------|
/// | `attr`        | `attr`   | `"set"`          |
/// | `attr=true`   | `attr`   | `"set"`          |
/// | `-attr`       | `attr`   | `"unset"`        |
/// | `attr=false`  | `attr`   | `"unset"`        |
/// | `!attr`       | `attr`   | `"unspecified"`  |
/// | `attr=value`  | `attr`   | `"value:value"`  |
fn parse_attribute_string(attr: &str) -> (String, String) {
    let attr = attr.trim();

    if let Some(stripped) = attr.strip_prefix('-') {
        (stripped.to_string(), "unset".to_string())
    } else if let Some(stripped) = attr.strip_prefix('!') {
        (stripped.to_string(), "unspecified".to_string())
    } else if let Some((name, value)) = attr.split_once('=') {
        match value {
            "true" => (name.to_string(), "set".to_string()),
            "false" => (name.to_string(), "unset".to_string()),
            _ => (name.to_string(), format!("value:{value}")),
        }
    } else {
        (attr.to_string(), "set".to_string())
    }
}

/// Validate attribute strings.
fn validate_attributes(attributes: &[&str]) -> Result<(), Error> {
    for attr in attributes {
        let attr = attr.trim();
        if attr.is_empty() {
            continue;
        }

        let has_whitespace = |s: &str| s.is_empty() || s.contains(char::is_whitespace);

        if let Some(stripped) = attr.strip_prefix('-') {
            if has_whitespace(stripped) {
                return Err(Error::from_str(&format!("Invalid attribute '{attr}'")));
            }
        } else if let Some(stripped) = attr.strip_prefix('!') {
            if has_whitespace(stripped) {
                return Err(Error::from_str(&format!("Invalid attribute '{attr}'")));
            }
        } else if let Some((name, _value)) = attr.split_once('=') {
            if has_whitespace(name) {
                return Err(Error::from_str(&format!("Invalid attribute '{attr}'")));
            }
        } else if attr.contains(char::is_whitespace) {
            return Err(Error::from_str(&format!("Invalid attribute '{attr}'")));
        }
    }

    Ok(())
}

/// Format a pattern and attributes into a gitattributes line.
fn format_attribute_line(pattern: &str, attributes: &[impl AsRef<str>]) -> String {
    let mut line = pattern.to_string();

    for attr in attributes {
        let attr = attr.as_ref().trim();
        if attr.is_empty() {
            continue;
        }

        line.push(' ');
        line.push_str(attr);
    }

    line
}

/// Find the appropriate `.gitattributes` file by walking from the current
/// directory up to the repository root.
///
/// Returns the path of the first `.gitattributes` file found, or defaults to
/// `<current_dir>/.gitattributes` (which will be created on first write).
fn find_gitattributes_file(repo: &Repository) -> Result<PathBuf, Error> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| Error::from_str("Repository has no working directory"))?;

    let current_dir = std::env::current_dir()
        .map_err(|e| Error::from_str(&format!("Failed to get current directory: {e}")))?;

    let mut dir = current_dir.as_path();
    while dir.starts_with(workdir) {
        let gitattributes = dir.join(".gitattributes");
        if gitattributes.exists() {
            return Ok(gitattributes);
        }

        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }

    // No .gitattributes found; default to one in the current directory.
    Ok(current_dir.join(".gitattributes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_set_attribute() {
        assert_eq!(
            parse_attribute_string("diff"),
            ("diff".into(), "set".into())
        );
    }

    #[test]
    fn parse_set_attribute_explicit_true() {
        assert_eq!(
            parse_attribute_string("diff=true"),
            ("diff".into(), "set".into())
        );
    }

    #[test]
    fn parse_unset_attribute_prefix() {
        assert_eq!(
            parse_attribute_string("-diff"),
            ("diff".into(), "unset".into())
        );
    }

    #[test]
    fn parse_unset_attribute_explicit_false() {
        assert_eq!(
            parse_attribute_string("diff=false"),
            ("diff".into(), "unset".into())
        );
    }

    #[test]
    fn parse_unspecified_attribute() {
        assert_eq!(
            parse_attribute_string("!diff"),
            ("diff".into(), "unspecified".into())
        );
    }

    #[test]
    fn parse_value_attribute() {
        assert_eq!(
            parse_attribute_string("filter=lfs"),
            ("filter".into(), "value:lfs".into())
        );
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(
            parse_attribute_string("  text  "),
            ("text".into(), "set".into())
        );
    }

    #[test]
    fn validate_accepts_valid_attributes() {
        assert!(validate_attributes(&["diff", "-text", "!eol", "filter=lfs"]).is_ok());
        assert!(validate_attributes(&["diff=true", "text=false"]).is_ok());
    }

    #[test]
    fn validate_accepts_empty() {
        assert!(validate_attributes(&[]).is_ok());
        assert!(validate_attributes(&["", "  "]).is_ok());
    }

    #[test]
    fn validate_rejects_bare_minus() {
        assert!(validate_attributes(&["-"]).is_err());
    }

    #[test]
    fn validate_rejects_bare_bang() {
        assert!(validate_attributes(&["!"]).is_err());
    }

    #[test]
    fn validate_rejects_whitespace_in_name() {
        assert!(validate_attributes(&["my attr"]).is_err());
        assert!(validate_attributes(&["-my attr"]).is_err());
        assert!(validate_attributes(&["!my attr"]).is_err());
        assert!(validate_attributes(&["my attr=value"]).is_err());
    }

    #[test]
    fn validate_rejects_empty_name_with_value() {
        assert!(validate_attributes(&["=value"]).is_err());
    }

    #[test]
    fn format_single_attribute() {
        assert_eq!(format_attribute_line("*.txt", &["diff"]), "*.txt diff");
    }

    #[test]
    fn format_multiple_attributes() {
        assert_eq!(
            format_attribute_line("*.txt", &["diff", "-text", "filter=lfs"]),
            "*.txt diff -text filter=lfs"
        );
    }

    #[test]
    fn format_skips_empty_attributes() {
        assert_eq!(format_attribute_line("*.txt", &[""]), "*.txt");
        assert_eq!(
            format_attribute_line("*.txt", &["", "diff", ""]),
            "*.txt diff"
        );
    }

    #[test]
    fn format_trims_attribute_whitespace() {
        assert_eq!(
            format_attribute_line("*.txt", &["  diff  ", "  -text  "]),
            "*.txt diff -text"
        );
    }

    #[test]
    fn filter_returns_all_for_empty_file() {
        let result = filter_new_attributes("*.txt", &["diff", "-text", "filter=lfs"], &[]);
        assert_eq!(result, vec!["diff", "-text", "filter=lfs"]);
    }

    #[test]
    fn filter_removes_exact_duplicates() {
        let lines = vec!["*.txt diff -text".into()];
        let result = filter_new_attributes("*.txt", &["diff", "-text"], &lines);
        assert!(result.is_empty());
    }

    #[test]
    fn filter_keeps_new_attributes() {
        let lines = vec!["*.txt diff -text".into()];
        let result = filter_new_attributes("*.txt", &["diff", "eol=lf"], &lines);
        assert_eq!(result, vec!["eol=lf"]);
    }

    #[test]
    fn filter_semantic_set_equivalence() {
        // diff=true is the same as diff
        let lines = vec!["*.txt diff".into()];
        assert!(filter_new_attributes("*.txt", &["diff=true"], &lines).is_empty());
    }

    #[test]
    fn filter_semantic_unset_equivalence() {
        // diff=false is the same as -diff
        let lines = vec!["*.txt -diff".into()];
        assert!(filter_new_attributes("*.txt", &["diff=false"], &lines).is_empty());
    }

    #[test]
    fn filter_set_differs_from_unset() {
        let lines = vec!["*.txt diff".into()];
        let result = filter_new_attributes("*.txt", &["-diff"], &lines);
        assert_eq!(result, vec!["-diff"]);
    }

    #[test]
    fn filter_collects_across_multiple_lines() {
        let lines = vec![
            "*.txt diff".into(),
            "*.txt filter=lfs".into(),
            "*.txt -text".into(),
        ];
        assert!(
            filter_new_attributes("*.txt", &["diff", "filter=lfs", "-text"], &lines).is_empty()
        );
    }

    #[test]
    fn filter_ignores_other_patterns() {
        let lines = vec!["*.md diff".into()];
        let result = filter_new_attributes("*.txt", &["diff"], &lines);
        assert_eq!(result, vec!["diff"]);
    }

    #[test]
    fn filter_skips_comments_and_blanks() {
        let lines = vec![
            "# comment".into(),
            "*.txt diff".into(),
            "  ".into(),
            "  # indented comment".into(),
        ];
        let result = filter_new_attributes("*.txt", &["diff", "-text"], &lines);
        assert_eq!(result, vec!["-text"]);
    }

    #[test]
    fn filter_distinguishes_different_values() {
        let lines = vec!["*.txt filter=foo".into()];
        assert!(filter_new_attributes("*.txt", &["filter=foo"], &lines).is_empty());
        assert_eq!(
            filter_new_attributes("*.txt", &["filter=bar"], &lines),
            vec!["filter=bar"]
        );
    }
}
