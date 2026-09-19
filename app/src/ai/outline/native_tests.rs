use super::*;

#[test]
fn repo_recency_evicts_the_least_recently_used_repo() {
    let mut recency = RepoRecency::default();

    assert_eq!(recency.touch(Path::new("/repo/one")), None);
    assert_eq!(recency.touch(Path::new("/repo/two")), None);
    assert_eq!(recency.touch(Path::new("/repo/three")), None);
    assert_eq!(recency.touch(Path::new("/repo/one")), None);
    assert_eq!(
        recency.touch(Path::new("/repo/four")),
        Some(PathBuf::from("/repo/two"))
    );
    assert_eq!(
        recency.paths,
        VecDeque::from([
            PathBuf::from("/repo/three"),
            PathBuf::from("/repo/one"),
            PathBuf::from("/repo/four"),
        ])
    );
}
