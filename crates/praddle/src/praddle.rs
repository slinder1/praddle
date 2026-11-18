// Copyright © 2026 Advanced Micro Devices, Inc. All rights reserved.
// SPDX-License-Identifier: MIT

use crate::change::{self, AnyChange, Change};
use crate::cli;
use crate::env;
use crate::gh::{self, Pr, PrState};
use crate::publication::{PublicationPlan, RemoteRefs};
use crate::util::Extract;
use anyhow::{Context, Result, bail};
use git2::Repository;
use rayon::prelude::*;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

pub fn praddle(cli: cli::Cli) -> Result<()> {
    if let cli::Command::InstallHook(ref args) = cli.command {
        return install_hook(args, cli.globals.dry_run);
    }
    env::init(cli)?;
    let cli = env::get().cli();
    if cli.globals.serial {
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build_global()
            .context("could not install serial thread pool")
            .extract();
    }
    match cli.command {
        cli::Command::Push(ref args) => push(args),
        cli::Command::Url(ref args) => url(args),
        cli::Command::InstallHook(_) => unreachable!(),
    }
}

fn push(args: &cli::Push) -> Result<()> {
    let env = env::get();
    let mut reviewers = vec![];
    for group_key in args.reviewer_groups.iter() {
        let group = env
            .reviewer_groups()
            .context("used a review group, but none were found in the config file")?
            .get(group_key)
            .with_context(|| format!("reviewer group {group_key:?} not found in config file"))?;
        reviewers.extend_from_slice(group);
    }
    let local_changes =
        change::get_local_changes().context("could not enumerate current local branch")?;
    if local_changes.is_empty() {
        bail!("no local changes");
    }
    let mut prs_by_change_id =
        gh::prs_by_change_id(local_changes.iter().map(|change| change.id.as_str()))
            .context("could not enumerate remote prs")?;
    let mut any_changes = vec![];
    for local_change in local_changes {
        any_changes.push(match prs_by_change_id.remove(&local_change.id) {
            None => AnyChange::LocalChange(local_change),
            Some(pr) => {
                if !pr.in_state(PrState::Open) {
                    bail!(
                        "pr {} for local change {} is not open",
                        pr.number,
                        local_change.id
                    );
                }
                AnyChange::Change(Change { local_change, pr })
            }
        });
    }
    let remote_refs = RemoteRefs::fetch(&any_changes).context("could not fetch remote state")?;
    let diffs = any_changes
        .par_iter()
        .map(|change| change.diff(&remote_refs))
        .collect::<Result<Vec<_>>>()
        .context("could not build diffs")?;
    let mut publication = PublicationPlan::build(&any_changes, &remote_refs)
        .context("could not plan branch updates")?;
    publication.prepare_stack(&mut any_changes)?;
    if args.draft {
        any_changes
            .par_iter_mut()
            .filter_map(|ac| {
                if let AnyChange::Change(c) = ac {
                    Some(c)
                } else {
                    None
                }
            })
            .map(|c| {
                c.pr.mark_ready(false)
                    .with_context(|| format!("could not mark pr as draft: {:?}", c.pr))
            })
            .collect::<Result<Vec<_>>>()?;
    }
    let published_refs = publication
        .push()
        .context("could not publish local changes")?;
    let mut changes = any_changes
        .into_par_iter()
        .map(|any_change| {
            let change = match any_change {
                AnyChange::LocalChange(local_change) => {
                    let pr = Pr::create(&local_change)
                        .with_context(|| format!("could not create new pr for {local_change:?}"))?;
                    Change { local_change, pr }
                }
                AnyChange::Change(change) => change,
            };
            Ok(change)
        })
        .collect::<Result<Vec<_>>>()
        .context("could not create new prs")?;
    changes
        .par_iter_mut()
        .zip(publication.bases().par_iter())
        .map(|(c, base)| {
            c.pr.set_base(base.as_ref()).with_context(|| {
                format!(
                    "could not retarget pr {} to branch: {:?}",
                    c.pr.number, base,
                )
            })
        })
        .collect::<Result<Vec<_>>>()
        .context("could not set pr bases")?;
    publication
        .reconcile_stack(&changes)
        .context("could not reconcile pr stack")?;
    changes
        .par_iter_mut()
        .map(|c| {
            let commit = c.local_change.commit()?;
            let title = commit
                .summary()
                .context("failed to get commit summary")?
                .context("commit has no summary")?;
            let body = commit
                .body()
                .context("failed to get commit body")?
                .context("commit has no body")?;
            c.pr.set_details(title, body)
        })
        .collect::<Result<Vec<_>>>()
        .context("could not update pr titles and bodies")?;
    changes
        .par_iter()
        .zip(diffs)
        .map(|(c, diff)| c.pr.add_details_comment(&diff))
        .collect::<Result<Vec<_>>>()
        .context("could not add interdiff comments")?;
    changes
        .par_iter()
        .map(|c| c.pr.add_reviewers(reviewers.as_ref()))
        .collect::<Result<Vec<_>>>()
        .context("could not add pr reviewers")?;
    if !args.draft {
        changes
            .par_iter_mut()
            .map(|c| c.pr.mark_ready(true))
            .collect::<Result<Vec<_>>>()
            .context("could not mark prs as ready")?;
    }
    published_refs.commit();
    Ok(())
}

fn url(_args: &cli::Url) -> Result<()> {
    let local_changes =
        change::get_local_changes().context("could not enumerate current local branch")?;
    let mut prs_by_change_id =
        gh::prs_by_change_id(local_changes.iter().map(|change| change.id.as_str()))
            .context("could not enumerate remote prs")?;
    for local_change in local_changes {
        if let Some(pr) = prs_by_change_id.remove(&local_change.id) {
            println!("{}", pr.get_url()?);
            return Ok(());
        }
    }
    bail!("no change has an existing PR");
}

static COMMIT_MSG_HOOK_SRC: &str = include_str!("commit-msg");
static EXECUTABLE_MODE_BITS: u32 = 0o111;

fn install_hook(args: &cli::InstallHook, dry_run: bool) -> Result<()> {
    let repo = Repository::open(".").context("not in a git repo")?;
    let mut hook_path = PathBuf::from(repo.commondir());
    hook_path.extend(["hooks", "commit-msg"]);
    if dry_run {
        let verb = if args.force { "overwrite" } else { "write" };
        eprintln!("would {verb} {hook_path:?}");
        return Ok(());
    }
    let mut hook_file: File = if args.force {
        File::create(&hook_path)
            .with_context(|| format!("could not create hook file: {hook_path:?}"))
    } else {
        File::create_new(&hook_path)
            .with_context(|| format!("could not create hook file (try --force): {hook_path:?}"))
    }?;
    hook_file
        .write_all(COMMIT_MSG_HOOK_SRC.as_bytes())
        .with_context(|| format!("could not write to hook file: {hook_path:?}"))?;
    hook_file
        .flush()
        .with_context(|| format!("could not flush hook file: {hook_path:?}"))?;
    let mut perms = hook_file
        .metadata()
        .with_context(|| format!("could not get metadata for hook file: {hook_path:?}"))?
        .permissions();
    perms.set_mode(perms.mode() | EXECUTABLE_MODE_BITS);
    hook_file
        .set_permissions(perms)
        .with_context(|| format!("could not set permissions for hook file: {hook_path:?}"))?;
    Ok(())
}
