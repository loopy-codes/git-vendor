//! In-source vendoring for Git repositories.
//!
//! Vendor dependencies are tracked via custom attributes in `.gitattributes`:
//!
//! ```text
//! path/to/dep/* vendored vendor-name=owner/repo vendor-url=https://example.com/owner/repo.git vendor-branch=main
//! ```
//!
//! Fetched content is stored under `refs/vendor/<name>`.

use git_filter_tree::FilterTree;
use git_set_attr::SetAttr;
use git2::build::CheckoutBuilder;
use git2::{Error, FetchOptions, MergeOptions, Oid, Repository};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

/// High-level options for [`Vendor::vendor_merge`], mirroring `git merge` flags.
///
/// These control the commit/staging behavior of the merge. The low-level
/// tree-merge algorithm is configured separately via [`MergeOptions`].
#[derive(Debug, Default)]
pub struct VendorMergeOpts {
    /// Perform the merge and update the working tree and index, but do not
    /// create a commit.  `MERGE_HEAD` is recorded so that a subsequent
    /// `git commit` produces a merge commit (`--no-commit`).
    pub no_commit: bool,
    /// Like `no_commit`, but also omits `MERGE_HEAD` so the eventual commit
    /// is an ordinary (non-merge) commit (`--squash`).
    pub squash: bool,
    /// Override the default merge commit message (`-m`).
    pub message: Option<String>,
}

/// A vendored dependency parsed from `.gitattributes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorDep {
    pub name: String,
    pub pattern: String,
    pub url: String,
    pub branch: Option<String>,
}

pub trait Vendor {
    /// Add the pattern to the appropriate `.gitattributes` file using `git_set_attr`.
    ///
    /// If there is a `.gitattributes` file in the current directory, that file is used.
    /// Otherwise, the first found `.gitattributes` file when walking up the directory
    /// tree from the current directory to the repository root directory is used.
    ///
    /// If the pattern is already specified, the `url` and `branch` are updated if necessary.
    ///
    /// The `maybe_name` argument overrides the dependency name. When `None`, the name is
    /// derived from the URL as `owner/repo`. Local paths (non-URL remotes)
    /// require an explicit name.
    fn track_pattern(
        &self,
        pattern: &str,
        url: &str,
        maybe_branch: Option<&str>,
        maybe_name: Option<&str>,
    ) -> Result<(), Error>;

    /// Remove the pattern from the appropriate `.gitattributes` file using `git_set_attr`.
    ///
    /// If there is a `.gitattributes` file in the current directory, that file is used.
    /// Otherwise, the first found `.gitattributes` file when walking up the directory
    /// tree from the current directory to the repository root directory is used.
    fn untrack_pattern(&self, pattern: &str) -> Result<(), Error>;

    /// Return the status of all vendored content, or any errors encountered along the way.
    fn vendor_status(&self, maybe_pattern: Option<&str>) -> Result<(), Error>;

    /// Fetch the latest content from all relevant vendor sources.
    ///
    /// All vendor refs are stored under `/refs/vendor/`.
    fn vendor_fetch(
        &self,
        maybe_pattern: Option<&str>,
        fetch_opts: Option<&mut FetchOptions<'_>>,
    ) -> Result<(), Error>;

    /// Merge the latest content from all relevant vendor sources.
    ///
    /// Behaves like `git merge`: updates the working tree and index, optionally
    /// creates a merge commit, and records `MERGE_HEAD`/`MERGE_MSG` when
    /// appropriate.
    fn vendor_merge(
        &self,
        maybe_pattern: Option<&str>,
        opts: &VendorMergeOpts,
        merge_opts: Option<&MergeOptions>,
    ) -> Result<(), Error>;
}

impl Vendor for Repository {
    fn track_pattern(
        &self,
        pattern: &str,
        url: &str,
        maybe_branch: Option<&str>,
        maybe_name: Option<&str>,
    ) -> Result<(), Error> {
        require_non_bare(self)?;

        let name = resolve_name(url, maybe_name)?;

        let name_attr = format!("vendor-name={name}");
        let url_attr = format!("vendor-url={url}");

        let mut attrs: Vec<&str> = vec!["vendored", &name_attr, &url_attr];

        let branch_attr;
        if let Some(branch) = maybe_branch {
            branch_attr = format!("vendor-branch={branch}");
            attrs.push(&branch_attr);
        }

        self.set_attr(pattern, &attrs, None)
    }

    fn untrack_pattern(&self, pattern: &str) -> Result<(), Error> {
        require_non_bare(self)?;

        let path = find_gitattributes(self)?;
        if !path.exists() {
            return Ok(());
        }

        remove_vendor_lines(&path, pattern)
    }

    fn vendor_status(&self, maybe_pattern: Option<&str>) -> Result<(), Error> {
        require_non_bare(self)?;

        let path = find_gitattributes(self)?;
        let deps = parse_vendor_deps(&path)?;
        let deps = filter_deps(&deps, maybe_pattern);

        if deps.is_empty() {
            println!("No vendored dependencies tracked");
            return Ok(());
        }

        for dep in deps {
            println!("{} ({})", dep.name, dep.pattern);
            println!("  URL: {}", dep.url);
            match &dep.branch {
                Some(b) => println!("  Branch: {b}"),
                None => println!("  Branch: (default)"),
            }

            let ref_name = vendor_ref_name(&dep.name);
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

    fn vendor_fetch(
        &self,
        maybe_pattern: Option<&str>,
        mut fetch_opts: Option<&mut FetchOptions<'_>>,
    ) -> Result<(), Error> {
        require_non_bare(self)?;

        let path = find_gitattributes(self)?;
        let deps = parse_vendor_deps(&path)?;
        let deps = filter_deps(&deps, maybe_pattern);

        if deps.is_empty() {
            return Err(Error::from_str("No vendored dependencies to fetch"));
        }

        for dep in deps {
            let ref_target = vendor_ref_name(&dep.name);

            let branch_display = dep.branch.as_deref().unwrap_or("HEAD");
            println!(
                "Fetching {} from {} ({})",
                dep.name, dep.url, branch_display
            );

            let mut remote = self.remote_anonymous(&dep.url)?;
            let refspec = match &dep.branch {
                Some(branch) => format!("+refs/heads/{branch}:{ref_target}"),
                None => format!("+HEAD:{ref_target}"),
            };
            remote.fetch(&[&refspec], fetch_opts.as_mut().map(|o| &mut **o), None)?;

            println!("  Fetched to {ref_target}");
        }

        Ok(())
    }

    fn vendor_merge(
        &self,
        maybe_pattern: Option<&str>,
        opts: &VendorMergeOpts,
        merge_opts: Option<&MergeOptions>,
    ) -> Result<(), Error> {
        require_non_bare(self)?;

        let path = find_gitattributes(self)?;
        let deps = parse_vendor_deps(&path)?;
        let deps = filter_deps(&deps, maybe_pattern);

        if deps.is_empty() {
            return Err(Error::from_str("No vendored dependencies to merge"));
        }

        let skip_commit = opts.no_commit || opts.squash;
        if skip_commit && deps.len() > 1 {
            return Err(Error::from_str(
                "--no-commit and --squash require a single dependency; \
                 specify a pattern to select one",
            ));
        }

        for dep in &deps {
            let ref_name = vendor_ref_name(&dep.name);

            println!("Merging {} ({})", dep.name, dep.pattern);

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

            let mut index = self.merge_trees(&head_tree, &head_tree, &filtered_tree, merge_opts)?;

            let default_message = format!("Merge vendored dependency: {}", dep.name);
            let message = opts.message.as_deref().unwrap_or(&default_message);

            if index.has_conflicts() {
                // Write the conflicted index to the repository so the user can
                // resolve in the working tree.
                let mut repo_index = self.index()?;
                repo_index.read_tree(&head_tree)?;
                for conflict in index.conflicts()? {
                    let conflict = conflict?;
                    if let Some(entry) = &conflict.our {
                        repo_index.add(entry)?;
                    }
                    if let Some(entry) = &conflict.their {
                        repo_index.add(entry)?;
                    }
                }
                repo_index.write()?;

                let mut co = CheckoutBuilder::new();
                co.allow_conflicts(true).conflict_style_merge(true);
                self.checkout_index(Some(&mut repo_index), Some(&mut co))?;

                if !opts.squash {
                    set_merge_head(self, vendor_oid)?;
                }
                set_merge_msg(self, message)?;

                return Err(Error::from_str(&format!(
                    "Conflicts detected while merging {}. \
                     Resolve them and commit the result.",
                    dep.name
                )));
            }

            // Clean merge — write the tree, update index and working directory.
            let merged_oid = index.write_tree_to(self)?;
            let merged_tree = self.find_tree(merged_oid)?;

            let mut repo_index = self.index()?;
            repo_index.read_tree(&merged_tree)?;
            repo_index.write()?;

            let mut co = CheckoutBuilder::new();
            co.force();
            self.checkout_tree(merged_tree.as_object(), Some(&mut co))?;

            if skip_commit {
                if !opts.squash {
                    set_merge_head(self, vendor_oid)?;
                }
                set_merge_msg(self, message)?;
                println!("  Merged (not committed)");
            } else {
                let signature = self.signature()?;
                self.commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    message,
                    &merged_tree,
                    &[&head_commit, &vendor_commit],
                )?;
                println!("  Merged successfully");
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Merge state helpers
// ---------------------------------------------------------------------------

/// Write `MERGE_HEAD` so that a subsequent `git commit` creates a merge commit.
fn set_merge_head(repo: &Repository, oid: Oid) -> Result<(), Error> {
    let path = repo.path().join("MERGE_HEAD");
    fs::write(&path, format!("{oid}\n")).map_err(|e| Error::from_str(&e.to_string()))
}

/// Write `MERGE_MSG` so that `git commit` picks up the message.
fn set_merge_msg(repo: &Repository, msg: &str) -> Result<(), Error> {
    let path = repo.path().join("MERGE_MSG");
    fs::write(&path, format!("{msg}\n")).map_err(|e| Error::from_str(&e.to_string()))
}

// ---------------------------------------------------------------------------
// Repository helpers
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

/// Resolve the vendor dependency name.
///
/// If `maybe_name` is provided, it is used as-is. Otherwise the name is
/// derived from the URL by extracting the last two path segments (typically
/// `owner/repo`), stripping any `.git` suffix. Local (non-URL) remotes
/// **must** supply an explicit name.
fn resolve_name(url: &str, maybe_name: Option<&str>) -> Result<String, Error> {
    if let Some(name) = maybe_name {
        if name.is_empty() {
            return Err(Error::from_str("Vendor dependency name must not be empty"));
        }
        return Ok(name.to_string());
    }

    name_from_url(url).ok_or_else(|| {
        Error::from_str(
            "Cannot derive a vendor name from a local path. \
             Please provide an explicit name.",
        )
    })
}

/// Return `true` if `url` looks like a remote URL rather than a local path.
///
/// Recognizes `scheme://...` and SCP-style `user@host:path`.
fn is_remote_url(url: &str) -> bool {
    // scheme://...
    if url.contains("://") {
        return true;
    }
    // SCP-style: git@host:path  (must have @ before : and no path separators before @)
    if let Some(at) = url.find('@') {
        if let Some(colon) = url[at..].find(':') {
            let colon_pos = at + colon;
            // Make sure the part before @ has no slashes (not a path)
            if !url[..at].contains('/') && colon_pos + 1 < url.len() {
                return true;
            }
        }
    }
    false
}

/// Try to extract `owner/repo` from a remote URL.
///
/// Supports:
/// - `https://host/owner/repo.git`
/// - `https://host/owner/repo`
/// - `git@host:owner/repo.git`
/// - `ssh://git@host/owner/repo.git`
///
/// Returns `None` for local paths or URLs with fewer than two path segments.
fn name_from_url(url: &str) -> Option<String> {
    if !is_remote_url(url) {
        return None;
    }

    // Normalize: strip trailing `/` and `.git` suffix.
    let mut cleaned = url.trim_end_matches('/');
    cleaned = cleaned.strip_suffix(".git").unwrap_or(cleaned);

    // Extract the path portion.
    let path = if let Some(rest) = cleaned.split("://").nth(1) {
        // scheme://[user@]host/path... → everything after the first `/`
        rest.find('/').map(|i| &rest[i + 1..])
    } else if let Some(at) = cleaned.find('@') {
        // SCP-style: user@host:path
        cleaned[at..].find(':').map(|i| &cleaned[at + i + 1..])
    } else {
        None
    }?;

    // Take the last two segments.
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None;
    }

    let owner = segments[segments.len() - 2];
    let repo = segments[segments.len() - 1];
    Some(format!("{owner}/{repo}"))
}

/// Build the full ref path for a vendor dependency, e.g. `refs/vendor/owner/repo`.
fn vendor_ref_name(name: &str) -> String {
    format!("refs/vendor/{name}")
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
/// A line is recognized as a vendor dependency when it carries at least
/// `vendor-name=` and `vendor-url=`. The `vendor-branch=` attribute is
/// optional — when absent, the dependency tracks the remote's default branch.
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

        let mut name = None;
        let mut url = None;
        let mut branch = None;
        let mut is_vendored = false;

        for attr in parts {
            if attr == "vendored" {
                is_vendored = true;
            } else if let Some(v) = attr.strip_prefix("vendor-name=") {
                name = Some(v.to_string());
            } else if let Some(v) = attr.strip_prefix("vendor-url=") {
                url = Some(v.to_string());
            } else if let Some(v) = attr.strip_prefix("vendor-branch=") {
                branch = Some(v.to_string());
            }
        }

        if !is_vendored {
            continue;
        }

        if let (Some(name), Some(url)) = (name, url) {
            deps.push(VendorDep {
                name,
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
            // FIXME: what if other non-vendor-related attributes are on this line?
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
/// vendor attribute (`vendored`, `vendor-name=`, `vendor-url=`, or
/// `vendor-branch=`).
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

    parts.any(|attr| {
        attr == "vendored"
            || attr.starts_with("vendor-name=")
            || attr.starts_with("vendor-url=")
            || attr.starts_with("vendor-branch=")
    })
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

    // -- is_remote_url ------------------------------------------------------

    #[test]
    fn is_remote_url_https() {
        assert!(is_remote_url("https://github.com/owner/repo.git"));
    }

    #[test]
    fn is_remote_url_ssh_scheme() {
        assert!(is_remote_url("ssh://git@github.com/owner/repo.git"));
    }

    #[test]
    fn is_remote_url_scp_style() {
        assert!(is_remote_url("git@github.com:owner/repo.git"));
    }

    #[test]
    fn is_remote_url_rejects_absolute_path() {
        assert!(!is_remote_url("/home/user/repos/mylib"));
    }

    #[test]
    fn is_remote_url_rejects_relative_path() {
        assert!(!is_remote_url("../repos/mylib"));
    }

    // -- name_from_url ------------------------------------------------------

    #[test]
    fn name_from_url_https() {
        assert_eq!(
            name_from_url("https://github.com/owner/repo.git"),
            Some("owner/repo".into())
        );
    }

    #[test]
    fn name_from_url_https_no_dotgit() {
        assert_eq!(
            name_from_url("https://github.com/owner/repo"),
            Some("owner/repo".into())
        );
    }

    #[test]
    fn name_from_url_https_trailing_slash() {
        assert_eq!(
            name_from_url("https://github.com/owner/repo.git/"),
            Some("owner/repo".into())
        );
    }

    #[test]
    fn name_from_url_scp_style() {
        assert_eq!(
            name_from_url("git@github.com:owner/repo.git"),
            Some("owner/repo".into())
        );
    }

    #[test]
    fn name_from_url_ssh_scheme() {
        assert_eq!(
            name_from_url("ssh://git@github.com/owner/repo.git"),
            Some("owner/repo".into())
        );
    }

    #[test]
    fn name_from_url_deep_path_takes_last_two() {
        assert_eq!(
            name_from_url("https://gitlab.com/group/sub/owner/repo.git"),
            Some("owner/repo".into())
        );
    }

    #[test]
    fn name_from_url_too_few_segments() {
        assert_eq!(name_from_url("https://github.com/repo.git"), None);
    }

    #[test]
    fn name_from_url_local_path() {
        assert_eq!(name_from_url("/home/user/repos/mylib"), None);
    }

    #[test]
    fn name_from_url_relative_path() {
        assert_eq!(name_from_url("../repos/mylib"), None);
    }

    // -- resolve_name -------------------------------------------------------

    #[test]
    fn resolve_name_explicit() {
        assert_eq!(
            resolve_name("https://github.com/a/b.git", Some("custom")).unwrap(),
            "custom"
        );
    }

    #[test]
    fn resolve_name_derived_from_url() {
        assert_eq!(
            resolve_name("https://github.com/owner/repo.git", None).unwrap(),
            "owner/repo"
        );
    }

    #[test]
    fn resolve_name_local_path_requires_explicit() {
        assert!(resolve_name("/local/path", None).is_err());
    }

    #[test]
    fn resolve_name_rejects_empty_explicit() {
        assert!(resolve_name("https://github.com/a/b.git", Some("")).is_err());
    }

    // -- vendor_ref_name ----------------------------------------------------

    #[test]
    fn vendor_ref_name_owner_repo() {
        assert_eq!(vendor_ref_name("owner/repo"), "refs/vendor/owner/repo");
    }

    #[test]
    fn vendor_ref_name_custom_name() {
        assert_eq!(vendor_ref_name("custom-name"), "refs/vendor/custom-name");
    }

    #[test]
    fn vendor_ref_name_multiple_slashes() {
        assert_eq!(
            vendor_ref_name("multiple/slash/names"),
            "refs/vendor/multiple/slash/names"
        );
    }

    // -- parse_vendor_deps --------------------------------------------------

    #[test]
    fn parse_vendor_deps_from_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".gitattributes");

        let mut f = fs::File::create(&path).unwrap();
        writeln!(
            f,
            "*.txt vendored vendor-name=o/r1 vendor-url=https://a.com/o/r1.git vendor-branch=main"
        )
        .unwrap();
        writeln!(
            f,
            "*.rs vendored vendor-name=o/r2 vendor-url=https://b.com/o/r2.git vendor-branch=dev"
        )
        .unwrap();
        writeln!(
            f,
            "*.toml vendored vendor-name=o/r3 vendor-url=https://c.com/o/r3.git"
        )
        .unwrap();
        writeln!(f, "# comment").unwrap();
        writeln!(f, "*.md diff").unwrap();
        writeln!(f).unwrap();
        drop(f);

        let deps = parse_vendor_deps(&path).unwrap();
        assert_eq!(deps.len(), 3);

        assert_eq!(deps[0].name, "o/r1");
        assert_eq!(deps[0].pattern, "*.txt");
        assert_eq!(deps[0].url, "https://a.com/o/r1.git");
        assert_eq!(deps[0].branch, Some("main".into()));

        assert_eq!(deps[1].name, "o/r2");
        assert_eq!(deps[1].pattern, "*.rs");
        assert_eq!(deps[1].url, "https://b.com/o/r2.git");
        assert_eq!(deps[1].branch, Some("dev".into()));

        assert_eq!(deps[2].name, "o/r3");
        assert_eq!(deps[2].pattern, "*.toml");
        assert_eq!(deps[2].url, "https://c.com/o/r3.git");
        assert_eq!(deps[2].branch, None);
    }

    #[test]
    fn parse_vendor_deps_missing_file_returns_empty() {
        let deps = parse_vendor_deps(Path::new("/nonexistent/.gitattributes")).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn parse_vendor_deps_skips_lines_missing_any_required_vendor_attr() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".gitattributes");

        // Missing vendor-name → skip
        fs::write(
            &path,
            "*.txt vendor-url=https://a.com/o/r.git vendor-branch=main\n",
        )
        .unwrap();
        assert!(parse_vendor_deps(&path).unwrap().is_empty());

        // Missing vendor-url → skip
        fs::write(&path, "*.txt vendor-name=o/r vendor-branch=main\n").unwrap();
        assert!(parse_vendor_deps(&path).unwrap().is_empty());
    }

    #[test]
    fn parse_vendor_deps_branch_is_optional() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".gitattributes");

        // Missing vendor-branch → still parsed, branch is None
        fs::write(
            &path,
            "*.txt vendored vendor-name=o/r vendor-url=https://a.com/o/r.git\n",
        )
        .unwrap();
        let deps = parse_vendor_deps(&path).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].branch, None);
    }

    // -- is_vendor_line_for_pattern -----------------------------------------

    #[test]
    fn is_vendor_line_matches() {
        assert!(is_vendor_line_for_pattern(
            "*.txt vendored vendor-name=o/r vendor-url=https://a.com vendor-branch=main",
            "*.txt"
        ));
    }

    #[test]
    fn is_vendor_line_matches_vendored_only() {
        assert!(is_vendor_line_for_pattern("*.txt vendored", "*.txt"));
    }

    #[test]
    fn is_vendor_line_ignores_other_patterns() {
        assert!(!is_vendor_line_for_pattern(
            "*.rs vendored vendor-name=o/r vendor-url=https://a.com vendor-branch=main",
            "*.txt"
        ));
    }

    #[test]
    fn is_vendor_line_ignores_non_vendor_lines() {
        assert!(!is_vendor_line_for_pattern("*.txt diff -text", "*.txt"));
    }

    #[test]
    fn is_vendor_line_ignores_comments_and_blanks() {
        assert!(!is_vendor_line_for_pattern("# comment", "*.txt"));
        assert!(!is_vendor_line_for_pattern("", "*.txt"));
        assert!(!is_vendor_line_for_pattern("   ", "*.txt"));
    }

    // -- remove_vendor_lines ------------------------------------------------

    #[test]
    fn remove_vendor_lines_keeps_non_vendor() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".gitattributes");

        let original = "\
*.txt vendored vendor-name=o/r vendor-url=https://a.com vendor-branch=main
*.txt diff
*.rs vendored vendor-name=x/y vendor-url=https://b.com vendor-branch=dev
# comment
";
        fs::write(&path, original).unwrap();

        remove_vendor_lines(&path, "*.txt").unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("vendor-url=https://a.com"));
        assert!(content.contains("*.txt diff"));
        assert!(content.contains("*.rs vendored vendor-name=x/y"));
        assert!(content.contains("# comment"));
    }

    #[test]
    fn remove_vendor_lines_noop_for_missing_file() {
        assert!(remove_vendor_lines(Path::new("/nonexistent/.gitattributes"), "*.txt").is_ok());
    }

    // -- filter_deps --------------------------------------------------------

    #[test]
    fn filter_deps_none_returns_all() {
        let deps = vec![
            VendorDep {
                name: "a/b".into(),
                pattern: "a".into(),
                url: "u".into(),
                branch: Some("b".into()),
            },
            VendorDep {
                name: "c/d".into(),
                pattern: "b".into(),
                url: "u".into(),
                branch: None,
            },
        ];
        assert_eq!(filter_deps(&deps, None).len(), 2);
    }

    #[test]
    fn filter_deps_exact_match() {
        let deps = vec![
            VendorDep {
                name: "a/b".into(),
                pattern: "*.txt".into(),
                url: "u".into(),
                branch: Some("b".into()),
            },
            VendorDep {
                name: "c/d".into(),
                pattern: "*.rs".into(),
                url: "u".into(),
                branch: None,
            },
        ];
        let filtered = filter_deps(&deps, Some("*.txt"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].pattern, "*.txt");
    }

    #[test]
    fn filter_deps_no_match() {
        let deps = vec![VendorDep {
            name: "a/b".into(),
            pattern: "*.txt".into(),
            url: "u".into(),
            branch: Some("b".into()),
        }];
        assert!(filter_deps(&deps, Some("*.rs")).is_empty());
    }
}
