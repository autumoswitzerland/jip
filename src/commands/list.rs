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
//  jip — `jip list`
//  ---------------------------------------------------------------------------
//  Prints every resolved dependency with its pinned version — the runtime,
//  compile-only (`provided`) and test packages from `jip.lock`.  When the
//  lock file is missing the project is resolved first.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-17
// =============================================================================

use crate::artifact::Artifact;
use crate::commands::{lock_parts, require_config};

/// List all resolved dependencies, grouped by their scope.
pub fn run(client: &reqwest::blocking::Client) -> anyhow::Result<()> {
    let config = require_config()?;
    let (runtime, provided, test) = lock_parts(client, &config)?;

    if runtime.is_empty() && provided.is_empty() && test.is_empty() {
        println!("no dependencies");
        return Ok(());
    }

    print_section("dependencies:", &runtime);
    print_section("provided-dependencies:", &provided);
    print_section("test-dependencies:", &test);
    Ok(())
}

/// Print one section title, then every artifact as `group:artifact = version`.
fn print_section(title: &str, artifacts: &[Artifact]) {
    if artifacts.is_empty() {
        return;
    }
    println!("{title}");
    for artifact in artifacts {
        println!("  {} = {}", artifact.key(), artifact.version);
    }
}
