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
//  jip — Entry Point
//  ---------------------------------------------------------------------------
//  Parses the command line and hands the work over to the matching
//  command module.  Errors are printed to stderr; the exit code is non-zero
//  when a command failed.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

mod artifact;
mod cache;
mod central;
mod cli;
mod commands;
mod config;
mod console;
mod convert;
mod lock;
mod pom;
mod resolver;

use std::process::ExitCode;

use clap::Parser;

use cli::{Command, Jip};

fn main() -> ExitCode {
    let args = Jip::parse();
    match run(args.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{} {err:#}", console::red(&console::bold("jip: error:")));
            ExitCode::FAILURE
        }
    }
}

/// Dispatch to the command module for the parsed subcommand.
fn run(command: Command) -> anyhow::Result<()> {
    let client = commands::new_client();
    match command {
        Command::Init => commands::init::run(&client),
        Command::Add {
            dependency,
            test,
            provided,
        } => commands::add::run(&client, &dependency, test, provided),
        Command::Remove {
            dependency,
            test,
            provided,
        } => commands::remove::run(&client, &dependency, test, provided),
        Command::Resolve => commands::resolve::run(&client),
        Command::Build => commands::build::run(&client),
        Command::Run {
            main,
            defines,
            args,
        } => commands::run::run(&client, main.as_deref(), &defines, &args),
        Command::Jar { fat } => commands::jar::run(&client, fat),
        Command::Test => commands::test::run(&client),
        Command::Search { query } => commands::search::run(&client, &query),
        Command::Tree => commands::tree::run(&client),
        Command::Update => commands::update::run(&client),
    }
}
