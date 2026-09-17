// Copyright © 2026 Advanced Micro Devices, Inc. All rights reserved.
// SPDX-License-Identifier: MIT

use crate::change::{Diff, LocalChange};
use crate::env;
use crate::util::{exec, print_cmd_and_files};
use anyhow::{Context, Result, bail};
use git2::message_trailers_strs;
use rayon::prelude::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::Command;
use tempfile::NamedTempFile;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: PrState,
    pub base_ref_name: String,
    pub head_ref_name: String,
    #[serde(rename = "isDraft")]
    pub draft: bool,
    pub stack: Option<PrStack>,
    pub stack_entry: Option<PrStackEntry>,
}

#[derive(Debug, Deserialize)]
pub struct PrStack {
    pub number: u64,
}

#[derive(Debug, Deserialize)]
pub struct PrStackEntry {
    pub position: u64,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum PrState {
    #[default]
    Open,
    Closed,
    Merged,
}

fn gh() -> Command {
    Command::new("gh")
}

/// A guess at a reasonable size for one arg to avoid making the command-line
/// too long. Anything longer than this will use a tempfile.
const MAX_INLINE_ARG_LENGTH: usize = 0x1000;

struct ArgInlineOrFile {
    arg_base: &'static str,
    file: Option<NamedTempFile>,
}
impl ArgInlineOrFile {
    pub fn new(arg_base: &'static str) -> ArgInlineOrFile {
        ArgInlineOrFile {
            arg_base,
            file: None,
        }
    }
    pub fn arg<S: AsRef<str>>(&mut self, contents: S) -> Result<String> {
        let ret: String;
        let arg_base = self.arg_base;
        let contents = contents.as_ref();
        let contents_bytes = contents.as_bytes();
        if contents_bytes.len() > MAX_INLINE_ARG_LENGTH {
            let mut file = NamedTempFile::new()?;
            file.write_all(contents_bytes)?;
            let path = file.path().to_str().context("arg file path is not utf-8")?;
            ret = format!("--{arg_base}-file={path}");
            if self.file.replace(file).is_some() {
                bail!("ArgInlineOrFile was reused");
            }
        } else {
            ret = format!("--{arg_base}={contents}");
        }
        Ok(ret)
    }
}

// FIXME: this pseudo-parsing seems wrong, but just getting it working for me first
fn build_repo_url() -> Result<String> {
    let env = env::get();
    let url = env.remote_url()?;
    Ok(if url.starts_with("https://") {
        url
    } else if let Some(git_path) = url.strip_prefix("git@github.com:") {
        format!(
            "https://github.com/{}",
            git_path.strip_suffix(".git").unwrap_or(git_path)
        )
    } else {
        bail!("unhandled git remote url: {url:?}");
    })
}

fn repo_arg() -> Result<String> {
    Ok(format!("--repo={}", build_repo_url()?))
}

fn repo_name() -> Result<(String, String)> {
    let url = build_repo_url()?;
    let path = url
        .strip_prefix("https://github.com/")
        .context("could not parse GitHub repository URL")?
        .trim_end_matches('/');
    let (owner, name) = path
        .split_once('/')
        .context("GitHub repository URL has no owner or repository name")?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        bail!("GitHub repository URL has an invalid owner or repository name: {path}");
    }
    Ok((owner.to_owned(), name.to_owned()))
}

#[derive(Deserialize)]
struct GraphQlResponse<T> {
    data: Option<T>,
}

#[derive(Deserialize)]
struct GraphQlSearchData<T> {
    search: GraphQlSearch<T>,
}

#[derive(Deserialize)]
struct GraphQlSearch<T> {
    nodes: Vec<T>,
    #[serde(rename = "pageInfo")]
    page_info: GraphQlPageInfo,
}

#[derive(Deserialize)]
struct GraphQlPageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

const GQL_LIMIT: u8 = 100;

#[derive(Debug, Deserialize)]
struct RestStack {
    pull_requests: Vec<RestStackPr>,
}

#[derive(Debug, Deserialize)]
struct RestStackPr {
    number: u64,
    merged_at: Option<String>,
}

#[derive(Serialize)]
struct StackRequest<'a> {
    pull_requests: &'a [u64],
}

fn rest_stack(endpoint: String) -> Result<RestStack> {
    let args = vec![
        "api".to_owned(),
        endpoint,
        "--header".to_owned(),
        "Accept: application/vnd.github+json".to_owned(),
        "--header".to_owned(),
        "X-GitHub-Api-Version: 2026-03-10".to_owned(),
    ];
    let mut cmd = gh();
    cmd.args(args);
    let output = exec(&mut cmd)?;
    Ok(serde_json::from_slice(output.stdout.as_ref())?)
}

fn rest_api_empty(method: &str, endpoint: String, body: Option<&[u64]>) -> Result<()> {
    let input = body
        .map(|pull_requests| -> anyhow::Result<_> {
            let mut input = NamedTempFile::new()?;
            serde_json::to_writer(&mut input, &StackRequest { pull_requests })?;
            let path = input
                .path()
                .to_str()
                .context("input file path is not utf-8")?;
            Ok((format!("--input={path}"), input))
        })
        .transpose()?;
    let mut args = vec![
        "api".to_owned(),
        endpoint,
        "--method".to_owned(),
        method.to_owned(),
        "--header".to_owned(),
        "Accept: application/vnd.github+json".to_owned(),
        "--header".to_owned(),
        "X-GitHub-Api-Version: 2026-03-10".to_owned(),
    ];
    if let Some((input_arg, _)) = input.as_ref() {
        args.push(input_arg.clone());
    }
    let mut cmd = gh();
    cmd.args(args);
    print_cmd_and_files(&cmd, input.as_ref().into_iter().map(|(_, file)| file))?;
    if env::get().dry_run() {
        return Ok(());
    }
    exec(&mut cmd)?;
    Ok(())
}

fn unstack(endpoint: String) -> Result<()> {
    let mut cmd = gh();
    cmd.args([
        "api",
        endpoint.as_str(),
        "--method",
        "POST",
        "--header",
        "Accept: application/vnd.github+json",
        "--header",
        "X-GitHub-Api-Version: 2026-03-10",
        "--include",
    ]);
    print_cmd_and_files(&cmd, std::iter::empty())?;
    if env::get().dry_run() {
        return Ok(());
    }
    let output = exec(&mut cmd)?;
    let status = String::from_utf8_lossy(output.stdout.as_ref())
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .context("gh api unstack response did not contain an HTTP status")?;
    match status {
        204 => Ok(()),
        200 => bail!("unstacking {endpoint} left pull requests in the stack"),
        _ => bail!("unexpected HTTP status from unstacking {endpoint}: {status}"),
    }
}

fn graphql_search<T, K, F>(gql_query: &str, search_query: &str, key: F) -> Result<Vec<T>>
where
    T: DeserializeOwned,
    K: Eq + std::hash::Hash,
    F: Fn(&T) -> K,
{
    let mut end_cursor = None;
    let mut seen = HashSet::new();
    let mut nodes = Vec::new();

    loop {
        let search_query_arg = format!("searchQuery={search_query}");
        let limit_arg = format!("limit={GQL_LIMIT}");
        let query_arg = format!("query={gql_query}");
        let mut args = vec![
            "api".to_owned(),
            "graphql".to_owned(),
            "-F".to_owned(),
            search_query_arg,
            "-F".to_owned(),
            limit_arg,
            "-f".to_owned(),
            query_arg,
        ];
        if let Some(cursor) = end_cursor.as_ref() {
            args.extend(["-F".to_owned(), format!("endCursor={cursor}")]);
        }

        let mut cmd = gh();
        cmd.args(args);
        let output = exec(&mut cmd)?;
        let response: GraphQlResponse<GraphQlSearchData<T>> =
            serde_json::from_slice(output.stdout.as_ref())?;
        let data = response
            .data
            .context("GitHub GraphQL response did not contain data")?;
        let search = data.search;

        for node in search.nodes {
            if seen.insert(key(&node)) {
                nodes.push(node);
            }
        }

        if !search.page_info.has_next_page {
            break;
        }
        end_cursor = Some(
            search
                .page_info
                .end_cursor
                .context("GitHub GraphQL response had no cursor for the next page")?,
        );
    }

    Ok(nodes)
}

impl Pr {
    pub fn mock(title: &str, body: &str, base: &str, head: &str) -> Self {
        let env = env::get();
        Self {
            number: env.next_mock_pr_id() as u64,
            title: title.to_owned(),
            body: body.to_owned(),
            state: PrState::Open,
            base_ref_name: base.to_owned(),
            head_ref_name: head.to_owned(),
            draft: true,
            stack: None,
            stack_entry: None,
        }
    }

    fn args_for<I, S>(&self, subcommand: &str, opts: I) -> Result<Vec<String>>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut args = vec!["pr".into()];
        args.extend([subcommand.into(), self.number.to_string(), repo_arg()?]);
        for opt in opts {
            args.push(opt.into());
        }
        Ok(args)
    }

    pub fn message(&self) -> String {
        format!("{}\n\n{}", self.title, self.body)
    }

    pub fn in_state(&self, state: PrState) -> bool {
        self.state == state
    }

    pub fn mark_ready(&mut self, ready: bool) -> Result<()> {
        let draft = !ready;
        if self.draft == draft {
            return Ok(());
        }
        let mut cmd = gh();
        let opts = if ready {
            vec![]
        } else {
            vec!["--undo".to_string()]
        };
        let args = self.args_for("ready", opts)?;
        cmd.args(args);
        print_cmd_and_files(&cmd, std::iter::empty())?;
        if !env::get().dry_run() {
            exec(&mut cmd)?;
        }
        self.draft = draft;
        Ok(())
    }

    pub fn set_base(&mut self, base: &str) -> Result<()> {
        if self.base_ref_name == base {
            return Ok(());
        }
        let mut cmd = gh();
        let args = self.args_for("edit", [format!("--base={base}")])?;
        cmd.args(args);
        print_cmd_and_files(&cmd, std::iter::empty())?;
        if !env::get().dry_run() {
            exec(&mut cmd)?;
        }
        self.base_ref_name = base.to_owned();
        Ok(())
    }

    pub fn set_details(&mut self, title: &str, body: &str) -> Result<()> {
        if self.title == title && self.body == body {
            return Ok(());
        }
        let mut body_arg = ArgInlineOrFile::new("body");
        let mut cmd = gh();
        let args = self.args_for("edit", [format!("--title={title}"), body_arg.arg(body)?])?;
        cmd.args(args);
        print_cmd_and_files(&cmd, body_arg.file.iter())?;
        if !env::get().dry_run() {
            exec(&mut cmd)?;
        }
        self.title = title.to_owned();
        self.body = body.to_owned();
        Ok(())
    }

    pub fn add_details_comment(&self, diff: &Diff) -> Result<()> {
        let (summary, changes) = match diff {
            Diff::InitialDiff(text) => ("Initial changes", text),
            Diff::InterDiff(text) => ("Changes since last version", text),
        };
        if changes.is_empty() {
            return Ok(());
        }
        let code_block = markdown_code_block("diff", changes);
        let comment = format!(
            "<details>\n<summary>🛠️ {summary} (click to expand):</summary>\n\n{code_block}\n</details>"
        );
        let mut body_arg = ArgInlineOrFile::new("body");
        let mut cmd = gh();
        let args = self.args_for("comment", [body_arg.arg(comment)?])?;
        cmd.args(args);
        print_cmd_and_files(&cmd, body_arg.file.iter())?;
        if env::get().dry_run() {
            return Ok(());
        }
        exec(&mut cmd)?;
        Ok(())
    }

    pub fn add_reviewers(&self, reviewers: &[String]) -> Result<()> {
        if reviewers.is_empty() {
            return Ok(());
        }
        let mut cmd = gh();
        let args = self.args_for("edit", [format!("--add-reviewer={}", reviewers.join(","))])?;
        cmd.args(args);
        print_cmd_and_files(&cmd, std::iter::empty())?;
        if env::get().dry_run() {
            return Ok(());
        }
        exec(&mut cmd)?;
        Ok(())
    }

    pub fn create(local_change: &LocalChange) -> Result<Pr> {
        let env = env::get();
        let commit = env
            .repo()?
            .find_commit(local_change.oid)
            .context("cannot find commit")?;
        let remote_branch_ref = local_change.remote_branch_ref();
        let title = commit
            .summary()
            .context("failed to get commit summary")?
            .context("commit has no summary")?;
        let body = commit
            .body()
            .context("failed to get commit body")?
            .context("commit has no body")?;
        let base = env.base_branch();
        let mut body_arg = ArgInlineOrFile::new("body");
        let mut cmd = gh();
        let args = vec![
            "pr".into(),
            "create".into(),
            repo_arg()?,
            "--draft".into(),
            format!("--base={base}"),
            format!("--title={title}"),
            body_arg.arg(body)?,
            format!("--head={remote_branch_ref}"),
        ];
        cmd.args(args);
        print_cmd_and_files(&cmd, body_arg.file.iter())?;
        let output = if env::get().dry_run() {
            let mock_pr = Pr::mock(title, body, base, &remote_branch_ref);
            eprintln!("mock-pr-for-change-{}: {}", local_change.id, mock_pr.number);
            return Ok(mock_pr);
        } else {
            exec(&mut cmd)?
        };
        for line in String::from_utf8_lossy(output.stdout.as_ref()).lines() {
            if line.starts_with("https://github.com") {
                let mut path_components = line.rsplitn(2, '/');
                let number = path_components
                    .next()
                    .with_context(|| format!("gh pr create printed invalid pr URL: {line}"))?;
                return Ok(Pr {
                    number: number.parse::<u64>().context("pr number is not a number")?,
                    title: title.into(),
                    body: body.into(),
                    state: PrState::Open,
                    base_ref_name: base.into(),
                    head_ref_name: remote_branch_ref,
                    draft: true,
                    stack: None,
                    stack_entry: None,
                });
            }
        }
        bail!("gh pr create did not produce a URL")
    }

    pub fn get_url(&self) -> Result<String> {
        Ok(format!("{}/pull/{}", build_repo_url()?, self.number))
    }
}

fn markdown_code_block(language: &str, contents: &str) -> String {
    let longest_run = contents
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(3.max(longest_run + 1));
    format!("{fence}{language}\n{contents}\n{fence}")
}

const PR_GQL_QUERY: &str = "\
query($searchQuery: String!, $limit: Int!, $endCursor: String) { \
search(query: $searchQuery, type: ISSUE, first: $limit, after: $endCursor) { \
nodes { \
    ... on PullRequest { \
        number title body state baseRefName headRefName isDraft \
        stack { number } \
        stackEntry { position } \
    } \
} \
pageInfo { hasNextPage endCursor } \
}}\
";

fn prs() -> Result<Vec<Pr>> {
    let (owner, name) = repo_name()?;
    let search_queries = [
        format!("repo:{owner}/{name} is:pr author:@me state:open"),
        format!("repo:{owner}/{name} is:pr author:@me state:closed"),
    ];
    let search_results: Vec<Vec<Pr>> = search_queries
        .par_iter()
        .map(|search_query| graphql_search(PR_GQL_QUERY, search_query, |pr: &Pr| pr.number))
        .collect::<Result<_>>()?;
    let mut seen = HashSet::new();
    let mut prs = Vec::new();
    for pr in search_results.into_iter().flatten() {
        if seen.insert(pr.number) {
            prs.push(pr);
        }
    }
    Ok(prs)
}

struct ExistingStack {
    number: u64,
    pull_requests: Vec<u64>,
}

fn existing_stack(prs: &[&Pr]) -> Result<Option<ExistingStack>> {
    let stack_numbers: HashSet<u64> = prs
        .iter()
        .filter_map(|pr| pr.stack.as_ref().map(|stack| stack.number))
        .collect();
    if stack_numbers.len() > 1 {
        bail!("local prs belong to multiple GitHub stacks: {stack_numbers:?}");
    }
    let Some(stack_number) = stack_numbers.iter().next().copied() else {
        return Ok(None);
    };
    let (owner, name) = repo_name()?;
    let stack = rest_stack(format!("repos/{owner}/{name}/stacks/{stack_number}"))?;
    let unmerged: Vec<u64> = stack
        .pull_requests
        .iter()
        .filter(|pr| pr.merged_at.is_none())
        .map(|pr| pr.number)
        .collect();
    let local_stack: Vec<&Pr> = prs
        .iter()
        .filter(|pr| {
            pr.stack
                .as_ref()
                .is_some_and(|stack| stack.number == stack_number)
        })
        .copied()
        .collect();
    if local_stack.len() != unmerged.len() {
        bail!("GitHub stack {stack_number} contains unmerged prs outside the local stack");
    }

    let mut by_position = local_stack;
    by_position.sort_unstable_by_key(|pr| {
        pr.stack_entry
            .as_ref()
            .map(|entry| entry.position)
            .unwrap_or(u64::MAX)
    });
    let existing: Vec<u64> = by_position.iter().map(|pr| pr.number).collect();
    if existing != unmerged {
        bail!("GraphQL and REST disagree about the order of GitHub stack {stack_number}");
    }
    Ok(Some(ExistingStack {
        number: stack_number,
        pull_requests: existing,
    }))
}

pub struct StackPlan {
    existing: Option<ExistingStack>,
}

impl StackPlan {
    pub fn inspect(prs: &[&Pr]) -> Result<Self> {
        Ok(Self {
            existing: existing_stack(prs)?,
        })
    }

    pub fn requires_unstack(&self, desired: &[(&Pr, &str)]) -> bool {
        let Some(existing) = self.existing.as_ref() else {
            return false;
        };
        let desired_order: Vec<u64> = desired.iter().rev().map(|(pr, _)| pr.number).collect();
        desired_order != existing.pull_requests
            || desired.iter().any(|(pr, base)| pr.base_ref_name != *base)
    }

    pub fn unstack(&mut self) -> Result<()> {
        let Some(existing) = self.existing.take() else {
            return Ok(());
        };
        if !existing.pull_requests.is_empty() {
            let (owner, name) = repo_name()?;
            unstack(format!(
                "repos/{owner}/{name}/stacks/{}/unstack",
                existing.number
            ))?;
        }
        Ok(())
    }

    pub fn reconcile(self, prs: &[&Pr]) -> Result<()> {
        let (owner, name) = repo_name()?;
        let desired: Vec<u64> = prs.iter().rev().map(|pr| pr.number).collect();
        let Some(existing) = self.existing else {
            return rest_api_empty(
                "POST",
                format!("repos/{owner}/{name}/stacks"),
                Some(&desired),
            );
        };
        let endpoint =
            |suffix: &str| format!("repos/{owner}/{name}/stacks/{}{suffix}", existing.number);
        if desired.starts_with(&existing.pull_requests) {
            let additions = &desired[existing.pull_requests.len()..];
            if !additions.is_empty() {
                rest_api_empty("POST", endpoint("/add"), Some(additions))?;
            }
        } else {
            unstack(endpoint("/unstack"))?;
            rest_api_empty(
                "POST",
                format!("repos/{owner}/{name}/stacks"),
                Some(&desired),
            )?;
        }
        Ok(())
    }
}

pub fn prs_by_change_id<'a>(
    change_ids: impl IntoIterator<Item = &'a str>,
) -> Result<HashMap<String, Pr>> {
    let requested_change_ids: HashSet<&str> = change_ids.into_iter().collect();
    let mut by_id: HashMap<String, Pr> = HashMap::new();
    for pr in prs()? {
        let trailers =
            message_trailers_strs(pr.message().as_ref()).context("message_trailers_strs failed")?;
        let mut pr_change_ids = trailers
            .iter()
            .filter_map(|(k, v)| if k == "Change-Id" { Some(v) } else { None });
        let id = match pr_change_ids.next() {
            Some(id) => id,
            None => continue,
        };
        if !requested_change_ids.contains(id) {
            continue;
        }
        if pr_change_ids.next().is_some() {
            bail!("pr has multiple Change-Id: {pr:?}");
        }
        if let Some(other) = by_id.get(id) {
            bail!(
                "multiple prs have Change-Id {id}: {} and {}",
                other.number,
                pr.number
            );
        }
        by_id.insert(id.to_owned(), pr);
    }
    Ok(by_id)
}

#[cfg(test)]
mod tests {
    use super::markdown_code_block;

    #[test]
    fn code_fence_is_longer_than_backtick_runs_in_its_contents() {
        assert_eq!(
            markdown_code_block("diff", "+before\n```\n+after"),
            "````diff\n+before\n```\n+after\n````"
        );
    }
}
