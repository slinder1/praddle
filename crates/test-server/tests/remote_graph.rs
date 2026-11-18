use praddle_test_server::TestHarness;

#[tokio::test(flavor = "multi_thread")]
async fn inspects_the_bare_remote_graph() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    let base = harness.remote_ref_oid("refs/heads/main").unwrap().unwrap();

    assert_eq!(harness.remote_ref_oid("refs/heads/missing").unwrap(), None);
    assert_eq!(
        harness.commit_parent_oids(&base).unwrap(),
        Vec::<String>::new()
    );
    assert!(!harness.commit_tree_oid(&base).unwrap().is_empty());

    harness.write("feature", "initial\n").unwrap();
    harness.git(["add", "feature"]).unwrap();
    harness.git(["commit", "-m", "Add feature"]).unwrap();
    harness
        .git(["push", "origin", "HEAD:refs/heads/change"])
        .unwrap();
    let change = harness
        .remote_ref_oid("refs/heads/change")
        .unwrap()
        .unwrap();

    assert_eq!(
        harness.commit_parent_oids(&change).unwrap(),
        vec![base.clone()]
    );
    assert!(harness.is_ancestor(&base, &change).unwrap());
    assert!(!harness.is_ancestor(&change, &base).unwrap());
    assert!(harness.is_ancestor("missing", &base).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_divergent_updates_but_allows_inverse_fast_forwards() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    harness.write("feature", "initial\n").unwrap();
    harness.git(["add", "feature"]).unwrap();
    harness.git(["commit", "-m", "Add feature"]).unwrap();
    harness
        .git(["push", "origin", "HEAD:refs/heads/change"])
        .unwrap();
    let change = harness
        .remote_ref_oid("refs/heads/change")
        .unwrap()
        .unwrap();

    harness.git(["reset", "--hard", "HEAD^"]).unwrap();
    harness.write("replacement", "replacement\n").unwrap();
    harness.git(["add", "replacement"]).unwrap();
    harness.git(["commit", "-m", "Replace feature"]).unwrap();
    assert!(
        harness
            .git(["push", "--force", "origin", "HEAD:refs/heads/change"])
            .is_err()
    );
    assert_eq!(
        harness.remote_ref_oid("refs/heads/change").unwrap(),
        Some(change)
    );

    harness
        .git(["push", "--force", "origin", "main:refs/heads/change"])
        .unwrap();
    assert_eq!(
        harness.remote_ref_oid("refs/heads/change").unwrap(),
        harness.remote_ref_oid("refs/heads/main").unwrap()
    );

    harness
        .git(["push", "origin", "HEAD:refs/heads/new-branch"])
        .unwrap();
    assert!(
        harness
            .git(["push", "origin", ":refs/heads/new-branch"])
            .is_ok()
    );
    assert!(!harness.remote_ref_exists("refs/heads/new-branch").unwrap());
}
