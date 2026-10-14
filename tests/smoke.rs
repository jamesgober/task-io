//! Smoke test: the crate links and exposes its version from outside.

#[test]
fn test_crate_version_is_semver() {
    let parts: Vec<&str> = task_io::VERSION.split('.').collect();
    assert_eq!(parts.len(), 3, "expected MAJOR.MINOR.PATCH");
    for part in parts {
        assert!(
            part.parse::<u64>().is_ok(),
            "non-numeric version part: {part}"
        );
    }
}
