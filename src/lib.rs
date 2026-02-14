//! Repositories are a myth!

use git2::{Error, Repository};

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
        let _ = (pattern, url, maybe_branch);
        Err(Error::from_str("Not yet implemented"))
    }

    fn untrack_pattern(&self, pattern: &str) -> Result<(), Error> {
        let _ = pattern;
        Err(Error::from_str("Not yet implemented"))
    }

    fn status(&self, maybe_pattern: Option<&str>) -> Result<(), Error> {
        let _ = maybe_pattern;
        Err(Error::from_str("Not yet implemented"))
    }

    fn fetch(&self, maybe_pattern: Option<&str>) -> Result<(), Error> {
        let _ = maybe_pattern;
        Err(Error::from_str("Not yet implemented"))
    }

    fn merge(&self, maybe_pattern: Option<&str>) -> Result<(), Error> {
        let _ = maybe_pattern;
        Err(Error::from_str("Not yet implemented"))
    }
}
