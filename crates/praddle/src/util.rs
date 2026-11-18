// Copyright © 2026 Advanced Micro Devices, Inc. All rights reserved.
// SPDX-License-Identifier: MIT

use anyhow::{Context, Result, bail};
use std::fmt::Debug;
use std::process::{Command, Output};
use tempfile::NamedTempFile;

const MAX_VERBOSE_LINE_BYTES: usize = 100;
const MAX_VERBOSE_LINES: usize = 10;

fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_VERBOSE_LINE_BYTES {
        return line.to_owned();
    }
    let marker = "[...]";
    let prefix_bytes = (MAX_VERBOSE_LINE_BYTES - marker.len()) / 2;
    let suffix_bytes = MAX_VERBOSE_LINE_BYTES - marker.len() - prefix_bytes;
    let mut prefix_end = prefix_bytes;
    while !line.is_char_boundary(prefix_end) {
        prefix_end -= 1;
    }
    let mut suffix_start = line.len() - suffix_bytes;
    while !line.is_char_boundary(suffix_start) {
        suffix_start += 1;
    }
    format!("{}{}{}", &line[..prefix_end], marker, &line[suffix_start..])
}

fn output_lines(output: &[u8], truncate: bool) -> Vec<String> {
    let output = String::from_utf8_lossy(output);
    let lines: Vec<_> = output.lines().collect();
    if !truncate || lines.len() <= MAX_VERBOSE_LINES {
        return lines
            .into_iter()
            .map(|line| {
                if truncate {
                    truncate_line(line)
                } else {
                    line.to_owned()
                }
            })
            .collect();
    }

    let leading_lines = (MAX_VERBOSE_LINES - 1) / 2;
    let trailing_lines = MAX_VERBOSE_LINES - leading_lines - 1;
    lines[..leading_lines]
        .iter()
        .copied()
        .chain(std::iter::once("[...]"))
        .chain(lines[lines.len() - trailing_lines..].iter().copied())
        .map(truncate_line)
        .collect()
}

fn print_output(prefix: &str, output: &[u8], truncate: bool) {
    for line in output_lines(output, truncate) {
        eprintln!("{prefix}{line}");
    }
}

pub fn print_cmd_and_files<'a, I>(cmd: &Command, files: I) -> Result<()>
where
    I: Iterator<Item = &'a NamedTempFile>,
{
    let env = crate::env::get();
    if env.dry_run() {
        eprintln!("would-exec: {:?}", cmd);
    }
    if env.always_echo() {
        for file in files {
            let contents = std::fs::read(file.path())?;
            eprintln!(
                "file-contents-{}: {}",
                file.path().display(),
                output_lines(&contents, env.verbosity() <= 1).join("\n")
            );
        }
    }
    Ok(())
}

pub fn exec_impl(env: &crate::env::Env, cmd: &mut Command) -> Result<Output> {
    let id = env.next_exec_id();
    if env.always_echo() {
        eprintln!("exec-{}: {:?}", id, cmd);
    }
    let output = cmd
        .output()
        .with_context(|| format!("exec-failed: {:?}", cmd))?;
    if env.always_echo() || !output.status.success() {
        let truncate = env.verbosity() <= 1;
        print_output(
            &format!("exec-{id}-stdout: "),
            output.stdout.as_ref(),
            truncate,
        );
        print_output(
            &format!("exec-{id}-stderr: "),
            output.stderr.as_ref(),
            truncate,
        );
    }
    if !output.status.success() {
        bail!("exec-{}-status-non-zero: {:?}", id, output.status);
    }
    Ok(output)
}

pub fn exec(cmd: &mut Command) -> Result<Output> {
    exec_impl(crate::env::get(), cmd)
}

pub trait Extract {
    type T;

    fn extract(self) -> Self::T;
}

impl<T, E: Debug> Extract for std::result::Result<T, E> {
    type T = T;
    fn extract(self) -> T {
        match self {
            Ok(x) => x,
            Err(e) => {
                eprint!("Error: {e:?}");
                std::process::exit(-1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_output_keeps_up_to_ten_lines() {
        let output = (1..=10)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            output_lines(output.as_bytes(), true),
            (1..=10).map(|line| line.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn verbose_output_elides_middle_lines() {
        let output = (1..=12)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            output_lines(output.as_bytes(), true),
            ["1", "2", "3", "4", "[...]", "8", "9", "10", "11", "12"]
        );
    }

    #[test]
    fn very_verbose_output_keeps_all_lines() {
        let output = (1..=12)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            output_lines(output.as_bytes(), false),
            (1..=12).map(|line| line.to_string()).collect::<Vec<_>>()
        );
    }
}
