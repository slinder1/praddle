use praddle_test_server::TestHarness;
use serde_json::{Value, json};
use std::{io::Write, process::Output};
use tempfile::NamedTempFile;

fn post(harness: &TestHarness, endpoint: &str, request: &Value) -> Output {
    let mut input = NamedTempFile::new().unwrap();
    serde_json::to_writer(&mut input, request).unwrap();
    input.flush().unwrap();
    harness
        .command("gh")
        .args([
            "api",
            "--method=POST",
            endpoint,
            &format!("--input={}", input.path().display()),
        ])
        .output()
        .unwrap()
}

async fn check_comment_cap(endpoint: &str) {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    let output = post(
        &harness,
        "graphql",
        &json!({
            "query": "mutation($input: CreatePullRequestInput!) { createPullRequest(input: $input) { pullRequest { id } } }",
            "variables": { "input": {
                "repositoryId": "REPOSITORY_1",
                "title": "Test pull request",
                "baseRefName": "main",
                "headRefName": "change"
            } }
        }),
    );
    assert!(output.status.success(), "{output:?}");

    let body = "\u{1f980}".repeat(65_536 / "\u{1f980}".len());
    for body in [body.clone(), format!("{body}x")] {
        let before = serde_json::to_value(harness.snapshot()).unwrap();
        let request = if endpoint == "graphql" {
            json!({
                "query": "mutation($input: AddCommentInput!) { addComment(input: $input) { commentEdge { node { url } } } }",
                "variables": { "input": { "subjectId": "PR_1", "body": body } }
            })
        } else {
            json!({ "body": body })
        };
        let output = post(&harness, endpoint, &request);
        if body.len() == 65_536 {
            assert!(output.status.success(), "{output:?}");
            assert_eq!(harness.snapshot().pull_requests[0].comments, vec![body]);
        } else {
            assert!(!output.status.success(), "oversized comment succeeded");
            assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP 422"));
            let response: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(response["message"], "comment body exceeds 65536 bytes");
            assert_eq!(serde_json::to_value(harness.snapshot()).unwrap(), before);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn graphql_comment_cap_counts_utf8_bytes_without_mutating_on_failure() {
    check_comment_cap("graphql").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rest_comment_cap_counts_utf8_bytes_without_mutating_on_failure() {
    check_comment_cap("repos/alice/widgets/issues/1/comments").await;
}
