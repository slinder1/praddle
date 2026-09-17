use praddle_test_server::TestHarness;
use std::{io::Write, process::Output};
use tempfile::NamedTempFile;

fn create_pr(harness: &TestHarness, title: &str, head: &str) {
    let output = harness
        .command("gh")
        .args([
            "pr",
            "create",
            "--repo=https://github.com/alice/widgets",
            "--base=main",
            &format!("--head={head}"),
            &format!("--title={title}"),
            "--body=Test pull request",
        ])
        .output()
        .unwrap();
    assert_success("gh pr create", &output);
}

fn create_stack(harness: &TestHarness, pull_requests: &[u64]) -> Output {
    let mut input = NamedTempFile::new().unwrap();
    serde_json::to_writer(
        &mut input,
        &serde_json::json!({ "pull_requests": pull_requests }),
    )
    .unwrap();
    input.flush().unwrap();
    harness
        .command("gh")
        .args([
            "api",
            "--method=POST",
            "repos/alice/widgets/stacks",
            &format!("--input={}", input.path().display()),
        ])
        .output()
        .unwrap()
}

fn assert_success(command: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{command} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_stack_with_fewer_than_two_pull_requests() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    create_pr(&harness, "First", "first");

    let output = create_stack(&harness, &[1]);

    assert!(!output.status.success());
    assert!(harness.snapshot().stacks.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn creates_a_stack_with_two_pull_requests() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    create_pr(&harness, "First", "first");
    create_pr(&harness, "Second", "second");

    let output = create_stack(&harness, &[1, 2]);

    assert_success("gh api stacks", &output);
    assert_eq!(harness.snapshot().stacks, [(1, vec![1, 2])].into());
}
