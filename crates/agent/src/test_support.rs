//! Filesystem fixtures shared by agent unit tests, including offline sandboxes.

use std::path::PathBuf;

/// Keep fixtures in the workspace's writable build tree. macOS's system temp
/// directory is protected by path policy, and a build sandbox cannot write to
/// the host's /tmp. Callers allocate unique children (TempDir or UUID).
pub(crate) fn fixture_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-runtime");
    std::fs::create_dir_all(&root).expect("create test fixture root");
    root.canonicalize().expect("canonical test fixture root")
}

#[test]
fn fixtures_are_unique_and_confined_to_workspace_target() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let root = fixture_root();
    assert_eq!(
        root,
        workspace
            .join("target/test-runtime")
            .canonicalize()
            .unwrap()
    );
    let first = tempfile::tempdir_in(&root).unwrap();
    let second = tempfile::tempdir_in(&root).unwrap();
    assert_ne!(first.path(), second.path());
    assert!(first.path().starts_with(&root));
    std::fs::write(first.path().join("fixture"), "isolated").unwrap();
    assert!(!second.path().join("fixture").exists());
}
