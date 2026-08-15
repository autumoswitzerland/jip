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
//  jip — Local Cache
//  ---------------------------------------------------------------------------
//  Downloaded jars are stored locally so that `jip run` works offline once
//  the dependencies have been fetched once.
//
//  Two locations are involved:
//    * jip's own cache  (~/.cache/jip) — where jip downloads jars.
//    * the Maven repo   (~/.m2/repository) — where Maven/Gradle store jars.
//      When enabled, jip reuses jars already present there and does not
//      download them again.
//
//  Both locations use the same directory layout as a Maven repository,
//  so files are interchangeable.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, bail};
use sha1::{Digest, Sha1};

use crate::artifact::Artifact;

/// Cache of downloaded artifacts and the repository they come from.
#[derive(Debug, Clone)]
pub struct Cache {
    /// Where jars downloaded by jip are stored.
    pub cache_root: PathBuf,
    /// Reuse jars from `~/.m2/repository` when present.
    pub use_m2: bool,
    /// HTTP client used for downloads.
    client: reqwest::blocking::Client,
}

impl Cache {
    pub fn new(client: reqwest::blocking::Client, use_m2: bool) -> Self {
        Self {
            cache_root: default_cache_root(),
            use_m2,
            client,
        }
    }

    /// Path of the Maven repository that Maven/Gradle create locally.
    pub fn m2_repo(&self) -> PathBuf {
        home_dir().join(".m2").join("repository")
    }

    /// Where a jar would live inside jip's own cache.
    fn cache_jar_path(&self, artifact: &Artifact) -> PathBuf {
        self.cache_root
            .join(artifact.directory())
            .join(artifact.jar_file_name())
    }

    /// Return the jar for `artifact` if it already exists locally.
    ///
    /// When the Maven repo is enabled and contains the jar, that one wins.
    pub fn existing_jar(&self, artifact: &Artifact) -> Option<PathBuf> {
        if self.use_m2 {
            let m2_path = self
                .m2_repo()
                .join(artifact.directory())
                .join(artifact.jar_file_name());
            if m2_path.exists() {
                return Some(m2_path);
            }
        }
        let cache_path = self.cache_jar_path(artifact);
        cache_path.exists().then_some(cache_path)
    }

    /// Make sure the jar for `artifact` is available locally, downloading it
    /// (and verifying its SHA-1 checksum) when it is missing.
    pub fn ensure_jar(&self, artifact: &Artifact, repo_url: &str) -> anyhow::Result<PathBuf> {
        if let Some(path) = self.existing_jar(artifact) {
            return Ok(path);
        }
        self.download_jar(artifact, repo_url)
    }

    /// Download a jar into the cache, verifying the SHA-1 checksum.
    fn download_jar(&self, artifact: &Artifact, repo_url: &str) -> anyhow::Result<PathBuf> {
        let dest = self.cache_jar_path(artifact);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }

        let jar_url = format!(
            "{repo_url}/{}{}",
            artifact.directory(),
            artifact.jar_file_name()
        );
        println!("downloading {jar_url}");

        // Download to a temporary file first, then rename into place.
        // This way a half-written jar is never mistaken for a complete one.
        let temp_path = dest.with_extension("jar.tmp");
        let jar_bytes = self
            .client
            .get(&jar_url)
            .send()
            .with_context(|| format!("cannot download {jar_url}"))?
            .error_for_status()
            .with_context(|| format!("download failed: {jar_url}"))?
            .bytes()
            .context("reading jar response body")?;
        fs::write(&temp_path, &jar_bytes)?;

        // Verify against the published SHA-1 when available.
        let sha1_url = format!("{jar_url}.sha1");
        match self.client.get(&sha1_url).send() {
            Ok(response) if response.status().is_success() => {
                let expected = response.text().context("reading checksum")?;
                let actual = hex::encode(Sha1::digest(&jar_bytes));
                if !expected.trim().eq_ignore_ascii_case(&actual) {
                    let _ = fs::remove_file(&temp_path);
                    bail!(
                        "checksum mismatch for {} (expected {}, got {})",
                        jar_url,
                        expected.trim(),
                        actual
                    );
                }
            }
            // Some repositories do not publish checksums; warn but proceed.
            _ => println!("warning: no SHA-1 checksum available for {jar_url}"),
        }

        fs::rename(&temp_path, &dest)
            .with_context(|| format!("cannot move jar into {}", dest.display()))?;
        Ok(dest)
    }
}

/// The directory jip uses for its own cache.
fn default_cache_root() -> PathBuf {
    // Respect the XDG convention where set.
    if let Ok(root) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(root).join("jip");
    }
    // Windows prefers %LOCALAPPDATA% for application data.
    if cfg!(windows)
        && let Ok(local_app_data) = std::env::var("LOCALAPPDATA")
    {
        return PathBuf::from(local_app_data).join("jip");
    }
    home_dir().join(".cache").join("jip")
}

/// The user's home directory, on any operating system.
///
/// Windows uses `USERPROFILE` instead of `HOME`; Unix systems use `HOME`.
fn home_dir() -> PathBuf {
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(home_var)
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
}

/// Download a small text file (such as a POM) from a repository.
pub fn download_text(client: &reqwest::blocking::Client, url: &str) -> anyhow::Result<String> {
    let mut response = client
        .get(url)
        .send()
        .with_context(|| format!("cannot download {url}"))?
        .error_for_status()
        .with_context(|| format!("download failed: {url}"))?;
    let mut text = String::new();
    response.read_to_string(&mut text)?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_paths_follow_repository_layout() {
        let cache = Cache::new(reqwest::blocking::Client::new(), false);
        let artifact = Artifact::parse("com.google.guava:guava:33.0.0-jre").unwrap();
        assert!(
            cache
                .cache_jar_path(&artifact)
                .ends_with("com/google/guava/guava/33.0.0-jre/guava-33.0.0-jre.jar")
        );
    }
}
