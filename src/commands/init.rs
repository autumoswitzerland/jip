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
//  jip — `jip init`
//  ---------------------------------------------------------------------------
//  Creates a fresh `jip.toml` in the current directory.  When the directory
//  already contains a Maven or Gradle project, its dependencies are
//  converted automatically so switching to jip is a single command.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::path::Path;

use anyhow::bail;

use crate::config::CONFIG_FILE;
use crate::convert;
use crate::lock::LOCK_FILE;

/// Create a new project file, converting an existing build system if present.
pub fn run(client: &reqwest::blocking::Client) -> anyhow::Result<()> {
    let path = Path::new(CONFIG_FILE);
    if path.exists() {
        bail!("{CONFIG_FILE} already exists in this directory");
    }

    let config = convert::convert_project(client)?;

    println!("created {CONFIG_FILE}");
    println!("created {LOCK_FILE}");
    if config.dependencies.is_empty() {
        println!("next: add a dependency with `jip add group:artifact:version`");
    } else {
        println!("next: run it with `jip run`");
    }
    Ok(())
}
