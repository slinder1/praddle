// Copyright © 2026 Advanced Micro Devices, Inc. All rights reserved.
// SPDX-License-Identifier: MIT

use crate::env;
use crate::gh::Pr;
use crate::publication::{RemoteRefs, branch_ref};
use anyhow::{Context, Result, bail};
use git2::{
    Commit, DiffDelta, DiffFormat, DiffHunk, DiffLine, DiffOptions, FileFavor, MergeOptions, Oid,
    Tree, message_trailers_bytes,
};
use std::collections::HashSet;

#[derive(Debug)]
pub enum AnyChange {
    LocalChange(LocalChange),
    Change(Change),
}

impl AnyChange {
    pub fn local_change(&self) -> &LocalChange {
        match self {
            AnyChange::LocalChange(local_change) => local_change,
            AnyChange::Change(change) => &change.local_change,
        }
    }
    pub fn diff(&self, remote_refs: &RemoteRefs) -> Result<Diff> {
        Ok(match self {
            AnyChange::LocalChange(local_change) => Diff::InitialDiff(local_change.diff()?),
            AnyChange::Change(change)
                if remote_refs
                    .get(&change.local_change.remote_branch_ref())
                    .is_some() =>
            {
                Diff::InterDiff(change.interdiff(remote_refs)?)
            }
            AnyChange::Change(change) => Diff::InitialDiff(change.local_change.diff()?),
        })
    }
}

pub enum Diff {
    InitialDiff(String),
    InterDiff(String),
}

type DiffPrinter<'a> = Box<dyn FnMut(DiffDelta, Option<DiffHunk>, DiffLine) -> bool + 'a>;

fn diff_printer<'a>(out: &'a mut String) -> DiffPrinter<'a> {
    Box::new(|_delta, _hunk, line| {
        match line.origin() {
            '+' | '-' | ' ' => out.push(line.origin()),
            _ => {}
        }
        let s = str::from_utf8(line.content()).expect("non-utf8 encoded content in diff");
        // The file mode and index info is just noise here, but I don't see how to disable both
        // in the options. Just rely on the output of the 'F' origin "line" and strip
        // everything but the `diff --git <A> <B>` part.
        if line.origin() == 'F'
            && let Some((p, _)) = s.split_once('\n')
        {
            out.push_str(p);
            out.push('\n')
        } else {
            out.push_str(s)
        };
        true
    })
}

#[derive(Debug)]
pub struct LocalChange {
    pub id: String,
    pub oid: Oid,
}

impl LocalChange {
    pub fn remote_branch(&self) -> String {
        let branch_prefix = env::get().user_branch_prefix();
        let change_id = &self.id;
        format!("{branch_prefix}{change_id}")
    }
    pub fn remote_branch_ref(&self) -> String {
        format!("refs/heads/{}", self.remote_branch())
    }
    pub fn diff(&self) -> Result<String> {
        let change = self.id.as_str();
        let repo = env::get().repo()?;
        let commit = self.commit()?;
        let parent = commit
            .parent(0)
            .with_context(|| format!("change {change} has no parent commit",))?;
        let mut diff_opts = DiffOptions::new();
        diff_opts.reverse(true);
        let diff = repo.diff_tree_to_tree(
            Some(&tree(&commit)?),
            Some(&tree(&parent)?),
            Some(&mut diff_opts),
        )?;
        let mut out = String::new();
        diff.print(DiffFormat::Patch, diff_printer(&mut out))
            .with_context(|| format!("failed to generate interdiff for change {change}"))?;
        Ok(out)
    }
    pub fn commit(&self) -> Result<Commit<'_>> {
        Ok(env::get().repo()?.find_commit(self.oid)?)
    }
}

#[derive(Debug)]
pub struct Change {
    pub local_change: LocalChange,
    pub pr: Pr,
}

impl Change {
    /// Adapted from https://joshcannon.me/2025/04/05/pr-interdiff.html
    pub fn interdiff(&self, remote_refs: &RemoteRefs) -> Result<String> {
        let change = self.local_change.id.as_str();
        let repo = env::get().repo()?;
        let remote_branch = self.local_change.remote_branch_ref();
        let old_commit = repo
            .find_commit(remote_refs.require(&remote_branch)?)
            .with_context(|| format!("remote branch is not a commit: {remote_branch}"))?;
        let new_commit = self.local_change.commit()?;
        let old_base_ref = branch_ref(&self.pr.base_ref_name);
        let old_merge_base = repo
            .find_commit(remote_refs.require(&old_base_ref)?)
            .with_context(|| format!("remote base is not a commit: {old_base_ref}"))?;
        let new_merge_base = new_commit
            .parent(0)
            .with_context(|| format!("new version of change {change} has no parent commit",))?;
        let mut merge_opts = MergeOptions::new();
        merge_opts
            .find_renames(true)
            .no_recursive(true)
            .file_favor(FileFavor::Theirs);
        let merge_idx = repo
            .merge_trees(
                &tree(&old_merge_base)?,
                &tree(&new_merge_base)?,
                &tree(&old_commit)?,
                Some(&merge_opts),
            )
            .with_context(|| format!("merge to calculate interdiff for change {change} failed"))?;
        let mut diff_opts = DiffOptions::new();
        diff_opts.reverse(true);
        let diff = repo.diff_tree_to_index(
            Some(&tree(&new_commit)?),
            Some(&merge_idx),
            Some(&mut diff_opts),
        )?;
        let mut out = String::new();
        diff.print(DiffFormat::Patch, diff_printer(&mut out))
            .with_context(|| format!("failed to generate interdiff for change {change}"))?;
        Ok(out)
    }
}

fn tree<'repo>(commit: &Commit<'repo>) -> Result<Tree<'repo>> {
    commit
        .tree()
        .with_context(|| format!("commit {:?} has no tree?", commit.id()))
}

pub fn get_local_changes() -> Result<Vec<LocalChange>> {
    let repo = env::get().repo()?;
    let mut local_changes = vec![];
    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    let base_branch_ref = env::get().base_branch_ref();
    revwalk.hide_ref(&base_branch_ref)?;
    let mut seen_change_ids = HashSet::new();
    for oid in revwalk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        if commit.parent_count() != 1 {
            bail!("change commit must have exactly one parent: {commit:?}");
        }
        let trailers = message_trailers_bytes(
            commit
                .message_raw()
                .with_context(|| format!("commit lacks message: {commit:?}"))?,
        )
        .context("message_trailer_bytes failed")?;
        let mut change_ids = trailers.iter().filter_map(|(k, v)| {
            if k == b"Change-Id" {
                Some(String::from_utf8(v.into()))
            } else {
                None
            }
        });
        let id = change_ids
            .next()
            .with_context(|| format!("commit lacks Change-Id: {commit:?}"))?
            .with_context(|| format!("commit Change-Id is not valid utf8: {commit:?}"))?;
        if change_ids.next().is_some() {
            bail!("commit has multiple Change-Id: {commit:?}");
        }
        if !seen_change_ids.insert(id.clone()) {
            bail!("local branch has duplicate Change-Id: {id}");
        }
        local_changes.push(LocalChange { id, oid });
    }
    Ok(local_changes)
}
