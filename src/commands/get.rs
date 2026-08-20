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
//  jip — `jip get`
//  ---------------------------------------------------------------------------
//  Clones a git repository into `./<repo-name>/` and runs it.  A detected
//  Maven or Gradle build is converted to `jip.toml` automatically (clones
//  are meant to be throwaway, so there is no conversion prompt), then the
//  project is run just like `jip run` would.  Nom-Maven / non-Gradle clones
//  are refused and the directory is left for the user to remove.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-20
// =============================================================================

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};

use crate::config::CONFIG_FILE;
use crate::convert;
use crate::lock::LOCK_FILE;

/// Clone the given repository into `./<repo-name>/` and run it.
pub fn run(
    client: &reqwest::blocking::Client,
    offline: bool,
    url: &str,
    branch: Option<&str>,
    args: &[String],
) -> anyhow::Result<()> {
    let name = repo_name(url);
    let target = Path::new(&name);
    if target.exists() {
        bail!("the path ./{name} already exists — remove it first to clone again");
    }

    if std::io::stdin().is_terminal() {
        print!("clone {url} into ./{name}? [Y/n] ");
        std::io::stdout().flush().context("cannot write prompt")?;
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .context("cannot read answer")?;
        let answer = answer.trim();
        if !matches!(answer.to_ascii_lowercase().as_str(), "" | "y" | "yes") {
            println!("{} — nothing cloned", crate::console::green("ok"));
            return Ok(());
        }
    }

    let mut git = Command::new("git");
    git.arg("clone").arg("--depth").arg("1");
    if let Some(branch) = branch {
        git.arg("--branch").arg(branch);
    }
    git.arg(url).arg(target);
    let status = git
        .status()
        .with_context(|| "failed to run `git clone` — is git installed?")?;
    if !status.success() {
        bail!("`git clone` failed for {url}");
    }

    std::env::set_current_dir(target).with_context(|| format!("cannot enter ./{name}"))?;

    let project_type = if Path::new("pom.xml").exists() {
        "Maven"
    } else if Path::new("build.gradle").exists()
        || Path::new("build.gradle.kts").exists()
        || Path::new("settings.gradle").exists()
        || Path::new("settings.gradle.kts").exists()
    {
        "Gradle"
    } else {
        bail!(
            "./{name} is not a Maven or Gradle project — there is nothing to \
             convert or run.  Remove the directory with `rm -rf {name}` if it \
             is not needed"
        );
    };

    convert::convert_project(client)?;
    println!(
        "{}",
        crate::console::green(&format!(
            "converted {project_type} project — created {CONFIG_FILE} and {LOCK_FILE}"
        ))
    );

    crate::commands::run::run(client, offline, None, &[], args)
}

/// Derive the local directory name from a git URL, like `git clone` does.
///
/// Handles `https://`/`ssh://` URLs as well as SCP-style `git@host:org/repo`
/// ones, stripping a trailing `.git` and any trailing slashes.
fn repo_name(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    base.strip_suffix(".git").unwrap_or(base).to_string()
}

#[cfg(test)]
mod tests {
    use super::repo_name;

    #[test]
    fn repo_name_derives_from_https_url() {
        assert_eq!(repo_name("https://github.com/foo/bar.git"), "bar");
        assert_eq!(repo_name("https://github.com/foo/bar"), "bar");
        assert_eq!(repo_name("https://host/a/b/"), "b");
    }

    #[test]
    fn repo_name_derives_from_scp_and_ssh_urls() {
        assert_eq!(repo_name("git@github.com:org/project.git"), "project");
        assert_eq!(repo_name("ssh://git@host:2222/org/repo.git"), "repo");
    }

    #[test]
    fn repo_name_survives_trailing_slashes() {
        assert_eq!(repo_name("https://github.com/foo/bar///"), "bar");
    }
}
