use git_set_attr::SetAttr;
use git2::Repository;
use std::fs;
use tempfile::TempDir;

fn read(path: &std::path::Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn creates_file_from_scratch() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    repo.set_attr("*.txt", &["diff", "-text"], Some(&ga))
        .unwrap();

    assert!(ga.exists());
    assert_eq!(read(&ga).trim(), "*.txt diff -text");
}

#[test]
fn appends_to_existing_file() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    fs::write(&ga, "*.md text\n").unwrap();
    repo.set_attr("*.txt", &["diff", "-text"], Some(&ga))
        .unwrap();

    let content = read(&ga);
    assert!(content.contains("*.md text"), "original line missing");
    assert!(
        content.contains("*.txt diff -text"),
        "new line missing: {content}"
    );
}

#[test]
fn exact_duplicate_is_noop() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    fs::write(&ga, "*.txt diff\n").unwrap();
    repo.set_attr("*.txt", &["diff"], Some(&ga)).unwrap();

    let content = read(&ga);
    assert_eq!(
        content.lines().filter(|l| l.starts_with("*.txt")).count(),
        1,
        "should not duplicate: {content}"
    );
}

#[test]
fn semantic_duplicate_set_is_noop() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    // `diff` and `diff=true` are semantically identical
    fs::write(&ga, "*.txt diff\n").unwrap();
    repo.set_attr("*.txt", &["diff=true"], Some(&ga)).unwrap();

    let content = read(&ga);
    assert_eq!(
        content.lines().filter(|l| l.starts_with("*.txt")).count(),
        1,
        "diff=true should be a no-op when diff is already set: {content}"
    );
}

#[test]
fn semantic_duplicate_unset_is_noop() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    // `-diff` and `diff=false` are semantically identical
    fs::write(&ga, "*.txt -diff\n").unwrap();
    repo.set_attr("*.txt", &["diff=false"], Some(&ga)).unwrap();

    let content = read(&ga);
    assert_eq!(
        content.lines().filter(|l| l.starts_with("*.txt")).count(),
        1,
        "diff=false should be a no-op when -diff is already set: {content}"
    );
}

#[test]
fn additive_appends_only_new_attributes() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    fs::write(&ga, "*.txt diff\n").unwrap();
    repo.set_attr("*.txt", &["diff", "filter=lfs", "-text"], Some(&ga))
        .unwrap();

    let content = read(&ga);
    // Original line preserved
    assert!(content.starts_with("*.txt diff\n"), "original line changed");
    // New attributes on a second line (only the ones that were missing)
    assert!(
        content.contains("*.txt filter=lfs -text"),
        "new attributes missing: {content}"
    );
    // `diff` must NOT appear on the second line
    let second_line = content.lines().nth(1).unwrap();
    assert!(
        !second_line.contains(" diff"),
        "diff should not be duplicated on the new line: {second_line}"
    );
}

#[test]
fn different_value_is_not_duplicate() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    fs::write(&ga, "*.txt filter=foo\n").unwrap();
    repo.set_attr("*.txt", &["filter=bar"], Some(&ga)).unwrap();

    let content = read(&ga);
    assert!(content.contains("filter=foo"), "original value missing");
    assert!(content.contains("filter=bar"), "new value missing");
}

#[test]
fn changing_state_is_not_duplicate() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    fs::write(&ga, "*.txt diff\n").unwrap();
    repo.set_attr("*.txt", &["-diff"], Some(&ga)).unwrap();

    let content = read(&ga);
    assert_eq!(
        content.lines().filter(|l| l.starts_with("*.txt")).count(),
        2,
        "set and unset are different states: {content}"
    );
}

#[test]
fn different_patterns_are_independent() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    fs::write(&ga, "*.md diff\n").unwrap();
    repo.set_attr("*.txt", &["diff"], Some(&ga)).unwrap();

    let content = read(&ga);
    assert!(content.contains("*.md diff"));
    assert!(content.contains("*.txt diff"));
}

#[test]
fn collects_existing_attributes_across_lines() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    fs::write(&ga, "*.txt diff\n*.txt filter=lfs\n*.txt -text\n").unwrap();
    repo.set_attr("*.txt", &["diff", "filter=lfs", "-text"], Some(&ga))
        .unwrap();

    let content = read(&ga);
    assert_eq!(
        content.lines().filter(|l| l.starts_with("*.txt")).count(),
        3,
        "nothing should have been added: {content}"
    );
}

#[test]
fn preserves_comments_and_blank_lines() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    let original = "# Top comment\n\n*.md text\n# Middle comment\n";
    fs::write(&ga, original).unwrap();
    repo.set_attr("*.txt", &["diff"], Some(&ga)).unwrap();

    let content = read(&ga);
    assert!(content.contains("# Top comment"));
    assert!(content.contains("# Middle comment"));
    assert!(content.contains("*.md text"));
    assert!(content.contains("*.txt diff"));
}

#[test]
fn custom_path_in_subdirectory() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();

    let sub = tmp.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let ga = sub.join(".gitattributes");

    repo.set_attr("*.bin", &["binary"], Some(&ga)).unwrap();

    assert!(ga.exists());
    assert_eq!(read(&ga).trim(), "*.bin binary");
}

#[test]
fn rejects_invalid_attributes() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    assert!(repo.set_attr("*.txt", &["has space"], Some(&ga)).is_err());
    assert!(repo.set_attr("*.txt", &["-"], Some(&ga)).is_err());
    assert!(repo.set_attr("*.txt", &["!"], Some(&ga)).is_err());
    assert!(repo.set_attr("*.txt", &["=value"], Some(&ga)).is_err());
}

#[test]
fn empty_attributes_is_noop() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    fs::write(&ga, "*.txt diff\n").unwrap();
    repo.set_attr("*.txt", &[], Some(&ga)).unwrap();

    assert_eq!(read(&ga), "*.txt diff\n", "file should be unchanged");
}

#[test]
fn multiple_calls_accumulate() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    repo.set_attr("*.txt", &["diff"], Some(&ga)).unwrap();
    repo.set_attr("*.txt", &["-text"], Some(&ga)).unwrap();
    repo.set_attr("*.txt", &["filter=lfs"], Some(&ga)).unwrap();

    let content = read(&ga);
    assert!(content.contains("diff"));
    assert!(content.contains("-text"));
    assert!(content.contains("filter=lfs"));
}

#[test]
fn idempotent_over_repeated_calls() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(&tmp).unwrap();
    let ga = tmp.path().join(".gitattributes");

    repo.set_attr("*.txt", &["diff", "filter=lfs"], Some(&ga))
        .unwrap();
    let first = read(&ga);

    repo.set_attr("*.txt", &["diff", "filter=lfs"], Some(&ga))
        .unwrap();
    let second = read(&ga);

    assert_eq!(first, second, "repeated call should be idempotent");
}
