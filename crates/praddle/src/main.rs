// Copyright © 2026 Advanced Micro Devices, Inc. All rights reserved.
// SPDX-License-Identifier: MIT

mod change;
mod cli;
mod env;
mod gh;
mod praddle;
mod publication;
mod util;

fn main() -> anyhow::Result<()> {
    use clap::Parser;

    praddle::praddle(cli::Cli::parse())
}
