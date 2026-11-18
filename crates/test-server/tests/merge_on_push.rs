use praddle_test_server::TestHarness;

#[tokio::test(flavor = "multi_thread")]
async fn marks_a_pull_request_merged_when_its_head_is_reachable_from_its_base() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    harness.write("feature", "initial\n").unwrap();
    harness.git(["add", "feature"]).unwrap();
    harness.git(["commit", "-m", "Add feature"]).unwrap();
    harness
        .git(["push", "origin", "HEAD:refs/heads/change"])
        .unwrap();

    let mut create = harness.command("gh");
    create.args([
        "pr",
        "create",
        "--repo=https://github.com/alice/widgets",
        "--draft",
        "--base=main",
        "--head=refs/heads/change",
        "--title=Add feature",
        "--body=Test pull request",
    ]);
    let output = create.output().unwrap();
    assert!(
        output.status.success(),
        "gh pr create failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(harness.snapshot().pull_requests[0].state, "OPEN");

    harness.write("feature", "updated\n").unwrap();
    harness.git(["add", "feature"]).unwrap();
    harness.git(["commit", "-m", "Update feature"]).unwrap();
    harness
        .git(["push", "origin", "HEAD:refs/heads/change"])
        .unwrap();
    assert_eq!(harness.snapshot().pull_requests[0].state, "OPEN");

    harness.git(["checkout", "main"]).unwrap();
    harness.git(["merge", "--ff-only", "change"]).unwrap();
    let output = harness.git(["push", "origin", "main"]).unwrap();
    assert_eq!(
        harness.snapshot().pull_requests[0].state,
        "MERGED",
        "git push stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
