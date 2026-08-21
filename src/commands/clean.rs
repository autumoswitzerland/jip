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
//  jip — `jip clean`
//  ---------------------------------------------------------------------------
//  Removes the `target/` directory with all build artifacts (classes,
//  tests, jars).  Sources, `jip.toml` and `jip.lock` are left alone.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-17
// =============================================================================

use std::fs;
use std::path::Path;

use anyhow::{Context, bail};

/// Remove the `target/` build directory.
pub fn run() -> anyhow::Result<()> {
    let target = Path::new("target");
    let had_root = target.exists();
    if had_root {
        if !target.is_dir() {
            bail!(
                "{} is not a directory — refusing to remove it",
                target.display()
            );
        }
        fs::remove_dir_all(target)
            .with_context(|| format!("cannot remove {}", target.display()))?;
    }

    if let Ok(config) = crate::config::ProjectConfig::load(Path::new("jip.toml"))
        && let Some(modules) = &config.modules
    {
        for path in modules.modules.values() {
            let module_target = Path::new(path).join("target");
            if module_target.exists() {
                fs::remove_dir_all(&module_target)
                    .with_context(|| format!("cannot remove {}", module_target.display()))?;
                println!(
                    "{}",
                    crate::console::green(&format!("removed {path}/target/"))
                );
            }
        }
    }

    if had_root {
        println!(
            "{}",
            crate::console::green("removed target/ build artifacts")
        );
    }
    if !had_root {
        println!("nothing to clean — no target/ directory");
    }
    Ok(())
}
