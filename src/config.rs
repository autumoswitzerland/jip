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
//      [classpath]        # optional: extra classpath entries
//      extra = ["web", "lib/foo.jar"]   # runtime + test (dirs or jars)
//      test-extra = ["src/test/resources"]  # only for `jip test`
//
//      [repositories]     # optional: extra repositories to fetch from
//      "local-repo" = "file:///srv/jars/lib/repo"   # tried before Maven Central
//
//      [dependencies]
//      com.google.guava = "33.0.0-jre"
//
//      [provided-dependencies]   # compile-only (Maven `provided`)
//      jakarta.servlet = "6.1.0"
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
    /// Proxy configuration for HTTP/HTTPS connections.
    #[serde(default, skip_serializing_if = "ProxySettings::is_default")]
    pub proxy: ProxySettings,
    /// Extra classpath entries beyond the resolved dependency jars.
    #[serde(default, skip_serializing_if = "ClasspathSettings::is_empty")]
    pub classpath: ClasspathSettings,
    /// Extra repositories to fetch dependencies from, tried in (key) order
    /// before Maven Central.  Keys are arbitrary names; values are repository
    /// base URLs, either `https://...` or `file://...` for a local folder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repositories: BTreeMap<String, String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    /// Dependencies required to compile but never needed at runtime, from
    /// Maven's `provided` scope or Gradle's `compileOnly`.  They go on the
    /// `javac` classpath of `jip build`/`run`/`test`, never on the classpath
    /// of the running program.
    #[serde(
        default,
        rename = "provided-dependencies",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub provided_dependencies: BTreeMap<String, String>,
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

/// Proxy settings for HTTP/HTTPS connections.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxySettings {
    /// HTTP proxy URL (e.g. `http://proxy:8080`).  Also read from `HTTP_PROXY`
    /// env var when not set here.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "http-proxy"
    )]
    pub http_proxy: Option<String>,
    /// HTTPS proxy URL (e.g. `http://proxy:8080`).  Also read from
    /// `HTTPS_PROXY` env var when not set here.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "https-proxy"
    )]
    pub https_proxy: Option<String>,
}

/// Extra classpath entries, for resources or jars that are not on any
/// repository.  Paths are relative to the project root.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClasspathSettings {
    /// Directories (resources, extra class folders) or `.jar` files added
    /// to the runtime classpath of `jip build`, `jip run`, and `jip test`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<String>,
    /// Directories or `.jar` files added only to the `jip test` classpath.
    #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "test-extra")]
    pub test_extra: Vec<String>,
}

impl ClasspathSettings {
    fn is_empty(&self) -> bool {
        self.extra.is_empty() && self.test_extra.is_empty()
    }
}

impl ProxySettings {
    fn is_default(&self) -> bool {
        self.http_proxy.is_none() && self.https_proxy.is_none()
    }
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
            proxy: ProxySettings::default(),
            classpath: ClasspathSettings::default(),
            repositories: BTreeMap::new(),
            dependencies: BTreeMap::new(),
            provided_dependencies: BTreeMap::new(),
            test_dependencies: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classpath_settings_round_trip() {
        let raw = r#"
            [project]
            name = "demo"

            [classpath]
            extra = ["web", "lib/foo.jar"]
            test-extra = ["src/test/resources"]
        "#;
        let config: ProjectConfig = toml::from_str(raw).unwrap();
        assert_eq!(config.classpath.extra, vec!["web", "lib/foo.jar"]);
        assert_eq!(config.classpath.test_extra, vec!["src/test/resources"]);
        let saved = toml::to_string_pretty(&config).unwrap();
        assert!(saved.contains("extra = ["));
        assert!(saved.contains("test-extra = ["));
    }

    #[test]
    fn empty_classpath_section_is_omitted() {
        let config = ProjectConfig::default_config();
        let saved = toml::to_string_pretty(&config).unwrap();
        assert!(!saved.contains("[classpath]"));
    }

    #[test]
    fn provided_dependencies_round_trip() {
        let raw = r#"
            [project]
            name = "demo"

            [provided-dependencies]
            "jakarta.servlet:jakarta.servlet-api" = "6.1.0"
        "#;
        let config: ProjectConfig = toml::from_str(raw).unwrap();
        assert_eq!(
            config
                .provided_dependencies
                .get("jakarta.servlet:jakarta.servlet-api")
                .unwrap(),
            "6.1.0"
        );
        let saved = toml::to_string_pretty(&config).unwrap();
        assert!(saved.contains("provided-dependencies"));
    }

    #[test]
    fn empty_provided_dependencies_are_omitted() {
        let config = ProjectConfig::default_config();
        let saved = toml::to_string_pretty(&config).unwrap();
        assert!(!saved.contains("provided-dependencies"));
    }

    #[test]
    fn proxy_settings_round_trip() {
        let raw = r#"
            [project]
            name = "demo"

            [proxy]
            http-proxy = "http://proxy:8080"
            https-proxy = "http://proxy:8443"
        "#;
        let config: ProjectConfig = toml::from_str(raw).unwrap();
        assert_eq!(
            config.proxy.http_proxy.as_deref(),
            Some("http://proxy:8080")
        );
        assert_eq!(
            config.proxy.https_proxy.as_deref(),
            Some("http://proxy:8443")
        );
        let saved = toml::to_string_pretty(&config).unwrap();
        assert!(saved.contains("http-proxy"));
        assert!(saved.contains("https-proxy"));
    }

    #[test]
    fn empty_proxy_section_is_omitted() {
        let config = ProjectConfig::default_config();
        let saved = toml::to_string_pretty(&config).unwrap();
        assert!(!saved.contains("[proxy]"));
    }
}
