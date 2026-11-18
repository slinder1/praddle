// Copyright © 2026 Advanced Micro Devices, Inc. All rights reserved.
// SPDX-License-Identifier: MIT

use crate::cli::Cli;
use anyhow::{Context, Result, bail};
use atomic_counter::{AtomicCounter, RelaxedCounter};
use git2::Repository;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, read_to_string};
use std::path::PathBuf;
use std::sync::OnceLock;
use thread_local::ThreadLocal;

pub struct ThreadLocalRepo {
    path: PathBuf,
    repo: ThreadLocal<Repository>,
}

impl ThreadLocalRepo {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            repo: ThreadLocal::new(),
        }
    }
    pub fn get(&self) -> Result<&Repository, git2::Error> {
        self.repo.get_or_try(|| Repository::open(&self.path))
    }
}

#[derive(Default, Serialize, Deserialize)]
struct FileConfig {
    remote: String,
    base_branch: String,
    user_branch_prefix: String,
    reviewer_groups: Option<HashMap<String, Vec<String>>>,
}

pub struct Env {
    cli: Cli,
    remote: String,
    base_branch: String,
    user_branch_prefix: String,
    reviewer_groups: Option<HashMap<String, Vec<String>>>,
    repo: ThreadLocalRepo,
    exec_ids: RelaxedCounter,
    mock_pr_ids: RelaxedCounter,
}

impl Env {
    pub fn new(cli: Cli) -> Result<Self> {
        let repo = ThreadLocalRepo::new(".".into());
        let file_config = read_config(&repo)?;
        let remote = cli.globals.remote.clone().unwrap_or(file_config.remote);
        let base_branch = cli
            .globals
            .base_branch
            .clone()
            .unwrap_or(file_config.base_branch);
        let user_branch_prefix = cli
            .globals
            .user_branch_prefix
            .clone()
            .unwrap_or(file_config.user_branch_prefix);
        if remote.is_empty() {
            bail!("field `remote` cannot be empty");
        }
        if base_branch.is_empty() {
            bail!("field `base_branch` cannot be empty");
        }
        if !user_branch_prefix.is_empty() && !user_branch_prefix.ends_with('/') {
            bail!("if field `user_branch_prefix` is non-empty it must end with `/`");
        }
        Ok(Self {
            cli,
            remote,
            base_branch,
            user_branch_prefix,
            reviewer_groups: file_config.reviewer_groups,
            repo,
            exec_ids: RelaxedCounter::new(0),
            mock_pr_ids: RelaxedCounter::new(0xffff_ffff),
        })
    }

    pub fn cli(&self) -> &Cli {
        &self.cli
    }

    pub fn repo(&self) -> Result<&Repository, git2::Error> {
        self.repo.get()
    }

    pub fn dry_run(&self) -> bool {
        self.cli.globals.dry_run
    }

    pub fn verbosity(&self) -> u8 {
        self.cli.globals.verbose
    }

    pub fn always_echo(&self) -> bool {
        self.dry_run() || self.verbosity() > 0
    }

    pub fn next_exec_id(&self) -> usize {
        self.exec_ids.inc()
    }

    pub fn next_mock_pr_id(&self) -> usize {
        self.mock_pr_ids.inc()
    }

    pub fn remote_url(&self) -> Result<String> {
        let remote = self
            .repo()?
            .find_remote(&self.remote)
            .with_context(|| format!("remote not found: {}", self.remote))?;
        remote
            .url()
            .map(str::to_owned)
            .with_context(|| format!("remote has no url: {}", self.remote))
    }

    pub fn base_branch(&self) -> &str {
        &self.base_branch
    }

    pub fn base_branch_ref(&self) -> String {
        format!("refs/heads/{}", self.base_branch)
    }

    pub fn user_branch_prefix(&self) -> &str {
        &self.user_branch_prefix
    }

    pub fn reviewer_groups(&self) -> Option<&HashMap<String, Vec<String>>> {
        self.reviewer_groups.as_ref()
    }
}

static ENV: OnceLock<Env> = OnceLock::new();

pub fn init(cli: Cli) -> Result<()> {
    ENV.set(Env::new(cli)?)
        .map_err(|_| anyhow::anyhow!("environment was initialized more than once"))
}

pub fn get() -> &'static Env {
    ENV.get().expect("environment must be initialized")
}

fn repo_config_path(repo: &ThreadLocalRepo, filename: &str) -> Option<PathBuf> {
    repo.get()
        .ok()
        .and_then(|r| r.workdir())
        .map(|wd| wd.join(filename))
        .filter(|p| fs::exists(p).unwrap_or(false))
}

fn user_config_path(filename: &str) -> Option<PathBuf> {
    dirs::config_dir()
        .map(|cd| cd.join(filename))
        .filter(|p| fs::exists(p).unwrap_or(false))
}

fn read_config(repo: &ThreadLocalRepo) -> Result<FileConfig> {
    let path = std::env::var_os("PRADDLE_CONFIG_PATH")
        .map(PathBuf::from)
        .or_else(|| repo_config_path(repo, ".praddle.toml"))
        .or_else(|| repo_config_path(repo, "praddle.toml"))
        .or_else(|| user_config_path("praddle.toml"));
    let path = match path {
        Some(p) => p,
        None => return Ok(Default::default()),
    };
    let contents = read_to_string(path.clone())
        .with_context(|| format!("could not read config file: {path:?}"))?;
    toml::from_str(contents.as_ref()).with_context(|| format!("invalid config file: {path:?}"))
}
