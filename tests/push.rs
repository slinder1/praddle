use praddle_test_server::TestHarness;
use std::process::Output;

const INITIAL_COMMENT_PREFIX: &str =
    "<details>\n<summary>🛠️ Initial changes (click to expand):</summary>\n\n```diff\n";
const INITIAL_COMMENT_SUFFIX: &str = "\n```\n</details>";

fn initial_comment(path: &str, contents: &str) -> String {
    format!(
        "{INITIAL_COMMENT_PREFIX}diff --git b/{path} a/{path}\n@@ -0,0 +1 @@\n+{contents}\n{INITIAL_COMMENT_SUFFIX}"
    )
}

fn interdiff_comment(path: &str, before: &str, after: &str) -> String {
    format!(
        "<details>\n<summary>🛠️ Changes since last version (click to expand):</summary>\n\n```diff\ndiff --git b/{path} a/{path}\n@@ -1 +1 @@\n-{before}\n+{after}\n\n```\n</details>"
    )
}

fn push_output(harness: &TestHarness) -> Output {
    let mut command = harness.command(env!("CARGO_BIN_EXE_praddle"));
    command.args([
        "--remote=origin",
        "--base-branch=main",
        "--user-branch-prefix=users/alice/",
        "--serial",
        "push",
    ]);
    command.output().unwrap()
}

fn push(harness: &TestHarness) {
    let output = push_output(harness);
    assert!(
        output.status.success(),
        "praddle failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn create_pr(harness: &TestHarness, title: &str, head: &str, change_id: &str) {
    let mut command = harness.command("gh");
    command.args([
        "pr",
        "create",
        "--repo=https://github.com/alice/widgets",
        "--draft",
        "--base=main",
        &format!("--head={head}"),
        &format!("--title={title}"),
        &format!("--body=Change-Id: {change_id}"),
    ]);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "gh pr create failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(harness: &TestHarness, args: &[&str]) -> String {
    String::from_utf8(harness.git(args).unwrap().stdout)
        .unwrap()
        .trim()
        .to_owned()
}

fn local_refs(harness: &TestHarness) -> String {
    git_stdout(
        harness,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn ignores_duplicate_change_ids_outside_the_current_stack() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    harness
        .git([
            "push",
            "--atomic",
            "origin",
            "HEAD:refs/heads/unrelated-one",
            "HEAD:refs/heads/unrelated-two",
        ])
        .unwrap();
    create_pr(&harness, "Unrelated one", "unrelated-one", "I9999");
    create_pr(&harness, "Unrelated two", "unrelated-two", "I9999");

    harness.write("change", "change\n").unwrap();
    harness.git(["add", "change"]).unwrap();
    harness
        .git(["commit", "-m", "Current change", "-m", "Change-Id: I0001"])
        .unwrap();

    push(&harness);

    let snapshot = harness.snapshot();
    assert_eq!(snapshot.pull_requests.len(), 3);
    assert!(snapshot.stacks.is_empty());
    assert_eq!(snapshot.pull_requests[2].title, "Current change");
    assert_eq!(snapshot.pull_requests[2].stack, None);
    assert_eq!(snapshot.pull_requests[2].stack_position, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn creates_a_stack_when_two_changes_are_pushed() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    for (path, title, change_id) in [
        ("first", "First change", "I0001"),
        ("second", "Second change", "I0002"),
    ] {
        harness.write(path, format!("{path}\n")).unwrap();
        harness.git(["add", path]).unwrap();
        harness
            .git([
                "commit",
                "-m",
                title,
                "-m",
                &format!("Change-Id: {change_id}"),
            ])
            .unwrap();
    }

    push(&harness);

    let snapshot = harness.snapshot();
    assert_eq!(snapshot.stacks, [(1, vec![2, 1])].into());
    let first = snapshot
        .pull_requests
        .iter()
        .find(|pr| pr.title == "First change")
        .unwrap();
    assert_eq!(first.stack, Some(1));
    assert_eq!(first.stack_position, Some(1));
    let second = snapshot
        .pull_requests
        .iter()
        .find(|pr| pr.title == "Second change")
        .unwrap();
    assert_eq!(second.stack, Some(1));
    assert_eq!(second.stack_position, Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn extends_a_stack_when_another_change_is_pushed() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    for (path, title, body, change_id) in [
        ("first", "First change", "First body", "I0001"),
        ("second", "Second change", "Second body", "I0002"),
    ] {
        harness.write(path, format!("{path}\n")).unwrap();
        harness.git(["add", path]).unwrap();
        harness
            .git([
                "commit",
                "-m",
                title,
                "-m",
                body,
                "-m",
                &format!("Change-Id: {change_id}"),
            ])
            .unwrap();
    }

    push(&harness);

    let snapshot = harness.snapshot();
    assert_eq!(snapshot.stacks, [(1, vec![2, 1])].into());
    assert_eq!(snapshot.pull_requests.len(), 2);

    harness.write("third", "third\n").unwrap();
    harness.git(["add", "third"]).unwrap();
    harness
        .git([
            "commit",
            "-m",
            "Third change",
            "-m",
            "Third body",
            "-m",
            "Change-Id: I0003",
        ])
        .unwrap();

    push(&harness);

    let snapshot = harness.snapshot();
    assert_eq!(snapshot.stacks, [(1, vec![2, 1, 3])].into());
    assert_eq!(snapshot.pull_requests.len(), 3);
    for (title, base, position) in [
        ("First change", "main", 1),
        ("Second change", "users/alice/I0001", 2),
        ("Third change", "users/alice/I0002", 3),
    ] {
        let pr = snapshot
            .pull_requests
            .iter()
            .find(|pr| pr.title == title)
            .unwrap();
        assert_eq!(pr.base_ref_name, base);
        assert_eq!(pr.stack, Some(1));
        assert_eq!(pr.stack_position, Some(position));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn extends_a_stack_with_two_changes_in_one_push() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    for (path, title, change_id) in [
        ("first", "First change", "I0001"),
        ("second", "Second change", "I0002"),
    ] {
        harness.write(path, format!("{path}\n")).unwrap();
        harness.git(["add", path]).unwrap();
        harness
            .git([
                "commit",
                "-m",
                title,
                "-m",
                &format!("Change-Id: {change_id}"),
            ])
            .unwrap();
    }

    push(&harness);

    let snapshot = harness.snapshot();
    assert_eq!(snapshot.stacks, [(1, vec![2, 1])].into());
    assert_eq!(snapshot.pull_requests.len(), 2);

    harness.write("third", "third\n").unwrap();
    harness.git(["add", "third"]).unwrap();
    harness
        .git([
            "commit",
            "-m",
            "Third change",
            "-m",
            "Third body",
            "-m",
            "Change-Id: I0003",
        ])
        .unwrap();
    harness.write("fourth", "fourth\n").unwrap();
    harness.git(["add", "fourth"]).unwrap();
    harness
        .git([
            "commit",
            "-m",
            "Fourth change",
            "-m",
            "Fourth body",
            "-m",
            "Change-Id: I0004",
        ])
        .unwrap();

    push(&harness);

    let snapshot = harness.snapshot();
    assert_eq!(snapshot.stacks, [(1, vec![2, 1, 4, 3])].into());
    assert_eq!(snapshot.pull_requests.len(), 4);
    for (title, base, position) in [
        ("First change", "main", 1),
        ("Second change", "users/alice/I0001", 2),
        ("Third change", "users/alice/I0002", 3),
        ("Fourth change", "users/alice/I0003", 4),
    ] {
        let pr = snapshot
            .pull_requests
            .iter()
            .find(|pr| pr.title == title)
            .unwrap();
        assert_eq!(pr.base_ref_name, base);
        assert_eq!(pr.stack, Some(1));
        assert_eq!(pr.stack_position, Some(position));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn leaves_an_unchanged_stack_alone() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    harness.write("first", "first\n").unwrap();
    harness.git(["add", "first"]).unwrap();
    harness
        .git(["commit", "-m", "First", "-m", "Change-Id: I0001"])
        .unwrap();
    harness.write("second", "second\n").unwrap();
    harness.git(["add", "second"]).unwrap();
    harness
        .git(["commit", "-m", "Second", "-m", "Change-Id: I0002"])
        .unwrap();
    push(&harness);

    let before = harness.snapshot();
    let first_tip = harness
        .remote_ref_oid("refs/heads/users/alice/I0001")
        .unwrap();
    let second_tip = harness
        .remote_ref_oid("refs/heads/users/alice/I0002")
        .unwrap();

    push(&harness);

    let after = harness.snapshot();
    assert_eq!(after.stacks, before.stacks);
    for old in &before.pull_requests {
        let new = after
            .pull_requests
            .iter()
            .find(|pr| pr.number == old.number)
            .unwrap();
        assert_eq!(new.base_ref_name, old.base_ref_name);
        assert_eq!(new.base_ref_history, old.base_ref_history);
        assert_eq!(new.comments, old.comments);
        assert_eq!(new.stack_position, old.stack_position);
    }
    assert_eq!(
        harness
            .remote_ref_oid("refs/heads/users/alice/I0001")
            .unwrap(),
        first_tip
    );
    assert_eq!(
        harness
            .remote_ref_oid("refs/heads/users/alice/I0002")
            .unwrap(),
        second_tip
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn restores_remote_refs_when_pr_reconciliation_fails() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    harness.write("change", "before\n").unwrap();
    harness.git(["add", "change"]).unwrap();
    harness
        .git(["commit", "-m", "Change", "-m", "Change-Id: I0001"])
        .unwrap();
    push(&harness);
    let old_head = harness
        .remote_ref_oid("refs/heads/users/alice/I0001")
        .unwrap()
        .unwrap();

    harness
        .git(["push", "origin", "refs/heads/main:refs/heads/auxiliary"])
        .unwrap();
    let mut edit = harness.command("gh");
    edit.args([
        "pr",
        "edit",
        "1",
        "--repo=https://github.com/alice/widgets",
        "--base=auxiliary",
    ]);
    assert!(edit.output().unwrap().status.success());

    harness.write("change", "after\n").unwrap();
    harness.git(["add", "change"]).unwrap();
    harness
        .git([
            "commit",
            "--amend",
            "-m",
            "Change",
            "-m",
            "Change-Id: I0001",
        ])
        .unwrap();

    harness.server().fail_next_base_update();
    let failed = push_output(&harness);
    assert!(!failed.status.success());
    assert_eq!(
        harness
            .remote_ref_oid("refs/heads/users/alice/I0001")
            .unwrap(),
        Some(old_head.clone())
    );

    push(&harness);

    let new_head = harness
        .remote_ref_oid("refs/heads/users/alice/I0001")
        .unwrap()
        .unwrap();
    assert!(harness.is_ancestor(&old_head, &new_head).unwrap());
    let snapshot = harness.snapshot();
    assert_eq!(snapshot.pull_requests[0].base_ref_name, "main");
    assert_eq!(
        snapshot.pull_requests[0].comments,
        [
            initial_comment("change", "before"),
            interdiff_comment("change", "before", "after"),
        ]
    );
    assert!(snapshot.stacks.is_empty());
    assert_eq!(snapshot.pull_requests[0].stack, None);
    assert_eq!(snapshot.pull_requests[0].stack_position, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn updates_a_change_when_it_is_edited_and_pushed_again() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    harness.write("change", "before\n").unwrap();
    harness.git(["add", "change"]).unwrap();
    harness
        .git([
            "commit",
            "-m",
            "Initial title",
            "-m",
            "Initial body",
            "-m",
            "Change-Id: I0001",
        ])
        .unwrap();

    let refs_before_push = local_refs(&harness);
    let mut expected_remote_refs = harness.remote_branch_refs().unwrap();
    assert!(expected_remote_refs.insert("refs/heads/users/alice/I0001".into()));
    push(&harness);
    assert_eq!(local_refs(&harness), refs_before_push);
    assert_eq!(harness.remote_branch_refs().unwrap(), expected_remote_refs);

    let snapshot = harness.snapshot();
    assert!(snapshot.stacks.is_empty());
    assert_eq!(snapshot.pull_requests.len(), 1);
    let change = &snapshot.pull_requests[0];
    assert_eq!(change.title, "Initial title");
    assert_eq!(change.body, "Initial body\n\nChange-Id: I0001");
    assert_eq!(change.base_ref_name, "main");
    assert_eq!(change.head_ref_name, "refs/heads/users/alice/I0001");
    assert_eq!(change.comments, [initial_comment("change", "before")]);
    assert_eq!(change.stack, None);
    assert_eq!(change.stack_position, None);
    let old_remote_head = harness
        .remote_ref_oid("refs/heads/users/alice/I0001")
        .unwrap()
        .unwrap();
    let remote_base = harness.remote_ref_oid("refs/heads/main").unwrap().unwrap();
    assert_eq!(
        harness.commit_parent_oids(&old_remote_head).unwrap(),
        [remote_base]
    );
    assert_eq!(
        harness.commit_tree_oid(&old_remote_head).unwrap(),
        git_stdout(&harness, &["rev-parse", "HEAD^{tree}"])
    );
    assert_eq!(
        harness.commit_message(&old_remote_head).unwrap(),
        "synthetic-praddle-commit"
    );

    harness.write("change", "after\n").unwrap();
    harness.git(["add", "change"]).unwrap();
    harness
        .git([
            "commit",
            "--amend",
            "-m",
            "Updated title",
            "-m",
            "Updated body",
            "-m",
            "Change-Id: I0001",
        ])
        .unwrap();

    let refs_before_update = local_refs(&harness);
    push(&harness);
    assert_eq!(local_refs(&harness), refs_before_update);

    let snapshot = harness.snapshot();
    assert!(snapshot.stacks.is_empty());
    assert_eq!(snapshot.pull_requests.len(), 1);
    let change = &snapshot.pull_requests[0];
    assert_eq!(change.title, "Updated title");
    assert_eq!(change.body, "Updated body\n\nChange-Id: I0001");
    assert_eq!(change.base_ref_name, "main");
    assert_eq!(change.head_ref_name, "refs/heads/users/alice/I0001");
    assert_eq!(
        change.comments,
        [
            initial_comment("change", "before"),
            interdiff_comment("change", "before", "after"),
        ]
    );
    assert_eq!(change.stack, None);
    assert_eq!(change.stack_position, None);
    let new_remote_head = harness
        .remote_ref_oid("refs/heads/users/alice/I0001")
        .unwrap()
        .unwrap();
    assert!(
        harness
            .is_ancestor(&old_remote_head, &new_remote_head)
            .unwrap()
    );
    assert_eq!(
        harness.commit_parent_oids(&new_remote_head).unwrap()[0],
        old_remote_head
    );
    assert_eq!(
        harness.commit_tree_oid(&new_remote_head).unwrap(),
        git_stdout(&harness, &["rev-parse", "HEAD^{tree}"])
    );
    assert_eq!(
        harness.commit_message(&new_remote_head).unwrap(),
        "synthetic-praddle-commit"
    );

    push(&harness);
    assert_eq!(
        harness
            .remote_ref_oid("refs/heads/users/alice/I0001")
            .unwrap(),
        Some(new_remote_head)
    );
    assert_eq!(harness.snapshot().pull_requests[0].comments.len(), 2);
    assert_eq!(local_refs(&harness), refs_before_update);
}

#[tokio::test(flavor = "multi_thread")]
async fn updates_descendants_with_fast_forward_merges() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    harness.write("first", "before\n").unwrap();
    harness.git(["add", "first"]).unwrap();
    harness
        .git(["commit", "-m", "First", "-m", "Change-Id: I0001"])
        .unwrap();
    harness.write("second", "second\n").unwrap();
    harness.git(["add", "second"]).unwrap();
    harness
        .git(["commit", "-m", "Second", "-m", "Change-Id: I0002"])
        .unwrap();
    push(&harness);

    let old_first = harness
        .remote_ref_oid("refs/heads/users/alice/I0001")
        .unwrap()
        .unwrap();
    let old_second = harness
        .remote_ref_oid("refs/heads/users/alice/I0002")
        .unwrap()
        .unwrap();

    harness.git(["reset", "--hard", "HEAD~2"]).unwrap();
    harness.write("first", "after\n").unwrap();
    harness.git(["add", "first"]).unwrap();
    harness
        .git(["commit", "-m", "First", "-m", "Change-Id: I0001"])
        .unwrap();
    harness.write("second", "second\n").unwrap();
    harness.git(["add", "second"]).unwrap();
    harness
        .git(["commit", "-m", "Second", "-m", "Change-Id: I0002"])
        .unwrap();
    let first_tree = git_stdout(&harness, &["rev-parse", "HEAD^^{tree}"]);
    let second_tree = git_stdout(&harness, &["rev-parse", "HEAD^{tree}"]);
    let refs_before_push = local_refs(&harness);

    push(&harness);

    assert_eq!(local_refs(&harness), refs_before_push);
    let new_first = harness
        .remote_ref_oid("refs/heads/users/alice/I0001")
        .unwrap()
        .unwrap();
    let new_second = harness
        .remote_ref_oid("refs/heads/users/alice/I0002")
        .unwrap()
        .unwrap();
    assert!(harness.is_ancestor(&old_first, &new_first).unwrap());
    assert!(harness.is_ancestor(&old_second, &new_second).unwrap());
    assert_eq!(harness.commit_parent_oids(&new_first).unwrap(), [old_first]);
    assert_eq!(
        harness.commit_parent_oids(&new_second).unwrap(),
        [old_second, new_first.clone()]
    );
    assert_eq!(harness.commit_tree_oid(&new_first).unwrap(), first_tree);
    assert_eq!(harness.commit_tree_oid(&new_second).unwrap(), second_tree);

    let snapshot = harness.snapshot();
    assert_eq!(snapshot.stacks, [(1, vec![2, 1])].into());
    let first = snapshot
        .pull_requests
        .iter()
        .find(|pr| pr.title == "First")
        .unwrap();
    let second = snapshot
        .pull_requests
        .iter()
        .find(|pr| pr.title == "Second")
        .unwrap();
    assert_eq!(first.comments.len(), 2);
    assert_eq!(second.comments.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn reorders_existing_changes_with_fast_forward_merges() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    harness.write("first", "first\n").unwrap();
    harness.git(["add", "first"]).unwrap();
    harness
        .git(["commit", "-m", "First", "-m", "Change-Id: I0001"])
        .unwrap();
    harness.write("second", "second\n").unwrap();
    harness.git(["add", "second"]).unwrap();
    harness
        .git(["commit", "-m", "Second", "-m", "Change-Id: I0002"])
        .unwrap();
    push(&harness);

    let old_first = harness
        .remote_ref_oid("refs/heads/users/alice/I0001")
        .unwrap()
        .unwrap();
    let old_second = harness
        .remote_ref_oid("refs/heads/users/alice/I0002")
        .unwrap()
        .unwrap();

    harness.git(["reset", "--hard", "HEAD~2"]).unwrap();
    harness.write("second", "second\n").unwrap();
    harness.git(["add", "second"]).unwrap();
    harness
        .git(["commit", "-m", "Second", "-m", "Change-Id: I0002"])
        .unwrap();
    harness.write("first", "first\n").unwrap();
    harness.git(["add", "first"]).unwrap();
    harness
        .git(["commit", "-m", "First", "-m", "Change-Id: I0001"])
        .unwrap();
    let refs_before_push = local_refs(&harness);

    push(&harness);

    assert_eq!(local_refs(&harness), refs_before_push);
    let new_first = harness
        .remote_ref_oid("refs/heads/users/alice/I0001")
        .unwrap()
        .unwrap();
    let new_second = harness
        .remote_ref_oid("refs/heads/users/alice/I0002")
        .unwrap()
        .unwrap();
    assert!(harness.is_ancestor(&old_first, &new_first).unwrap());
    assert!(harness.is_ancestor(&old_second, &new_second).unwrap());
    assert_eq!(
        harness.commit_parent_oids(&new_first).unwrap(),
        [old_first, new_second.clone()]
    );
    assert_eq!(
        harness.commit_parent_oids(&new_second).unwrap(),
        [old_second]
    );

    let snapshot = harness.snapshot();
    assert_eq!(snapshot.stacks, [(2, vec![1, 2])].into());
    let first = snapshot
        .pull_requests
        .iter()
        .find(|pr| pr.title == "First")
        .unwrap();
    let second = snapshot
        .pull_requests
        .iter()
        .find(|pr| pr.title == "Second")
        .unwrap();
    assert_eq!(first.state, "OPEN");
    assert_eq!(first.base_ref_name, "users/alice/I0002");
    assert_eq!(second.state, "OPEN");
    assert_eq!(second.base_ref_name, "main");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_temporarily_retargets_changes_moving_earlier() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    for (path, title, change_id) in [
        ("first", "First", "I0001"),
        ("second", "Second", "I0002"),
        ("third", "Third", "I0003"),
    ] {
        harness.write(path, format!("{path}\n")).unwrap();
        harness.git(["add", path]).unwrap();
        harness
            .git([
                "commit",
                "-m",
                title,
                "-m",
                &format!("Change-Id: {change_id}"),
            ])
            .unwrap();
    }
    push(&harness);
    let before = harness.snapshot();

    harness.git(["reset", "--hard", "HEAD~3"]).unwrap();
    for (path, title, change_id) in [
        ("first", "First", "I0001"),
        ("third", "Third", "I0003"),
        ("second", "Second", "I0002"),
    ] {
        harness.write(path, format!("{path}\n")).unwrap();
        harness.git(["add", path]).unwrap();
        harness
            .git([
                "commit",
                "-m",
                title,
                "-m",
                &format!("Change-Id: {change_id}"),
            ])
            .unwrap();
    }

    push(&harness);

    let after = harness.snapshot();
    assert_eq!(after.stacks, [(2, vec![3, 1, 2])].into());
    for title in ["First", "Second", "Third"] {
        let old = before
            .pull_requests
            .iter()
            .find(|pr| pr.title == title)
            .unwrap();
        let new = after
            .pull_requests
            .iter()
            .find(|pr| pr.title == title)
            .unwrap();
        let added_bases = &new.base_ref_history[old.base_ref_history.len()..];
        match title {
            "First" => assert!(added_bases.is_empty()),
            "Second" => assert_eq!(added_bases, ["users/alice/I0003"]),
            "Third" => assert_eq!(added_bases, ["main", "users/alice/I0001"]),
            _ => unreachable!(),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unstacks_before_inserting_a_change_between_existing_prs() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    for (path, title, change_id) in [("first", "First", "I0001"), ("third", "Third", "I0003")] {
        harness.write(path, format!("{path}\n")).unwrap();
        harness.git(["add", path]).unwrap();
        harness
            .git([
                "commit",
                "-m",
                title,
                "-m",
                &format!("Change-Id: {change_id}"),
            ])
            .unwrap();
    }
    push(&harness);
    let before = harness.snapshot();

    harness.git(["reset", "--hard", "HEAD~2"]).unwrap();
    for (path, title, change_id) in [
        ("first", "First", "I0001"),
        ("second", "Second", "I0002"),
        ("third", "Third", "I0003"),
    ] {
        harness.write(path, format!("{path}\n")).unwrap();
        harness.git(["add", path]).unwrap();
        harness
            .git([
                "commit",
                "-m",
                title,
                "-m",
                &format!("Change-Id: {change_id}"),
            ])
            .unwrap();
    }

    push(&harness);

    let after = harness.snapshot();
    assert_eq!(after.stacks, [(2, vec![2, 3, 1])].into());
    let old_first = before
        .pull_requests
        .iter()
        .find(|pr| pr.title == "First")
        .unwrap();
    let old_third = before
        .pull_requests
        .iter()
        .find(|pr| pr.title == "Third")
        .unwrap();
    let first = after
        .pull_requests
        .iter()
        .find(|pr| pr.title == "First")
        .unwrap();
    let second = after
        .pull_requests
        .iter()
        .find(|pr| pr.title == "Second")
        .unwrap();
    let third = after
        .pull_requests
        .iter()
        .find(|pr| pr.title == "Third")
        .unwrap();
    assert_eq!(first.base_ref_history, old_first.base_ref_history);
    assert_eq!(second.base_ref_history, ["main", "users/alice/I0001"]);
    assert_eq!(
        &third.base_ref_history[old_third.base_ref_history.len()..],
        ["users/alice/I0002"]
    );
}

async fn assert_push_mode(dry_run: bool, verbosity: u8) {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    harness.write("feature", "content\n").unwrap();
    harness.git(["add", "feature"]).unwrap();
    harness
        .git(["commit", "-m", "Add feature", "-m", "Change-Id: I0001"])
        .unwrap();

    let mut command = harness.command(env!("CARGO_BIN_EXE_praddle"));
    command.args([
        "--remote=origin",
        "--base-branch=main",
        "--user-branch-prefix=users/alice/",
        "--serial",
    ]);
    if dry_run {
        command.arg("--dry-run");
    }
    command.arg(format!("--verbose={verbosity}"));
    command.arg("push");

    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "praddle failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.contains("would-exec:"),
        dry_run,
        "dry_run={dry_run}, verbosity={verbosity}\nstderr:\n{stderr}"
    );
    let echoes_commands = dry_run || verbosity > 0;
    assert!(
        !stderr.contains("file-contents-"),
        "dry_run={dry_run}, verbosity={verbosity}\nstderr:\n{stderr}"
    );
    assert_eq!(
        stderr.contains("Change-Id: I0001"),
        echoes_commands,
        "dry_run={dry_run}, verbosity={verbosity}\nstderr:\n{stderr}"
    );
    let snapshot = harness.snapshot();
    assert_eq!(
        snapshot.pull_requests.is_empty(),
        dry_run,
        "dry_run={dry_run}, verbosity={verbosity}"
    );
    assert!(snapshot.stacks.is_empty());
    for pr in snapshot.pull_requests {
        assert_eq!(pr.stack, None);
        assert_eq!(pr.stack_position, None);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn quiet_run_executes_without_echoing() {
    assert_push_mode(false, 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn verbose_run_executes_and_echoes() {
    assert_push_mode(false, 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn very_verbose_run_executes_and_echoes() {
    assert_push_mode(false, 2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn quiet_dry_run_skips_execution_and_echoes() {
    assert_push_mode(true, 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn verbose_dry_run_skips_execution_and_echoes() {
    assert_push_mode(true, 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn very_verbose_dry_run_skips_execution_and_echoes() {
    assert_push_mode(true, 2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn misc() {
    let harness = TestHarness::start("alice", "widgets").await.unwrap();
    harness.write("feature", "content\n").unwrap();
    harness.git(["add", "feature"]).unwrap();
    harness
        .git(["commit", "-m", "Add feature", "-m", "Change-Id: I0001"])
        .unwrap();

    let mut command = harness.command(env!("CARGO_BIN_EXE_praddle"));
    command.args([
        "--remote=origin",
        "--base-branch=main",
        "--user-branch-prefix=users/alice/",
        "--serial",
        "--verbose=2",
        "push",
    ]);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "praddle failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let snapshot = harness.snapshot();
    assert_eq!(snapshot.pull_requests.len(), 1);
    let pr = &snapshot.pull_requests[0];
    assert_eq!(pr.title, "Add feature");
    assert!(pr.body.contains("Change-Id: I0001"));
    assert_eq!(pr.head_ref_name, "refs/heads/users/alice/I0001");
    assert!(!pr.is_draft);
    assert_eq!(pr.comments.len(), 1);
    assert!(snapshot.stacks.is_empty());
    assert_eq!(pr.stack, None);
    assert_eq!(pr.stack_position, None);

    harness.write("feature", "updated\n").unwrap();
    harness
        .write(
        "praddle-test.toml",
        "remote = \"origin\"\nbase_branch = \"main\"\nuser_branch_prefix = \"users/alice/\"\n\n[reviewer_groups]\ntest = [\"bob\"]\n",
    )
    .unwrap();
    harness.git(["add", "feature"]).unwrap();
    harness
        .git([
            "commit",
            "--amend",
            "-m",
            "Update feature",
            "-m",
            "Change-Id: I0001",
        ])
        .unwrap();
    let mut command = harness.command(env!("CARGO_BIN_EXE_praddle"));
    command
        .env(
            "PRADDLE_CONFIG_PATH",
            harness.worktree().join("praddle-test.toml"),
        )
        .args([
            "--remote=origin",
            "--base-branch=main",
            "--user-branch-prefix=users/alice/",
            "--serial",
            "--verbose=2",
            "push",
            "--reviewer-groups=test",
        ]);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "second praddle run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot = harness.snapshot();
    assert_eq!(snapshot.pull_requests.len(), 1);
    assert_eq!(snapshot.pull_requests[0].title, "Update feature");
    assert_eq!(snapshot.pull_requests[0].comments.len(), 2);
    assert_eq!(snapshot.pull_requests[0].reviewers, ["bob"]);

    assert!(
        harness
            .remote_ref_exists("refs/heads/users/alice/I0001")
            .unwrap()
    );
}
