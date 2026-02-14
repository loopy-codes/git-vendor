//! Provides a `FilterTree` trait, and an implementation for `git2::Repository`, which allows for pruning trees by Git pathspec patterns.

pub use git2::{Error, Repository};
use globset::{GlobSet, GlobSetBuilder};

#[cfg(feature = "cli")]
pub mod cli;

pub trait FilterTree {
    /// Filters tree entries by gitattributes-style patterns and returns a new tree with contents filtered through the provided patterns.
    /// Recursively walks the tree and matches patterns against full paths from the tree root.
    fn filter_by_patterns<'a>(
        &'a self,
        tree: &'a git2::Tree<'a>,
        patterns: &[&str],
    ) -> Result<git2::Tree<'a>, Error>;
}

impl FilterTree for git2::Repository {
    fn filter_by_patterns<'a>(
        &'a self,
        tree: &'a git2::Tree<'a>,
        patterns: &[&str],
    ) -> Result<git2::Tree<'a>, Error> {
        if patterns.is_empty() {
            return Err(Error::from_str("At least one pattern is required"));
        }

        // Build GlobSet matcher
        let mut glob_builder = GlobSetBuilder::new();
        for pattern in patterns {
            let glob = globset::Glob::new(pattern)
                .map_err(|e| Error::from_str(&format!("Invalid pattern '{}': {}", pattern, e)))?;
            glob_builder.add(glob);
        }

        let matcher = glob_builder
            .build()
            .map_err(|e| Error::from_str(&e.to_string()))?;

        // Recursively filter the tree
        filter_tree_recursive(self, tree, "", &matcher)
    }
}

/// Recursively filters a tree, matching patterns against full paths.
/// Returns a new tree containing only entries that match or have matching descendants.
fn filter_tree_recursive<'a>(
    repo: &'a Repository,
    tree: &'a git2::Tree<'a>,
    prefix: &str,
    matcher: &GlobSet,
) -> Result<git2::Tree<'a>, Error> {
    let mut builder = repo.treebuilder(None)?;

    for entry in tree.iter() {
        let name = entry.name().unwrap_or("");
        let full_path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", prefix, name)
        };

        match entry.kind() {
            Some(git2::ObjectType::Blob) => {
                // Check if this file matches the pattern
                if matcher.is_match(&full_path) {
                    builder.insert(name, entry.id(), entry.filemode())?;
                }
            }
            Some(git2::ObjectType::Tree) => {
                // Recursively filter the subtree
                let subtree = entry.to_object(repo)?.peel_to_tree()?;
                match filter_tree_recursive(repo, &subtree, &full_path, matcher) {
                    Ok(filtered_subtree) => {
                        // Only include the subtree if it has matching entries
                        if filtered_subtree.len() > 0 {
                            builder.insert(name, filtered_subtree.id(), entry.filemode())?;
                        }
                    }
                    Err(_) => {
                        // Skip subtrees that cause errors
                        continue;
                    }
                }
            }
            _ => {
                // Skip other object types (commits, tags, etc.)
                continue;
            }
        }
    }

    let tree_oid = builder.write()?;
    repo.find_tree(tree_oid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn setup_test_repo() -> (Repository, PathBuf) {
        let thread_id = std::thread::current().id();
        let temp_path = std::env::temp_dir().join(format!("git-filter-tree-test-{:?}", thread_id));
        let _ = fs::remove_dir_all(&temp_path);
        fs::create_dir_all(&temp_path).unwrap();
        let repo = Repository::init_bare(&temp_path).unwrap();
        (repo, temp_path)
    }

    fn cleanup_test_repo(path: PathBuf) {
        let _ = fs::remove_dir_all(path);
    }

    fn create_test_tree<'a>(repo: &'a Repository) -> Result<git2::Tree<'a>, Error> {
        let mut tree_builder = repo.treebuilder(None)?;

        // Create some blob entries
        let blob1 = repo.blob(b"content1")?;
        let blob2 = repo.blob(b"content2")?;
        let blob3 = repo.blob(b"content3")?;

        tree_builder.insert("file1.txt", blob1, 0o100644)?;
        tree_builder.insert("file2.rs", blob2, 0o100644)?;
        tree_builder.insert("test.md", blob3, 0o100644)?;

        let tree_oid = tree_builder.write()?;
        repo.find_tree(tree_oid)
    }

    #[test]
    fn test_filter_single_pattern() -> Result<(), Error> {
        let (repo, temp_path) = setup_test_repo();

        let tree = create_test_tree(&repo)?;
        assert_eq!(tree.len(), 3);

        // Filter for .txt files only
        let filtered = repo.filter_by_patterns(&tree, &["*.txt"])?;
        assert_eq!(filtered.len(), 1);
        assert!(filtered.get_name("file1.txt").is_some());
        assert!(filtered.get_name("file2.rs").is_none());
        assert!(filtered.get_name("test.md").is_none());

        cleanup_test_repo(temp_path);
        Ok(())
    }

    #[test]
    fn test_filter_multiple_patterns() -> Result<(), Error> {
        let (repo, temp_path) = setup_test_repo();

        let tree = create_test_tree(&repo)?;

        // Filter for .txt and .rs files
        let filtered = repo.filter_by_patterns(&tree, &["*.txt", "*.rs"])?;
        assert_eq!(filtered.len(), 2);
        assert!(filtered.get_name("file1.txt").is_some());
        assert!(filtered.get_name("file2.rs").is_some());
        assert!(filtered.get_name("test.md").is_none());

        cleanup_test_repo(temp_path);
        Ok(())
    }

    #[test]
    fn test_filter_exact_match() -> Result<(), Error> {
        let (repo, temp_path) = setup_test_repo();

        let tree = create_test_tree(&repo)?;

        // Filter for exact filename
        let filtered = repo.filter_by_patterns(&tree, &["file1.txt"])?;
        assert_eq!(filtered.len(), 1);
        assert!(filtered.get_name("file1.txt").is_some());

        cleanup_test_repo(temp_path);
        Ok(())
    }

    #[test]
    fn test_filter_wildcard_patterns() -> Result<(), Error> {
        let (repo, temp_path) = setup_test_repo();

        let tree = create_test_tree(&repo)?;

        // Filter with wildcard pattern
        let filtered = repo.filter_by_patterns(&tree, &["file*"])?;
        assert_eq!(filtered.len(), 2);
        assert!(filtered.get_name("file1.txt").is_some());
        assert!(filtered.get_name("file2.rs").is_some());
        assert!(filtered.get_name("test.md").is_none());

        cleanup_test_repo(temp_path);
        Ok(())
    }

    #[test]
    fn test_filter_no_matches() -> Result<(), Error> {
        let (repo, temp_path) = setup_test_repo();

        let tree = create_test_tree(&repo)?;

        // Filter with pattern that matches nothing
        let filtered = repo.filter_by_patterns(&tree, &["*.nonexistent"])?;
        assert_eq!(filtered.len(), 0);

        cleanup_test_repo(temp_path);
        Ok(())
    }

    #[test]
    fn test_filter_all_matches() -> Result<(), Error> {
        let (repo, temp_path) = setup_test_repo();

        let tree = create_test_tree(&repo)?;

        // Filter with pattern that matches everything
        let filtered = repo.filter_by_patterns(&tree, &["*"])?;
        assert_eq!(filtered.len(), 3);

        cleanup_test_repo(temp_path);
        Ok(())
    }

    #[test]
    fn test_filter_empty_patterns_error() {
        let (repo, temp_path) = setup_test_repo();

        let tree = create_test_tree(&repo).unwrap();

        // Empty patterns should return an error
        let result = repo.filter_by_patterns(&tree, &[]);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().message(),
            "At least one pattern is required"
        );

        cleanup_test_repo(temp_path);
    }

    #[test]
    fn test_filter_invalid_pattern_error() {
        let (repo, temp_path) = setup_test_repo();

        let tree = create_test_tree(&repo).unwrap();

        // Invalid glob pattern should return an error
        let result = repo.filter_by_patterns(&tree, &["[invalid"]);
        assert!(result.is_err());

        cleanup_test_repo(temp_path);
    }

    #[test]
    fn test_filter_with_nested_tree() -> Result<(), Error> {
        let (repo, temp_path) = setup_test_repo();

        let mut tree_builder = repo.treebuilder(None)?;

        // Create a nested tree
        let mut subtree_builder = repo.treebuilder(None)?;
        let blob = repo.blob(b"nested content")?;
        subtree_builder.insert("nested.txt", blob, 0o100644)?;
        let subtree_oid = subtree_builder.write()?;

        // Add files and subtree to main tree
        let blob1 = repo.blob(b"content1")?;
        tree_builder.insert("file1.txt", blob1, 0o100644)?;
        tree_builder.insert("subdir", subtree_oid, 0o040000)?;

        let tree_oid = tree_builder.write()?;
        let tree = repo.find_tree(tree_oid)?;

        // Filter - should keep both file and directory
        let filtered = repo.filter_by_patterns(&tree, &["*"])?;
        assert_eq!(filtered.len(), 2);

        cleanup_test_repo(temp_path);
        Ok(())
    }

    #[test]
    fn test_filter_preserves_empty_tree() -> Result<(), Error> {
        let (repo, temp_path) = setup_test_repo();

        // Create an empty tree
        let tree_builder = repo.treebuilder(None)?;
        let tree_oid = tree_builder.write()?;
        let tree = repo.find_tree(tree_oid)?;

        assert_eq!(tree.len(), 0);

        // Filter empty tree
        let filtered = repo.filter_by_patterns(&tree, &["*"])?;
        assert_eq!(filtered.len(), 0);

        cleanup_test_repo(temp_path);
        Ok(())
    }

    #[test]
    fn test_filter_case_sensitive() -> Result<(), Error> {
        let (repo, temp_path) = setup_test_repo();

        let mut tree_builder = repo.treebuilder(None)?;
        let blob1 = repo.blob(b"content1")?;
        let blob2 = repo.blob(b"content2")?;

        tree_builder.insert("File.txt", blob1, 0o100644)?;
        tree_builder.insert("file.txt", blob2, 0o100644)?;

        let tree_oid = tree_builder.write()?;
        let tree = repo.find_tree(tree_oid)?;

        // Filter with exact case match
        let filtered = repo.filter_by_patterns(&tree, &["file.txt"])?;
        assert_eq!(filtered.len(), 1);
        assert!(filtered.get_name("file.txt").is_some());

        cleanup_test_repo(temp_path);
        Ok(())
    }

    #[test]
    fn test_filter_complex_patterns() -> Result<(), Error> {
        let (repo, temp_path) = setup_test_repo();

        let mut tree_builder = repo.treebuilder(None)?;
        let blob = repo.blob(b"content")?;

        tree_builder.insert("test1.txt", blob, 0o100644)?;
        tree_builder.insert("test2.rs", blob, 0o100644)?;
        tree_builder.insert("data.json", blob, 0o100644)?;
        tree_builder.insert("README.md", blob, 0o100644)?;

        let tree_oid = tree_builder.write()?;
        let tree = repo.find_tree(tree_oid)?;

        // Multiple patterns with different wildcards
        let filtered = repo.filter_by_patterns(&tree, &["test*", "*.md"])?;
        assert_eq!(filtered.len(), 3);
        assert!(filtered.get_name("test1.txt").is_some());
        assert!(filtered.get_name("test2.rs").is_some());
        assert!(filtered.get_name("README.md").is_some());
        assert!(filtered.get_name("data.json").is_none());

        cleanup_test_repo(temp_path);
        Ok(())
    }
}
