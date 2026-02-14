//! In-source vendoring for Git repositories.
//!
//! Vendor dependencies are tracked via custom attributes in `.gitattributes`:
//!
//! ```text
//! path/to/dep/* vendor-url=https://example.com/repo.git vendor-branch=main
//! ```
//!
//! Fetched content is stored under `refs/vendor/<sanitized-pattern>`.

use git_filter_tree::FilterTree;
use git_set_attr::SetAttr;
use git2::{Error, Repository};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

/// A vendored dependency parsed from `.gitattributes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorDep {
    pub pattern: String,
    pub url: String,
    pub branch: String,
}

pub trait Vendor {
    /// Add the pattern to the appropriate `.gitattributes` file using `git_set_attr`.
    ///
    /// If there is a `.gitattributes` file in the current directory, that file is used.
    /// Otherwise, the first found `.gitattributes` file when walking up the directory
    /// tree from the current directory to the repository root directory is used.
    ///
    /// If the pattern is already specified, the `url` and `branch` are updated if necessary.
    fn track_pattern(
        &self,
        pattern: &str,
        url: &str,
        maybe_branch: Option<&str>,
    ) -> Result<(), Error>;

    /// Remove the pattern from the appropriate `.gitattributes` file using `git_set_attr`.
    ///
    /// If there is a `.gitattributes` file in the current directory, that file is used.
    /// Otherwise, the first found `.gitattributes` file when walking up the directory
    /// tree from the current directory to the repository root directory is used.
    fn untrack_pattern(&self, pattern: &str) -> Result<(), Error>;

    /// Return the status of all vendored content, or any errors encountered along the way.
    fn status(&self, maybe_pattern: Option<&str>) -> Result<(), Error>;

    /// Fetch the latest content from all relevant vendor sources.
    ///
    /// All vendor refs are stored under `/refs/vendor/`.
    fn fetch(&self, maybe_pattern: Option<&str>) -> Result<(), Error>;

    /// Merge the latest content from all relevant vendor sources.
    fn merge(&self, maybe_pattern: Option<&str>) -> Result<(), Error>;
}

impl Vendor for Repository {
    fn track_pattern(
        &self,
        pattern: &str,
        url: &str,
        maybe_branch: Option<&str>,
    ) -> Result<(), Error> {
        require_non_bare(self)?;

        let branch = maybe_branch.unwrap_or("main");
        let url_attr = format!("vendor-url={url}");
        let branch_attr = format!("vendor-branch={branch}");

        self.set_attr(pattern, &[&url_attr, &branch_attr], None)
    }

    fn untrack_pattern(&self, pattern: &str) -> Result<(), Error> {
        require_non_bare(self)?;

        let path = find_gitattributes(self)?;
        if !path.exists() {
            return Ok(());
        }

        remove_vendor_lines(&path, pattern)
    }

    fn status(&self, maybe_pattern: Option<&str>) -> Result<(), Error> {
        require_non_bare(self)?;

        let path = find_gitattributes(self)?;
        let deps = parse_vendor_deps(&path)?;
        let deps = filter_deps(&deps, maybe_pattern);

        if deps.is_empty() {
            println!("No vendored dependencies tracked");
            return Ok(());
        }

        for dep in deps {
            println!("Pattern: {}", dep.pattern);
            println!("  URL: {}", dep.url);
            println!("  Branch: {}", dep.branch);

            let ref_name = vendor_ref_name(&dep.pattern);
            match self.find_reference(&ref_name) {
                Ok(reference) => {
                    if let Some(oid) = reference.target() {
                        println!("  Ref: {ref_name} ({oid})");
                    } else {
                        println!("  Ref: {ref_name} (symbolic)");
                    }
                }
                Err(_) => {
                    println!("  Ref: {ref_name} (not fetched)");
                }
            }
            println!();
        }

        Ok(())
    }

    fn fetch(&self, maybe_pattern: Option<&str>) -> Result<(), Error> {
        require_non_bare(self)?;

        let path = find_gitattributes(self)?;
        let deps = parse_vendor_deps(&path)?;
        let deps = filter_deps(&deps, maybe_pattern);

        if deps.is_empty() {
            return Err(Error::from_str("No vendored dependencies to fetch"));
        }

        for dep in deps {
            let sanitized = sanitize_ref_component(&dep.pattern);
            let ref_target = vendor_ref_name(&dep.pattern);

            println!("Fetching {} from {} ({})", dep.pattern, dep.url, dep.branch);

            let remote_name = format!("vendor-{sanitized}");
            let _ = self.remote_delete(&remote_name);

            let mut remote = self.remote(&remote_name, &dep.url)?;
            let refspec = format!("+refs/heads/{}:{ref_target}", dep.branch);
            remote.fetch(&[&refspec], None, None)?;

            let _ = self.remote_delete(&remote_name);

            println!("  Fetched to {ref_target}");
        }

        Ok(())
    }

    fn merge(&self, maybe_pattern: Option<&str>) -> Result<(), Error> {
        require_non_bare(self)?;

        let path = find_gitattributes(self)?;
        let deps = parse_vendor_deps(&path)?;
        let deps = filter_deps(&deps, maybe_pattern);

        if deps.is_empty() {
            return Err(Error::from_str("No vendored dependencies to merge"));
        }

        for dep in deps {
            let ref_name = vendor_ref_name(&dep.pattern);

            println!("Merging {}", dep.pattern);

            let reference = self.find_reference(&ref_name).map_err(|_| {
                Error::from_str(&format!(
                    "Vendor ref {ref_name} not found. Run fetch first."
                ))
            })?;

            let vendor_oid = reference
                .target()
                .ok_or_else(|| Error::from_str("Invalid vendor reference"))?;
            let vendor_commit = self.find_commit(vendor_oid)?;
            let vendor_tree = vendor_commit.tree()?;

            let filtered_tree = self.filter_by_patterns(&vendor_tree, &[&dep.pattern])?;

            let head = self.head()?;
            let head_commit = head.peel_to_commit()?;
            let head_tree = head_commit.tree()?;

            let mut index = self.merge_trees(&head_tree, &head_tree, &filtered_tree, None)?;

            if index.has_conflicts() {
                return Err(Error::from_str(&format!(
                    "Conflicts detected while merging {}",
                    dep.pattern
                )));
            }

            let merged_oid = index.write_tree_to(self)?;
            let merged_tree = self.find_tree(merged_oid)?;

            let signature = self.signature()?;
            let message = format!("Merge vendored dependency: {}", dep.pattern);

            self.commit(
                Some("HEAD"),
                &signature,
                &signature,
                &message,
                &merged_tree,
                &[&head_commit, &vendor_commit],
            )?;

            println!("  Merged successfully");
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_non_bare(repo: &Repository) -> Result<(), Error> {
    if repo.is_bare() {
        Err(Error::from_str(
            "This operation is not supported in a bare repository",
        ))
    } else {
        Ok(())
    }
}

/// Build the full ref path for a vendor pattern, e.g. `refs/vendor/STAR.txt`.
fn vendor_ref_name(pattern: &str) -> String {
    format!("refs/vendor/{}", sanitize_ref_component(pattern))
}

/// Sanitize a pattern into a component safe for use in a git ref name.
fn sanitize_ref_component(pattern: &str) -> String {
    pattern
        .replace('*', "STAR")
        .replace('?', "QMARK")
        .replace('[', "(")
        .replace(']', ")")
        .replace(' ', "_")
        .replace('/', "-")
}

/// Find the appropriate `.gitattributes` file by walking from the current
/// directory up to the repository root.
///
/// Returns the path of the first `.gitattributes` file found, or defaults to
/// `<current_dir>/.gitattributes` (which will be created on first write).
fn find_gitattributes(repo: &Repository) -> Result<PathBuf, Error> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| Error::from_str("Repository has no working directory"))?;

    let current_dir = std::env::current_dir()
        .map_err(|e| Error::from_str(&format!("Failed to get current directory: {e}")))?;

    let mut dir = current_dir.as_path();
    while dir.starts_with(workdir) {
        let candidate = dir.join(".gitattributes");
        if candidate.exists() {
            return Ok(candidate);
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }

    Ok(current_dir.join(".gitattributes"))
}

/// Parse vendor dependencies from a `.gitattributes` file.
///
/// Only lines that contain **both** `vendor-url=<value>` and
/// `vendor-branch=<value>` attributes are returned.
fn parse_vendor_deps(path: &Path) -> Result<Vec<VendorDep>, Error> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(path)
        .map_err(|e| Error::from_str(&format!("Failed to open {}: {e}", path.display())))?;

    let mut deps = Vec::new();

    for line in BufReader::new(file).lines() {
        let line =
            line.map_err(|e| Error::from_str(&format!("Failed to read .gitattributes: {e}")))?;
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let pattern = match parts.next() {
            Some(p) => p,
            None => continue,
        };

        let mut url = None;
        let mut branch = None;

        for attr in parts {
            if let Some(v) = attr.strip_prefix("vendor-url=") {
                url = Some(v.to_string());
            } else if let Some(v) = attr.strip_prefix("vendor-branch=") {
                branch = Some(v.to_string());
            }
        }

        if let (Some(url), Some(branch)) = (url, branch) {
            deps.push(VendorDep {
                pattern: pattern.to_string(),
                url,
                branch,
            });
        }
    }

    Ok(deps)
}

/// Remove all lines from a `.gitattributes` file that match `pattern` **and**
/// carry vendor attributes.  Non-vendor lines for the same pattern are kept.
fn remove_vendor_lines(path: &Path, pattern: &str) -> Result<(), Error> {
    if !path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(path)
        .map_err(|e| Error::from_str(&format!("Failed to read {}: {e}", path.display())))?;

    let mut kept = Vec::new();
    for line in content.lines() {
        if is_vendor_line_for_pattern(line, pattern) {
            continue;
        }
        kept.push(line);
    }

    let mut file = fs::File::create(path)
        .map_err(|e| Error::from_str(&format!("Failed to write {}: {e}", path.display())))?;

    for line in &kept {
        writeln!(file, "{line}")
            .map_err(|e| Error::from_str(&format!("Failed to write .gitattributes: {e}")))?;
    }

    file.flush()
        .map_err(|e| Error::from_str(&format!("Failed to flush .gitattributes: {e}")))?;

    Ok(())
}

/// Return `true` if `line` starts with `pattern` and contains at least one
/// `vendor-url=` or `vendor-branch=` attribute.
fn is_vendor_line_for_pattern(line: &str, pattern: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }

    let mut parts = trimmed.split_whitespace();
    let line_pattern = match parts.next() {
        Some(p) => p,
        None => return false,
    };

    if line_pattern != pattern {
        return false;
    }

    parts.any(|attr| attr.starts_with("vendor-url=") || attr.starts_with("vendor-branch="))
}

/// Filter dependencies by exact pattern match.
fn filter_deps<'a>(deps: &'a [VendorDep], filter: Option<&str>) -> Vec<&'a VendorDep> {
    match filter {
        None => deps.iter().collect(),
        Some(f) => deps.iter().filter(|d| d.pattern == f).collect(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// Mutex to serialize tests that call `std::env::set_current_dir`, since
    /// the current directory is process-global state.
    static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn setup_repo() -> (Repository, TempDir) {
        let dir = TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@test").unwrap();

        // Create an initial empty commit so HEAD exists.
        let sig = repo.signature().unwrap();
        let oid = {
            let mut idx = repo.index().unwrap();
            idx.write_tree().unwrap()
        };
        {
            let tree = repo.find_tree(oid).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }

        (repo, dir)
    }

    fn write_gitattributes(dir: &Path, content: &str) {
        let path = dir.join(".gitattributes");
        let mut f = fs::File::create(&path).unwrap();
        write!(f, "{content}").unwrap();
    }

    // -----------------------------------------------------------------------
    // Pure helper tests (no cwd dependency)
    // -----------------------------------------------------------------------

    #[test]
    fn sanitize_ref_component_replaces_globs() {
        assert_eq!(sanitize_ref_component("*.txt"), "STAR.txt");
        assert_eq!(sanitize_ref_component("vendor/*"), "vendor-STAR");
        assert_eq!(sanitize_ref_component("src/[a-z]?"), "src-(a-z)QMARK");
    }

    #[test]
    fn vendor_ref_name_has_correct_prefix() {
        assert_eq!(vendor_ref_name("*.txt"), "refs/vendor/STAR.txt");
    }

    #[test]
    fn parse_vendor_deps_from_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".gitattributes");

        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "*.txt vendor-url=https://a.com/r.git vendor-branch=main").unwrap();
        writeln!(f, "*.rs vendor-url=https://b.com/r.git vendor-branch=dev").unwrap();
        writeln!(f, "# comment").unwrap();
        writeln!(f, "*.md diff").unwrap();
        writeln!(f, "").unwrap();
        drop(f);

        let deps = parse_vendor_deps(&path).unwrap();
        assert_eq!(deps.len(), 2);

        assert_eq!(deps[0].pattern, "*.txt");
        assert_eq!(deps[0].url, "https://a.com/r.git");
        assert_eq!(deps[0].branch, "main");

        assert_eq!(deps[1].pattern, "*.rs");
        assert_eq!(deps[1].url, "https://b.com/r.git");
        assert_eq!(deps[1].branch, "dev");
    }

    #[test]
    fn parse_vendor_deps_missing_file_returns_empty() {
        let deps = parse_vendor_deps(Path::new("/nonexistent/.gitattributes")).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn parse_vendor_deps_skips_partial_vendor_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".gitattributes");

        // Only vendor-url, missing vendor-branch → should be skipped.
        fs::write(&path, "*.txt vendor-url=https://a.com/r.git\n").unwrap();

        let deps = parse_vendor_deps(&path).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn is_vendor_line_for_pattern_matches() {
        assert!(is_vendor_line_for_pattern(
            "*.txt vendor-url=https://a.com vendor-branch=main",
            "*.txt"
        ));
    }

    #[test]
    fn is_vendor_line_for_pattern_ignores_other_patterns() {
        assert!(!is_vendor_line_for_pattern(
            "*.rs vendor-url=https://a.com vendor-branch=main",
            "*.txt"
        ));
    }

    #[test]
    fn is_vendor_line_for_pattern_ignores_non_vendor_lines() {
        assert!(!is_vendor_line_for_pattern("*.txt diff -text", "*.txt"));
    }

    #[test]
    fn is_vendor_line_for_pattern_ignores_comments_and_blanks() {
        assert!(!is_vendor_line_for_pattern("# comment", "*.txt"));
        assert!(!is_vendor_line_for_pattern("", "*.txt"));
        assert!(!is_vendor_line_for_pattern("   ", "*.txt"));
    }

    #[test]
    fn remove_vendor_lines_keeps_non_vendor() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".gitattributes");

        let original = "\
*.txt vendor-url=https://a.com vendor-branch=main
*.txt diff
*.rs vendor-url=https://b.com vendor-branch=dev
# comment
";
        fs::write(&path, original).unwrap();

        remove_vendor_lines(&path, "*.txt").unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("vendor-url=https://a.com"));
        assert!(content.contains("*.txt diff"));
        assert!(content.contains("*.rs vendor-url=https://b.com"));
        assert!(content.contains("# comment"));
    }

    #[test]
    fn remove_vendor_lines_noop_for_missing_file() {
        assert!(remove_vendor_lines(Path::new("/nonexistent/.gitattributes"), "*.txt").is_ok());
    }

    #[test]
    fn filter_deps_none_returns_all() {
        let deps = vec![
            VendorDep {
                pattern: "a".into(),
                url: "u".into(),
                branch: "b".into(),
            },
            VendorDep {
                pattern: "b".into(),
                url: "u".into(),
                branch: "b".into(),
            },
        ];
        assert_eq!(filter_deps(&deps, None).len(), 2);
    }

    #[test]
    fn filter_deps_exact_match() {
        let deps = vec![
            VendorDep {
                pattern: "*.txt".into(),
                url: "u".into(),
                branch: "b".into(),
            },
            VendorDep {
                pattern: "*.rs".into(),
                url: "u".into(),
                branch: "b".into(),
            },
        ];
        let filtered = filter_deps(&deps, Some("*.txt"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].pattern, "*.txt");
    }

    #[test]
    fn filter_deps_no_match() {
        let deps = vec![VendorDep {
            pattern: "*.txt".into(),
            url: "u".into(),
            branch: "b".into(),
        }];
        assert!(filter_deps(&deps, Some("*.rs")).is_empty());
    }

    // -----------------------------------------------------------------------
    // Trait method tests (need cwd)
    // -----------------------------------------------------------------------

    #[test]
    fn track_pattern_writes_gitattributes() {
        let _guard = CWD_LOCK.lock().unwrap();
        let (repo, dir) = setup_repo();
        std::env::set_current_dir(dir.path()).unwrap();

        repo.track_pattern("*.txt", "https://example.com/r.git", Some("main"))
            .unwrap();

        let content = fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
        assert!(content.contains("*.txt"));
        assert!(content.contains("vendor-url=https://example.com/r.git"));
        assert!(content.contains("vendor-branch=main"));
    }

    #[test]
    fn track_pattern_defaults_to_main_branch() {
        let _guard = CWD_LOCK.lock().unwrap();
        let (repo, dir) = setup_repo();
        std::env::set_current_dir(dir.path()).unwrap();

        repo.track_pattern("*.rs", "https://example.com/r.git", None)
            .unwrap();

        let content = fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
        assert!(content.contains("vendor-branch=main"));
    }

    #[test]
    fn untrack_pattern_removes_vendor_lines() {
        let _guard = CWD_LOCK.lock().unwrap();
        let (repo, dir) = setup_repo();
        std::env::set_current_dir(dir.path()).unwrap();

        repo.track_pattern("*.txt", "https://example.com/r.git", Some("main"))
            .unwrap();

        // Verify it was tracked.
        let ga = dir.path().join(".gitattributes");
        let deps = parse_vendor_deps(&ga).unwrap();
        assert_eq!(deps.len(), 1);

        // Untrack.
        repo.untrack_pattern("*.txt").unwrap();

        // The vendor line should be gone.
        let deps = parse_vendor_deps(&ga).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn untrack_pattern_is_noop_without_gitattributes() {
        let _guard = CWD_LOCK.lock().unwrap();
        let (repo, dir) = setup_repo();
        std::env::set_current_dir(dir.path()).unwrap();

        assert!(repo.untrack_pattern("*.txt").is_ok());
    }

    #[test]
    fn status_ok_with_no_deps() {
        let _guard = CWD_LOCK.lock().unwrap();
        let (repo, dir) = setup_repo();
        std::env::set_current_dir(dir.path()).unwrap();

        assert!(repo.status(None).is_ok());
    }

    #[test]
    fn status_ok_with_tracked_dep() {
        let _guard = CWD_LOCK.lock().unwrap();
        let (repo, dir) = setup_repo();
        std::env::set_current_dir(dir.path()).unwrap();

        write_gitattributes(
            dir.path(),
            "*.txt vendor-url=https://example.com/r.git vendor-branch=main\n",
        );

        assert!(repo.status(None).is_ok());
    }

    #[test]
    fn fetch_errors_with_no_deps() {
        let _guard = CWD_LOCK.lock().unwrap();
        let (repo, dir) = setup_repo();
        std::env::set_current_dir(dir.path()).unwrap();

        let err = repo.fetch(None).unwrap_err();
        assert!(err.message().contains("No vendored dependencies to fetch"));
    }

    #[test]
    fn merge_errors_with_no_deps() {
        let _guard = CWD_LOCK.lock().unwrap();
        let (repo, dir) = setup_repo();
        std::env::set_current_dir(dir.path()).unwrap();

        let err = Vendor::merge(&repo, None).unwrap_err();
        assert!(err.message().contains("No vendored dependencies to merge"));
    }

    #[test]
    fn bare_repo_rejects_all_operations() {
        let dir = TempDir::new().unwrap();
        let repo = Repository::init_bare(dir.path()).unwrap();

        assert!(
            repo.track_pattern("*.txt", "https://example.com/r.git", None)
                .is_err()
        );
        assert!(repo.untrack_pattern("*.txt").is_err());
        assert!(repo.status(None).is_err());
        assert!(repo.fetch(None).is_err());
        assert!(Vendor::merge(&repo, None).is_err());
    }
}
