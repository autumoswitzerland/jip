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
//  jip — Lock File
//  ---------------------------------------------------------------------------
//  Reads and writes `jip.lock`, the jip equivalent of Python's
//  `pip freeze` output.  It pins the exact versions that were resolved so
//  that every checkout builds the same classpath.
//
//  The lock file is meant to be committed to version control.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::artifact::Artifact;

/// File name of the lock file, relative to the working directory.
pub const LOCK_FILE: &str = "jip.lock";

/// Format version of the lock file.  Bump it when the format changes.
const LOCK_FORMAT_VERSION: u32 = 3;

/// The lock file content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFile {
    pub version: u32,
    pub packages: Vec<LockedPackage>,
    /// Compile-only packages (Maven `provided` scope / Gradle `compileOnly`),
    /// pinned for `javac` but never on the runtime classpath.
    #[serde(default)]
    pub provided_packages: Vec<LockedPackage>,
    /// Test-scoped packages, only used on the `jip test` classpath.
    #[serde(default)]
    pub test_packages: Vec<LockedPackage>,
}

/// A single pinned dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedPackage {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

impl LockedPackage {
    /// The `group:artifact` key this package is pinned under.
    pub fn key(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }

    /// Rebuild an `Artifact` from the stored coordinates.
    pub fn to_artifact(&self) -> Artifact {
        Artifact {
            group: self.group.clone(),
            artifact: self.artifact.clone(),
            version: self.version.clone(),
        }
    }
}

impl LockFile {
    /// Load the lock file, returning `None` when it does not exist yet.
    pub fn load(path: &Path) -> anyhow::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let raw =
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let lock: Self =
            toml::from_str(&raw).with_context(|| format!("cannot parse {}", path.display()))?;
        Ok(Some(lock))
    }

    /// Save the lock file, sorted by coordinates for stable diffs.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let raw = toml::to_string_pretty(self)?;
        fs::write(path, raw).with_context(|| format!("cannot write {}", path.display()))
    }

    /// Build a lock file from flat artifact lists (runtime, compile-only and
    /// test), sorted and de-duplicated.  Compile-only and test packages that
    /// are already pinned as runtime packages are not repeated in their
    /// sections.
    pub fn from_artifacts(
        artifacts: Vec<Artifact>,
        provided_artifacts: Vec<Artifact>,
        test_artifacts: Vec<Artifact>,
    ) -> Self {
        let packages = dedupe(artifacts);
        let runtime_keys: HashSet<String> = packages.iter().map(|package| package.key()).collect();
        let mut provided_packages = dedupe(provided_artifacts);
        let mut test_packages = dedupe(test_artifacts);
        provided_packages.retain(|package| !runtime_keys.contains(&package.key()));
        test_packages.retain(|package| !runtime_keys.contains(&package.key()));
        Self {
            version: LOCK_FORMAT_VERSION,
            packages,
            provided_packages,
            test_packages,
        }
    }
}

/// Build a sorted, de-duplicated package list from a flat artifact list.
fn dedupe(artifacts: Vec<Artifact>) -> Vec<LockedPackage> {
    let mut seen: BTreeMap<String, LockedPackage> = BTreeMap::new();
    for artifact in artifacts {
        seen.entry(artifact.key()).or_insert(LockedPackage {
            group: artifact.group,
            artifact: artifact.artifact,
            version: artifact.version,
        });
    }
    seen.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(group: &str, artifact: &str, version: &str) -> Artifact {
        Artifact {
            group: group.to_string(),
            artifact: artifact.to_string(),
            version: version.to_string(),
        }
    }

    #[test]
    fn provided_and_test_packages_not_repeated_in_runtime() {
        let lock = LockFile::from_artifacts(
            vec![artifact("com.example", "runtime-dep", "1.0")],
            vec![artifact("jakarta.servlet", "jakarta.servlet-api", "6.1.0")],
            vec![artifact("junit", "junit", "4.13.2")],
        );
        assert_eq!(lock.version, 3);
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(lock.provided_packages.len(), 1);
        assert_eq!(lock.test_packages.len(), 1);
        assert_eq!(
            lock.provided_packages[0].key(),
            "jakarta.servlet:jakarta.servlet-api"
        );
    }

    #[test]
    fn runtime_dependency_never_repeated_in_provided_or_test() {
        let lock = LockFile::from_artifacts(
            vec![artifact("com.example", "shared", "1.0")],
            vec![artifact("com.example", "shared", "1.0")],
            vec![artifact("com.example", "shared", "1.0")],
        );
        assert_eq!(lock.packages.len(), 1);
        assert!(lock.provided_packages.is_empty());
        assert!(lock.test_packages.is_empty());
    }
}
