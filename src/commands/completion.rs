// ------------------------------------------------------------------------------
// Copyright (c) 2026 autumo GmbH. All rights reserved.
//
// Licensed under the GNU Affero General Public License v3.0 (AGPLv3).
// See LICENSE file in the project root for full license information.
//
// This file is part of jip. jip is free software: you can redistribute
// it and/or modify it under the terms of the GNU Affero General Public License
// as published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
// ------------------------------------------------------------------------------

// =============================================================================
//  jip — `jip completion`
//  ---------------------------------------------------------------------------
//  Prints a shell completion script for bash, zsh, or fish.  Generated from
//  the clap command definition, so completions always match the installed
//  version of jip.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-17
// =============================================================================

use anyhow::bail;
use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::cli::Jip;

/// Print the completion script for the requested shell to stdout.
pub fn run(shell: &str) -> anyhow::Result<()> {
    let shell = match shell {
        "bash" => Shell::Bash,
        "zsh" => Shell::Zsh,
        "fish" => Shell::Fish,
        other => bail!("unsupported shell \"{other}\" — expected bash, zsh or fish"),
    };
    let mut cmd = Jip::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, &name, &mut std::io::stdout());
    Ok(())
}
