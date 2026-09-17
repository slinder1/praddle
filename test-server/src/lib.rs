use anyhow::{Context, Result, bail};
use axum::{
    Router,
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, Mutex},
};
use tempfile::TempDir;
use tokio::{net::UnixListener, sync::oneshot, task::JoinHandle};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub base_ref_name: String,
    pub base_ref_history: Vec<String>,
    pub head_ref_name: String,
    pub is_draft: bool,
    pub reviewers: Vec<String>,
    pub comments: Vec<String>,
    pub stack: Option<u64>,
    pub stack_position: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Snapshot {
    pub owner: String,
    pub repository: String,
    pub pull_requests: Vec<PullRequest>,
    pub stacks: BTreeMap<u64, Vec<u64>>,
}

struct Model {
    next_pr: u64,
    next_stack: u64,
    fail_next_base_update: bool,
    pull_requests: BTreeMap<u64, PullRequest>,
    stacks: BTreeMap<u64, Vec<u64>>,
}

#[derive(Clone)]
struct AppState {
    owner: String,
    repository: String,
    git_dir: PathBuf,
    model: Arc<Mutex<Model>>,
}

pub struct TestRepository {
    _root: TempDir,
    worktree: PathBuf,
    git_dir: PathBuf,
}

impl TestRepository {
    pub fn init(owner: &str, repository: &str) -> Result<Self> {
        validate_name("owner", owner)?;
        validate_name("repository", repository)?;
        let root = tempfile::tempdir()?;
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("remote.git");
        fs::create_dir(&worktree)?;

        run_git(
            root.path(),
            ["init", "--bare", "--initial-branch=main", path(&git_dir)?],
        )?;
        run_git(&worktree, ["init", "--initial-branch=main"])?;
        run_git(&worktree, ["config", "user.name", "Praddle Test"])?;
        run_git(&worktree, ["config", "user.email", "praddle@example.com"])?;
        run_git(&worktree, ["commit", "--allow-empty", "-m", "Base"])?;
        run_git(&worktree, ["remote", "add", "origin", path(&git_dir)?])?;
        run_git(&worktree, ["push", "--set-upstream", "origin", "main"])?;
        run_git(&worktree, ["checkout", "-b", "change"])?;
        let remote_url = format!("https://github.com/{owner}/{repository}");
        run_git(
            &worktree,
            ["remote", "set-url", "origin", remote_url.as_str()],
        )?;

        Ok(Self {
            _root: root,
            worktree,
            git_dir,
        })
    }

    pub fn git<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        run_git(&self.worktree, args)
    }

    pub fn write(&self, relative_path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
        let destination = self.worktree.join(relative_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(destination, contents)?;
        Ok(())
    }

    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command.current_dir(&self.worktree);
        command
    }

    pub fn remote_ref_exists(&self, reference: &str) -> Result<bool> {
        Ok(self.remote_ref_oid(reference)?.is_some())
    }

    pub fn remote_ref_oid(&self, reference: &str) -> Result<Option<String>> {
        let mut command = self.remote_git();
        command.args([
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            reference,
        ]);
        let output = command
            .output()
            .with_context(|| format!("could not execute {command:?}"))?;
        match output.status.code() {
            Some(0) => Ok(Some(oid_output(&output)?)),
            Some(1) => Ok(None),
            _ => {
                checked_command_output(&command, output)?;
                unreachable!()
            }
        }
    }

    pub fn remote_branch_refs(&self) -> Result<BTreeSet<String>> {
        let mut command = self.remote_git();
        command.args(["for-each-ref", "--format=%(refname)", "refs/heads"]);
        let output = checked_output(&mut command)?;
        Ok(String::from_utf8(output.stdout)?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    pub fn commit_tree_oid(&self, commit: &str) -> Result<String> {
        let mut command = self.remote_git();
        command.args([
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{commit}^{{tree}}"),
        ]);
        oid_output(&checked_output(&mut command)?)
    }

    pub fn commit_parent_oids(&self, commit: &str) -> Result<Vec<String>> {
        let mut command = self.remote_git();
        command.args([
            "show",
            "--no-patch",
            "--format=%P",
            "--end-of-options",
            commit,
        ]);
        let output = checked_output(&mut command)?;
        Ok(String::from_utf8(output.stdout)?
            .split_whitespace()
            .map(str::to_owned)
            .collect())
    }

    pub fn commit_message(&self, commit: &str) -> Result<String> {
        let mut command = self.remote_git();
        command.args([
            "show",
            "--no-patch",
            "--format=%B",
            "--end-of-options",
            commit,
        ]);
        let output = checked_output(&mut command)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let mut command = self.remote_git();
        command.args([
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            ancestor,
            descendant,
        ]);
        let output = command
            .output()
            .with_context(|| format!("could not execute {command:?}"))?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => {
                checked_command_output(&command, output)?;
                unreachable!()
            }
        }
    }

    pub fn worktree(&self) -> &Path {
        &self.worktree
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    fn remote_git(&self) -> Command {
        let mut command = Command::new("git");
        command.arg("--git-dir").arg(&self.git_dir);
        command
    }
}

pub struct TestHarness {
    server: TestServer,
    repository: TestRepository,
}

impl TestHarness {
    pub async fn start(owner: &str, repository: &str) -> Result<Self> {
        let test_repository = TestRepository::init(owner, repository)?;
        let server = TestServer::start(owner, repository, test_repository.git_dir()).await?;
        Ok(Self {
            server,
            repository: test_repository,
        })
    }

    pub fn git<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = self.command("git");
        command.args(args);
        checked_output(&mut command)
    }

    pub fn write(&self, relative_path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
        self.repository.write(relative_path, contents)
    }

    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = self.repository.command(program);
        self.server.apply_environment(&mut command);
        command
    }

    pub fn remote_ref_exists(&self, reference: &str) -> Result<bool> {
        self.repository.remote_ref_exists(reference)
    }

    pub fn remote_ref_oid(&self, reference: &str) -> Result<Option<String>> {
        self.repository.remote_ref_oid(reference)
    }

    pub fn remote_branch_refs(&self) -> Result<BTreeSet<String>> {
        self.repository.remote_branch_refs()
    }

    pub fn commit_tree_oid(&self, commit: &str) -> Result<String> {
        self.repository.commit_tree_oid(commit)
    }

    pub fn commit_parent_oids(&self, commit: &str) -> Result<Vec<String>> {
        self.repository.commit_parent_oids(commit)
    }

    pub fn commit_message(&self, commit: &str) -> Result<String> {
        self.repository.commit_message(commit)
    }

    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        self.repository.is_ancestor(ancestor, descendant)
    }

    pub fn worktree(&self) -> &Path {
        self.repository.worktree()
    }

    pub fn snapshot(&self) -> Snapshot {
        self.server.snapshot()
    }

    pub fn server(&self) -> &TestServer {
        &self.server
    }
}

fn run_git<I, S>(directory: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command.args(args).current_dir(directory);
    checked_output(&mut command)
}

fn checked_output(command: &mut Command) -> Result<Output> {
    let output = command
        .output()
        .with_context(|| format!("could not execute {command:?}"))?;
    checked_command_output(command, output)
}

fn checked_command_output(command: &Command, output: Output) -> Result<Output> {
    if !output.status.success() {
        bail!(
            "{command:?} failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output)
}

fn oid_output(output: &Output) -> Result<String> {
    Ok(String::from_utf8(output.stdout.clone())?.trim().to_owned())
}

fn path(path: &Path) -> Result<&str> {
    path.to_str().context("test repository path is not UTF-8")
}

impl AppState {
    fn snapshot(&self) -> Snapshot {
        let model = self.model.lock().unwrap();
        Snapshot {
            owner: self.owner.clone(),
            repository: self.repository.clone(),
            pull_requests: model.pull_requests.values().cloned().collect(),
            stacks: model.stacks.clone(),
        }
    }
}

pub struct TestServer {
    state: AppState,
    socket: PathBuf,
    config_dir: TempDir,
    shutdown: Option<oneshot::Sender<()>>,
    tasks: Vec<JoinHandle<()>>,
}

impl TestServer {
    pub async fn start(owner: &str, repository: &str, git_dir: impl Into<PathBuf>) -> Result<Self> {
        validate_name("owner", owner)?;
        validate_name("repository", repository)?;
        let git_dir = git_dir.into();
        if !git_dir.is_dir() {
            bail!("git directory does not exist: {}", git_dir.display());
        }
        let git_dir = git_dir.canonicalize()?;

        let config_dir = tempfile::tempdir()?;
        let socket = config_dir.path().join("github.sock");
        let state = AppState {
            owner: owner.to_owned(),
            repository: repository.to_owned(),
            git_dir,
            model: Arc::new(Mutex::new(Model {
                next_pr: 1,
                next_stack: 1,
                fail_next_base_update: false,
                pull_requests: BTreeMap::new(),
                stacks: BTreeMap::new(),
            })),
        };
        let app = Router::new()
            .fallback(any(dispatch))
            .with_state(state.clone());
        let unix = UnixListener::bind(&socket)?;
        install_push_hook(&state.git_dir, &socket)?;
        let (shutdown, shutdown_rx) = oneshot::channel();
        let unix_task = tokio::spawn(async move {
            let _ = axum::serve(unix, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        fs::write(
            config_dir.path().join("config.yml"),
            format!(
                "http_unix_socket: {}\nprompt: disabled\nspinner: disabled\nhosts:\n  github.com:\n    user: {}\n    oauth_token: test-token\n    git_protocol: https\n",
                socket.display(),
                owner
            ),
        )?;

        Ok(Self {
            state,
            socket,
            config_dir,
            shutdown: Some(shutdown),
            tasks: vec![unix_task],
        })
    }

    pub fn apply_environment(&self, command: &mut Command) {
        let source = format!(
            "https://github.com/{}/{}",
            self.state.owner, self.state.repository
        );
        let target = format!("file://{}", self.state.git_dir.display());
        command
            .env("GH_CONFIG_DIR", self.config_dir.path())
            .env("GH_TOKEN", "test-token")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_SPINNER_DISABLED", "1")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", format!("url.{target}.insteadOf"))
            .env("GIT_CONFIG_VALUE_0", source);
    }

    pub fn snapshot(&self) -> Snapshot {
        self.state.snapshot()
    }

    pub fn fail_next_base_update(&self) {
        self.state.model.lock().unwrap().fail_next_base_update = true;
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn validate_name(kind: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.contains('/') {
        bail!("{kind} must be one non-empty path component");
    }
    Ok(())
}

fn install_push_hook(git_dir: &Path, socket: &Path) -> Result<()> {
    let hooks = git_dir.join("hooks");
    let pre_receive = hooks.join("pre-receive");
    fs::write(&pre_receive, PRE_RECEIVE_HOOK)?;
    make_executable(&pre_receive)?;

    let post_receive = hooks.join("post-receive");
    let socket = path_arg(socket)?;
    let contents = format!(
        "#!/bin/sh\ncurl --silent --show-error --fail --unix-socket {socket} --request POST http://localhost/_test/push >/dev/null\n"
    );
    fs::write(&post_receive, contents)?;
    make_executable(&post_receive)
}

const PRE_RECEIVE_HOOK: &str = r#"#!/bin/sh
zero=0000000000000000000000000000000000000000
while read old new ref
do
    [ "$old" = "$zero" ] && continue
    [ "$new" = "$zero" ] && continue
    case "$ref" in
        refs/heads/*)
            git merge-base --is-ancestor "$old" "$new"
            status=$?
            if [ "$status" -eq 0 ]; then
                continue
            elif [ "$status" -ne 1 ]; then
                echo "could not verify update: $ref" >&2
                exit "$status"
            fi
            git merge-base --is-ancestor "$new" "$old"
            status=$?
            if [ "$status" -eq 1 ]; then
                echo "divergent update rejected: $ref" >&2
                exit 1
            elif [ "$status" -ne 0 ]; then
                echo "could not verify inverse update: $ref" >&2
                exit "$status"
            fi
            ;;
    esac
done
"#;

fn make_executable(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn path_arg(path: &Path) -> Result<String> {
    let path = path.to_str().context("hook path is not UTF-8")?;
    Ok(format!("'{}'", path.replace('\'', "'\"'\"'")))
}

async fn dispatch(
    State(state): State<AppState>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    body: Bytes,
) -> Response {
    match dispatch_inner(&state, method, uri.path(), &body) {
        Ok(response) => response,
        Err((status, message)) => {
            (status, json!({ "message": message }).to_string()).into_response()
        }
    }
}

type HttpResult = Result<Response, (StatusCode, String)>;

fn dispatch_inner(state: &AppState, method: Method, path: &str, body: &[u8]) -> HttpResult {
    if path == "/_test/push" && method == Method::POST {
        detect_merged(state).map_err(internal)?;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    if path == "/_test/state" && method == Method::GET {
        return json_response(
            StatusCode::OK,
            &serde_json::to_value(state.snapshot()).unwrap(),
        );
    }
    if path == "/graphql" && method == Method::POST {
        return graphql(state, body);
    }
    let api_prefix = format!("/repos/{}/{}", state.owner, state.repository);
    if let Some(rest) = path.strip_prefix(&api_prefix) {
        return rest_api(state, method, rest, body);
    }
    Err((
        StatusCode::NOT_FOUND,
        format!("unimplemented {method} {path}"),
    ))
}

fn graphql(state: &AppState, body: &[u8]) -> HttpResult {
    let request: Value = serde_json::from_slice(body).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid GraphQL request: {error}"),
        )
    })?;
    let query = request.get("query").and_then(Value::as_str).unwrap_or("");
    let variables = request
        .get("variables")
        .cloned()
        .unwrap_or_else(|| json!({}));
    for key in ["owner"] {
        if variables
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| value != state.owner)
        {
            return Err((StatusCode::NOT_FOUND, "unknown repository owner".into()));
        }
    }
    for key in ["name", "repo"] {
        if variables
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| value != state.repository)
        {
            return Err((StatusCode::NOT_FOUND, "unknown repository".into()));
        }
    }
    if let Some(search) = variables.get("searchQuery").and_then(Value::as_str)
        && !search.contains(&format!("repo:{}/{}", state.owner, state.repository))
    {
        return Err((StatusCode::NOT_FOUND, "unknown repository".into()));
    }

    if query.contains("__type(") || query.contains("__schema {") {
        return json_response(
            StatusCode::OK,
            &json!({ "data": { "__type": { "fields": [] } } }),
        );
    }
    if query.contains("createPullRequest") {
        let input = &variables["input"];
        let mut model = state.model.lock().unwrap();
        let number = model.next_pr;
        model.next_pr += 1;
        let pr = PullRequest {
            number,
            title: string(input, "title"),
            body: string(input, "body"),
            state: "OPEN".into(),
            base_ref_name: string(input, "baseRefName"),
            base_ref_history: vec![string(input, "baseRefName")],
            head_ref_name: string(input, "headRefName"),
            is_draft: input.get("draft").and_then(Value::as_bool).unwrap_or(false),
            reviewers: vec![],
            comments: vec![],
            stack: None,
            stack_position: None,
        };
        model.pull_requests.insert(number, pr);
        return json_response(
            StatusCode::OK,
            &json!({ "data": { "createPullRequest": { "pullRequest": { "id": pr_id(number), "url": pr_url(state, number) } } } }),
        );
    }
    if query.contains("updatePullRequest") {
        let input = &variables["input"];
        let number = id_number(input.get("pullRequestId"));
        let mut model = state.model.lock().unwrap();
        if input.get("baseRefName").is_some() && model.fail_next_base_update {
            model.fail_next_base_update = false;
            return Err((StatusCode::CONFLICT, "injected base update failure".into()));
        }
        let pr = get_pr_mut(&mut model, number)?;
        if let Some(value) = input.get("baseRefName").and_then(Value::as_str) {
            if pr.stack.is_some() && pr.base_ref_name != value {
                return Err((
                    StatusCode::CONFLICT,
                    "cannot retarget a pull request while it belongs to a stack".into(),
                ));
            }
            pr.base_ref_name = value.into();
            pr.base_ref_history.push(value.into());
        }
        if let Some(value) = input.get("title").and_then(Value::as_str) {
            pr.title = value.into();
        }
        if let Some(value) = input.get("body").and_then(Value::as_str) {
            pr.body = value.into();
        }
        return json_response(
            StatusCode::OK,
            &json!({ "data": { "updatePullRequest": { "__typename": "UpdatePullRequestPayload" } } }),
        );
    }
    if query.contains("markPullRequestReadyForReview")
        || query.contains("convertPullRequestToDraft")
    {
        let input = &variables["input"];
        let number = id_number(input.get("pullRequestId"));
        let mut model = state.model.lock().unwrap();
        let pr = get_pr_mut(&mut model, number)?;
        pr.is_draft = query.contains("convertPullRequestToDraft");
        let field = if pr.is_draft {
            "convertPullRequestToDraft"
        } else {
            "markPullRequestReadyForReview"
        };
        return json_response(
            StatusCode::OK,
            &json!({ "data": { field: { "pullRequest": { "id": pr_id(number) } } } }),
        );
    }
    if query.contains("addComment") {
        let input = &variables["input"];
        let number = id_number(input.get("subjectId"));
        let mut model = state.model.lock().unwrap();
        let pr = get_pr_mut(&mut model, number)?;
        pr.comments.push(string(input, "body"));
        return json_response(
            StatusCode::OK,
            &json!({ "data": { "addComment": { "commentEdge": { "node": { "url": format!("{}#issuecomment-{}", pr_url(state, number), pr.comments.len()) } } } } }),
        );
    }
    if query.contains("requestReviewsByLogin") || query.contains("requestReviews(") {
        let input = &variables["input"];
        let number = id_number(input.get("pullRequestId"));
        let mut model = state.model.lock().unwrap();
        let pr = get_pr_mut(&mut model, number)?;
        for key in ["userLogins", "botLogins", "teamSlugs"] {
            if let Some(values) = input.get(key).and_then(Value::as_array) {
                pr.reviewers
                    .extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
            }
        }
        let field = if query.contains("requestReviewsByLogin") {
            "requestReviewsByLogin"
        } else {
            "requestReviews"
        };
        return json_response(
            StatusCode::OK,
            &json!({ "data": { (field): { "clientMutationId": null } } }),
        );
    }
    if query.contains("search(") {
        let search = variables
            .get("searchQuery")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let model = state.model.lock().unwrap();
        let nodes: Vec<Value> = model
            .pull_requests
            .values()
            .filter(|pr| {
                if search.contains("state:open") {
                    pr.state == "OPEN"
                } else if search.contains("state:closed") {
                    pr.state != "OPEN"
                } else if search.contains("is:merged") {
                    pr.state == "MERGED"
                } else {
                    true
                }
            })
            .map(|pr| pr_json(state, pr))
            .collect();
        return json_response(
            StatusCode::OK,
            &json!({ "data": { "search": { "nodes": nodes, "pageInfo": { "hasNextPage": false, "endCursor": null } } } }),
        );
    }
    if query.contains("query PullRequestProjectItems") {
        return json_response(
            StatusCode::OK,
            &json!({ "data": { "repository": { "pullRequest": { "projectItems": { "totalCount": 0, "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } } } } } }),
        );
    }

    let number = variables
        .get("number")
        .or_else(|| variables.get("pr_number"))
        .or_else(|| variables.get("prNumber"))
        .or_else(|| variables.get("pullRequestNumber"))
        .and_then(Value::as_u64);
    let model = state.model.lock().unwrap();
    let pull_request = number
        .and_then(|number| model.pull_requests.get(&number))
        .map(|pr| pr_json(state, pr));
    let nodes: Vec<Value> = model
        .pull_requests
        .values()
        .filter(|pr| pr.state == "OPEN")
        .map(|pr| pr_json(state, pr))
        .collect();
    json_response(
        StatusCode::OK,
        &json!({
            "data": {
                "repository": repository_json(state, pull_request, nodes),
                "viewer": { "login": state.owner }
            }
        }),
    )
}

fn repository_json(state: &AppState, pull_request: Option<Value>, nodes: Vec<Value>) -> Value {
    json!({
        "id": "REPOSITORY_1",
        "name": state.repository,
        "nameWithOwner": format!("{}/{}", state.owner, state.repository),
        "url": format!("https://github.com/{}/{}", state.owner, state.repository),
        "isFork": false,
        "viewerPermission": "ADMIN",
        "viewerCanPush": true,
        "viewerCanAdminister": true,
        "hasIssuesEnabled": true,
        "defaultBranchRef": { "name": "main" },
        "owner": { "id": "USER_1", "login": state.owner },
        "pullRequest": pull_request,
        "pullRequests": { "nodes": nodes, "pageInfo": { "hasNextPage": false, "endCursor": null } },
        "assignableUsers": { "nodes": [], "totalCount": 0 },
        "labels": { "nodes": [] },
        "milestones": { "nodes": [] }
    })
}

fn pr_json(state: &AppState, pr: &PullRequest) -> Value {
    json!({
        "id": pr_id(pr.number),
        "number": pr.number,
        "title": pr.title,
        "body": pr.body,
        "state": pr.state,
        "closed": pr.state != "OPEN",
        "url": pr_url(state, pr.number),
        "baseRefName": pr.base_ref_name,
        "headRefName": pr.head_ref_name,
        "isDraft": pr.is_draft,
        "author": { "id": "USER_1", "login": state.owner },
        "reviewRequests": { "nodes": [] },
        "assignees": { "nodes": [] },
        "assignedActors": { "nodes": [] },
        "labels": { "nodes": [] },
        "projectCards": { "nodes": [] },
        "projectItems": { "nodes": [] },
        "milestone": null,
        "comments": { "nodes": [] },
        "stack": pr.stack.map(|number| json!({ "number": number })),
        "stackEntry": pr.stack_position.map(|position| json!({ "position": position }))
    })
}

fn rest_api(state: &AppState, method: Method, path: &str, body: &[u8]) -> HttpResult {
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    if parts.as_slice() == ["stacks"] && method == Method::POST {
        let numbers = pull_request_numbers(body)?;
        if numbers.len() < 2 {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                "a stack requires at least two pull requests".into(),
            ));
        }
        let mut model = state.model.lock().unwrap();
        let stack = model.next_stack;
        model.next_stack += 1;
        assign_stack(&mut model, stack, &numbers);
        return json_response(StatusCode::CREATED, &json!({ "number": stack }));
    }
    if let ["stacks", stack] = parts.as_slice() {
        let stack = parse_number(stack)?;
        if method == Method::GET {
            let model = state.model.lock().unwrap();
            let numbers = model
                .stacks
                .get(&stack)
                .ok_or_else(|| (StatusCode::NOT_FOUND, "unknown stack".into()))?;
            let prs: Vec<Value> = numbers.iter().map(|number| json!({ "number": number, "merged_at": model.pull_requests.get(number).filter(|pr| pr.state == "MERGED").map(|_| "merged") })).collect();
            return json_response(StatusCode::OK, &json!({ "pull_requests": prs }));
        }
    }
    if let ["stacks", stack, operation] = parts.as_slice() {
        let stack = parse_number(stack)?;
        if method == Method::POST && *operation == "add" {
            let additions = pull_request_numbers(body)?;
            let mut model = state.model.lock().unwrap();
            let mut numbers = model
                .stacks
                .get(&stack)
                .cloned()
                .ok_or_else(|| (StatusCode::NOT_FOUND, "unknown stack".into()))?;
            numbers.extend(additions);
            assign_stack(&mut model, stack, &numbers);
            return json_response(StatusCode::OK, &json!({}));
        }
        if method == Method::POST && *operation == "unstack" {
            let mut model = state.model.lock().unwrap();
            let numbers = model
                .stacks
                .remove(&stack)
                .ok_or_else(|| (StatusCode::NOT_FOUND, "unknown stack".into()))?;
            for number in numbers {
                if let Some(pr) = model.pull_requests.get_mut(&number) {
                    pr.stack = None;
                    pr.stack_position = None;
                }
            }
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
    }
    if let ["issues", number, "comments"] = parts.as_slice() {
        let number = parse_number(number)?;
        if method == Method::POST {
            let request: Value = serde_json::from_slice(body).map_err(bad_json)?;
            let mut model = state.model.lock().unwrap();
            let pr = get_pr_mut(&mut model, number)?;
            pr.comments.push(string(&request, "body"));
            return json_response(
                StatusCode::CREATED,
                &json!({ "id": pr.comments.len(), "html_url": format!("{}#issuecomment-{}", pr_url(state, number), pr.comments.len()) }),
            );
        }
    }
    if let ["pulls", number, "requested_reviewers"] = parts.as_slice() {
        let number = parse_number(number)?;
        let request: Value = serde_json::from_slice(body).map_err(bad_json)?;
        let mut model = state.model.lock().unwrap();
        let pr = get_pr_mut(&mut model, number)?;
        for key in ["reviewers", "team_reviewers"] {
            if let Some(values) = request.get(key).and_then(Value::as_array) {
                pr.reviewers
                    .extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
            }
        }
        return json_response(StatusCode::CREATED, &pr_json(state, pr));
    }
    Err((
        StatusCode::NOT_FOUND,
        format!("unimplemented REST {method} {path}"),
    ))
}

fn assign_stack(model: &mut Model, stack: u64, numbers: &[u64]) {
    model.stacks.insert(stack, numbers.to_vec());
    for (position, number) in numbers.iter().enumerate() {
        if let Some(pr) = model.pull_requests.get_mut(number) {
            pr.stack = Some(stack);
            pr.stack_position = Some(position + 1);
        }
    }
}

fn detect_merged(state: &AppState) -> Result<()> {
    let open_pull_requests: Vec<(u64, String, String)> = {
        let model = state.model.lock().unwrap();
        model
            .pull_requests
            .values()
            .filter(|pr| pr.state == "OPEN")
            .map(|pr| {
                (
                    pr.number,
                    branch_ref(&pr.head_ref_name),
                    branch_ref(&pr.base_ref_name),
                )
            })
            .collect()
    };
    if open_pull_requests.is_empty() {
        return Ok(());
    }

    let mut merged = Vec::new();
    for (number, head, base) in open_pull_requests {
        let mut command = Command::new("git");
        command.args([
            "--git-dir",
            path(&state.git_dir)?,
            "merge-base",
            "--is-ancestor",
            &head,
            &base,
        ]);
        let output = command
            .output()
            .with_context(|| format!("could not execute {command:?}"))?;
        match output.status.code() {
            Some(0) => merged.push((number, head, base)),
            Some(1) => {}
            _ => {
                checked_command_output(&command, output)?;
            }
        }
    }

    let mut model = state.model.lock().unwrap();
    for (number, head, base) in merged {
        if let Some(pr) = model.pull_requests.get_mut(&number)
            && pr.state == "OPEN"
            && branch_ref(&pr.head_ref_name) == head
            && branch_ref(&pr.base_ref_name) == base
        {
            pr.state = "MERGED".into();
        }
    }
    Ok(())
}

fn branch_ref(name: &str) -> String {
    if name.starts_with("refs/") {
        name.to_owned()
    } else {
        format!("refs/heads/{name}")
    }
}

fn json_response(status: StatusCode, value: &Value) -> HttpResult {
    let mut response = (status, value.to_string()).into_response();
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    Ok(response)
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn id_number(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_str)
        .and_then(|id| id.strip_prefix("PR_"))
        .and_then(|number| number.parse().ok())
        .unwrap_or(0)
}

fn pr_id(number: u64) -> String {
    format!("PR_{number}")
}

fn pr_url(state: &AppState, number: u64) -> String {
    format!(
        "https://github.com/{}/{}/pull/{number}",
        state.owner, state.repository
    )
}

fn get_pr_mut(model: &mut Model, number: u64) -> Result<&mut PullRequest, (StatusCode, String)> {
    model.pull_requests.get_mut(&number).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("unknown pull request {number}"),
        )
    })
}

fn parse_number(value: &str) -> Result<u64, (StatusCode, String)> {
    value
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, format!("invalid number {value}")))
}

fn pull_request_numbers(body: &[u8]) -> Result<Vec<u64>, (StatusCode, String)> {
    let value: Value = serde_json::from_slice(body).map_err(bad_json)?;
    value
        .get("pull_requests")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_u64).collect())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "missing pull_requests".into()))
}

fn bad_json(error: serde_json::Error) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, format!("invalid JSON: {error}"))
}

fn internal(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

pub async fn serve(
    owner: String,
    repository: String,
    git_dir: PathBuf,
    socket: &Path,
) -> Result<()> {
    validate_name("owner", &owner)?;
    validate_name("repository", &repository)?;
    if !git_dir.is_dir() {
        bail!("git directory does not exist: {}", git_dir.display());
    }
    let state = AppState {
        owner,
        repository,
        git_dir: git_dir.canonicalize()?,
        model: Arc::new(Mutex::new(Model {
            next_pr: 1,
            next_stack: 1,
            fail_next_base_update: false,
            pull_requests: BTreeMap::new(),
            stacks: BTreeMap::new(),
        })),
    };
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        fs::remove_file(socket)?;
    }
    let unix = UnixListener::bind(socket)
        .with_context(|| format!("could not listen on {}", socket.display()))?;
    install_push_hook(&state.git_dir, socket)?;
    println!("{{\"socket\":\"{}\"}}", socket.display());
    let app = Router::new().fallback(any(dispatch)).with_state(state);
    tokio::select! {
        result = axum::serve(unix, app) => result?,
        _ = tokio::signal::ctrl_c() => {},
    }
    Ok(())
}
