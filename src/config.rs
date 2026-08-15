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
//  jip — Project Configuration
//  ---------------------------------------------------------------------------
//  Reads and writes the `jip.toml` project file, the jip equivalent of
//  Python's `pyproject.toml` or `requirements.txt`.
//
//  Minimal example:
//
//      [project]
//      name = "hello"
//      java = "21"
//      main = "com.example.App"   # optional: defaults to auto-detection
//      source = "src/main/java"   # optional: defaults to "src/main/java"
//
//      [cache]
//      use-m2 = true      # reuse jars from ~/.m2 when present
//
//      [dependencies]
//      com.google.guava = "33.0.0-jre"
//
//      [test-dependencies]   # only used by `jip test`
//      "org.junit.platform:junit-platform-console-standalone" = "1.13.0-M3"
//
//  Dependency keys are `group:artifact` and values are version strings.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// File name of the project configuration, relative to the working directory.
pub const CONFIG_FILE: &str = "jip.toml";

/// The top-level configuration model for a jip project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(default)]
    pub project: ProjectSettings,
    #[serde(default)]
    pub cache: CacheSettings,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    /// Dependencies that are only used by `jip test` and never leak onto
    /// the runtime classpath (`jip run`/`jip build`).
    #[serde(default, rename = "test-dependencies")]
    pub test_dependencies: BTreeMap<String, String>,
}

/// Project-level settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectSettings {
    /// Display name of the project.
    pub name: Option<String>,
    /// Required Java version as a major number, e.g. "21".
    pub java: Option<String>,
    /// Default entry point for `jip run`: a fully qualified class name, or a
    /// `.java`/`.jar` file for quick starts. When omitted, the class with a
    /// `public static void main` method is detected automatically.
    pub main: Option<String>,
    /// Directory holding the project's `.java` sources, relative to the
    /// project root. Defaults to `src/main/java` (the Maven layout).
    pub source: Option<String>,
}

/// Cache-related settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheSettings {
    /// Reuse jars from the local Maven repository (`~/.m2/repository`)
    /// instead of downloading them again.
    #[serde(default, rename = "use-m2")]
    pub use_m2: bool,
}

impl ProjectConfig {
    /// Load the configuration from `path`, or return a fresh default config
    /// when the file does not exist yet.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if path.exists() {
            let raw = fs::read_to_string(path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            let config: Self =
                toml::from_str(&raw).with_context(|| format!("cannot parse {}", path.display()))?;
            Ok(config)
        } else {
            Ok(Self::default_config())
        }
    }

    /// Save the configuration to `path`, replacing any existing content.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let raw = toml::to_string_pretty(self)?;
        fs::write(path, raw).with_context(|| format!("cannot write {}", path.display()))
    }

    /// A fresh configuration with sensible defaults.
    pub fn default_config() -> Self {
        Self {
            project: ProjectSettings::default(),
            cache: CacheSettings::default(),
            dependencies: BTreeMap::new(),
            test_dependencies: BTreeMap::new(),
        }
    }
}
