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
use std::path::{Path, PathBuf};

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
    /// Use only locally cached jars; fail when a dependency is not cached.
    pub offline: bool,
    /// HTTP client used for downloads.
    client: reqwest::blocking::Client,
}

impl Cache {
    pub fn new(client: reqwest::blocking::Client, use_m2: bool, offline: bool) -> Self {
        Self {
            cache_root: default_cache_root(),
            use_m2,
            offline,
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
    ///
    /// In offline mode, only cached jars are accepted; missing jars cause an
    /// error instead of a download attempt.
    ///
    /// `repos` are tried in order; the first repository that has the jar wins.
    pub fn ensure_jar(&self, artifact: &Artifact, repos: &[String]) -> anyhow::Result<PathBuf> {
        if let Some(path) = self.existing_jar(artifact) {
            return Ok(path);
        }
        if self.offline {
            bail!(
                "{} is not cached — run without --offline to download it",
                artifact.jar_file_name()
            );
        }
        self.download_jar(artifact, repos)
    }

    /// Download a jar into the cache, verifying the SHA-1 checksum.
    fn download_jar(&self, artifact: &Artifact, repos: &[String]) -> anyhow::Result<PathBuf> {
        let mut tried = Vec::new();
        for repo in repos {
            match self.try_download(artifact, repo) {
                Ok(dest) => return Ok(dest),
                Err(err) => tried.push(format!("  {repo}: {err}")),
            }
        }
        bail!(
            "cannot download {} from any repository:\n{}",
            artifact.jar_file_name(),
            tried.join("\n")
        )
    }

    /// Download one jar from a single repository, which may be an HTTP URL
    /// or a `file://` path to a local Maven-style folder.
    fn try_download(&self, artifact: &Artifact, repo: &str) -> anyhow::Result<PathBuf> {
        let jar_path = format!("{}{}", artifact.directory(), artifact.jar_file_name());
        if let Some(base_dir) = file_url_path(repo) {
            ensure_repo_dir(&base_dir)?;
            let src = base_dir.join(&jar_path);
            let bytes = fs::read(&src)
                .with_context(|| format!("jar not found in local repository {src:?}"))?;
            let expected = fs::read_to_string(format!("{}.sha1", src.display())).ok();
            let jar_url = format!("{repo}/{jar_path}");
            return self.write_cached(artifact, &bytes, expected.as_deref(), &jar_url);
        }

        let jar_url = format!("{repo}/{jar_path}");
        println!("downloading {jar_url}");
        let bytes = self
            .client
            .get(&jar_url)
            .send()
            .with_context(|| format!("cannot download {jar_url}"))?
            .error_for_status()
            .with_context(|| format!("download failed: {jar_url}"))?
            .bytes()
            .context("reading jar response body")?;
        let expected = self.fetch_sha1(&format!("{jar_url}.sha1"));
        self.write_cached(artifact, &bytes, expected.as_deref(), &jar_url)
    }

    /// Fetch a `.sha1` file from an HTTP repository, if published.
    fn fetch_sha1(&self, url: &str) -> Option<String> {
        match self.client.get(url).send() {
            Ok(response) if response.status().is_success() => response.text().ok(),
            _ => None,
        }
    }

    /// Write verified jar bytes into the cache: a temporary file first, then
    /// renamed into place so a half-written jar is never mistaken for a
    /// complete one.
    fn write_cached(
        &self,
        artifact: &Artifact,
        bytes: &[u8],
        expected: Option<&str>,
        jar_url: &str,
    ) -> anyhow::Result<PathBuf> {
        let dest = self.cache_jar_path(artifact);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }

        let temp_path = dest.with_extension("jar.tmp");
        fs::write(&temp_path, bytes)?;

        match expected {
            Some(expected) => {
                let actual = hex::encode(Sha1::digest(bytes));
                if !expected.trim().eq_ignore_ascii_case(&actual) {
                    let _ = fs::remove_file(&temp_path);
                    bail!(
                        "checksum mismatch for {jar_url} (expected {}, got {})",
                        expected.trim(),
                        actual
                    );
                }
            }
            // Some repositories do not publish checksums; warn but proceed.
            // Local `file://` repositories are trusted and rarely carry
            // checksums, so they do not warn.
            None => {
                if !jar_url.starts_with("file://") {
                    crate::console::warn(&format!("no SHA-1 checksum available for {jar_url}"));
                }
            }
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

/// Read a text file (such as a POM) from a repository that may be an HTTP
/// URL or a `file://` path to a local Maven-style folder.
pub fn download_repo_text(
    client: &reqwest::blocking::Client,
    repo: &str,
    relative_path: &str,
) -> anyhow::Result<String> {
    if let Some(base_dir) = file_url_path(repo) {
        ensure_repo_dir(&base_dir)?;
        let path = base_dir.join(relative_path);
        return fs::read_to_string(&path)
            .with_context(|| format!("file not found in local repository {path:?}"));
    }
    let url = format!("{repo}/{relative_path}");
    download_text(client, &url)
}

/// Fail fast when a `file://` repository directory does not exist, so the
/// resolution error names the real problem instead of a generic file lookup.
fn ensure_repo_dir(base_dir: &Path) -> anyhow::Result<()> {
    if base_dir.is_dir() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "local repository does not exist at {}",
            base_dir.display()
        ))
    }
}

/// The filesystem path behind a `file://` URL, or `None` for HTTP URLs.
pub(crate) fn file_url_path(repo: &str) -> Option<PathBuf> {
    let rest = repo.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    // `file:///C:/path` -> `C:/path` on Windows.
    if cfg!(windows) {
        let bytes = rest.as_bytes();
        if rest.starts_with('/')
            && rest.len() > 2
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
        {
            return Some(PathBuf::from(&rest[1..]));
        }
    }
    Some(PathBuf::from(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_paths_follow_repository_layout() {
        let cache = Cache::new(reqwest::blocking::Client::new(), false, false);
        let artifact = Artifact::parse("com.google.guava:guava:33.0.0-jre").unwrap();
        assert!(
            cache
                .cache_jar_path(&artifact)
                .ends_with("com/google/guava/guava/33.0.0-jre/guava-33.0.0-jre.jar")
        );
    }

    #[test]
    fn parses_file_urls() {
        assert_eq!(
            file_url_path("file:///srv/repo"),
            Some(PathBuf::from("/srv/repo"))
        );
        assert_eq!(
            file_url_path("file://localhost/srv/repo"),
            Some(PathBuf::from("/srv/repo"))
        );
        assert_eq!(file_url_path("https://repo1.maven.org/maven2"), None);
    }

    #[test]
    fn offline_ensure_jar_fails_for_missing_artifact() {
        let cache = Cache::new(reqwest::blocking::Client::new(), false, true);
        let artifact = Artifact::parse("com.example.nonexistent:forgifact:99.99.99-zzz").unwrap();
        let repos = vec!["https://repo1.maven.org/maven2".to_string()];
        let result = cache.ensure_jar(&artifact, &repos);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("is not cached"));
    }
}
