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
//  jip — Maven / Gradle Conversion
//  ---------------------------------------------------------------------------
//  Turns an existing Maven or Gradle project into `jip.toml`, so switching
//  to jip does not mean re-typing every dependency.
//
//  Supported inputs:
//    * pom.xml              — reads <dependencies> plus <dependencyManagement>.
//    * build.gradle         — Groovy DSL.
//    * build.gradle.kts     — Kotlin DSL.
//
//  Only dependencies that end up on the runtime classpath are converted;
//  test, provided, and optional dependencies are skipped.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, bail};
use regex::Regex;

use crate::central;
use crate::commands::build::{self, MainTarget};
use crate::commands::{resolve, write_lock};
use crate::config::{CONFIG_FILE, CacheSettings, ProjectConfig, ProjectSettings};
use crate::lock::LOCK_FILE;
use crate::pom::{is_runtime_dependency, parse_pom};

/// The kind of build system a project uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectType {
    /// A Maven project (`pom.xml`).
    Maven,
    /// A Gradle project with a Groovy build script (`build.gradle`).
    GradleGroovy,
    /// A Gradle project with a Kotlin build script (`build.gradle.kts`).
    GradleKotlin,
}

impl std::fmt::Display for ProjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectType::Maven => write!(f, "Maven"),
            ProjectType::GradleGroovy => write!(f, "Gradle (Groovy)"),
            ProjectType::GradleKotlin => write!(f, "Gradle (Kotlin)"),
        }
    }
}

/// A direct dependency of the existing project.
#[derive(Debug, Clone)]
pub struct ConvertedDependency {
    pub group: String,
    pub artifact: String,
    /// `None` when no version could be determined (see `jip init` for the fallback).
    pub version: Option<String>,
}

impl ConvertedDependency {
    pub fn key(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }
}

/// Detect the build system from the files in the current directory.
pub fn detect() -> Option<ProjectType> {
    if Path::new("pom.xml").exists() {
        Some(ProjectType::Maven)
    } else if Path::new("build.gradle").exists() {
        Some(ProjectType::GradleGroovy)
    } else if Path::new("build.gradle.kts").exists() {
        Some(ProjectType::GradleKotlin)
    } else {
        None
    }
}

/// Read and convert all dependencies of the detected project.
pub fn collect_dependencies(project_type: ProjectType) -> anyhow::Result<Vec<ConvertedDependency>> {
    match project_type {
        ProjectType::Maven => {
            let xml = fs::read_to_string("pom.xml").context("cannot read pom.xml")?;
            maven_dependencies_from_xml(&xml)
        }
        ProjectType::GradleGroovy => {
            let content = fs::read_to_string("build.gradle").context("cannot read build.gradle")?;
            gradle_dependencies_from_content(&content)
        }
        ProjectType::GradleKotlin => {
            let content =
                fs::read_to_string("build.gradle.kts").context("cannot read build.gradle.kts")?;
            gradle_dependencies_from_content(&content)
        }
    }
}

/// Extract the runtime dependencies from a `pom.xml`.
///
/// Versions are taken from the `<dependency>` element or, when missing,
/// from `<dependencyManagement>` in the same POM.  `${...}` placeholders
/// are substituted using the POM's own properties.
pub fn maven_dependencies_from_xml(xml: &str) -> anyhow::Result<Vec<ConvertedDependency>> {
    let pom = parse_pom(xml)?;

    // Built-in properties Maven always provides, plus the POM's own ones.
    let mut properties = HashMap::new();
    if let Some(group) = &pom.group_id {
        properties.insert("project.groupId".to_string(), group.clone());
    }
    if let Some(name) = &pom.artifact_id {
        properties.insert("project.artifactId".to_string(), name.clone());
    }
    if let Some(version) = &pom.version {
        properties.insert("project.version".to_string(), version.clone());
    }
    for (key, value) in &pom.properties {
        properties.insert(key.clone(), value.clone());
    }

    // Versions for dependencies that rely on <dependencyManagement>.
    let mut managed = HashMap::new();
    for dep in &pom.managed_dependencies {
        if let Some(version) = &dep.version {
            managed.insert(
                (
                    crate::resolver::interpolate(&dep.group_id, &properties),
                    crate::resolver::interpolate(&dep.artifact_id, &properties),
                ),
                crate::resolver::interpolate(version, &properties),
            );
        }
    }

    let mut result = Vec::new();
    for dep in &pom.dependencies {
        if !is_runtime_dependency(dep) {
            continue;
        }
        let group = crate::resolver::interpolate(&dep.group_id, &properties);
        let artifact = crate::resolver::interpolate(&dep.artifact_id, &properties);
        let version = dep
            .version
            .as_deref()
            .map(|v| crate::resolver::interpolate(v, &properties))
            .or_else(|| managed.get(&(group.clone(), artifact.clone())).cloned());
        result.push(ConvertedDependency {
            group,
            artifact,
            // A leftover placeholder (e.g. ${revision}) means the version is
            // unknown without the parent POM; leave it for the lookup step.
            version: version.filter(|v| !v.contains("${")),
        });
    }
    Ok(result)
}

/// Gradle configurations that contribute to the runtime classpath.
const RUNTIME_CONFIGURATIONS: [&str; 4] = ["implementation", "api", "runtimeOnly", "compile"];

/// Extract the runtime dependencies from a `build.gradle` or `build.gradle.kts`.
///
/// Two common declaration styles are recognised:
///   * `implementation 'group:artifact:version'` (and the `(...)` variant)
///   * `implementation group: 'g', name: 'a', version: 'v'` / `(group = "...", ...)`
///
/// Multi-line declarations and version catalogs (`libs.versions.*`) are not
/// supported and are simply skipped.
pub fn gradle_dependencies_from_content(content: &str) -> anyhow::Result<Vec<ConvertedDependency>> {
    // Single-line "group:artifact:version" style, e.g.
    //   implementation 'com.google.guava:guava:33.0.0-jre'
    //   implementation("com.google.guava:guava:33.0.0-jre")
    let shorthand = Regex::new(
        r#"^\s*(implementation|api|runtimeOnly|compile)\s*(\(\s*)?['"]([^:'"]+):([^:'"]+):([^'"]+)['"]"#,
    )?;

    // Groovy named-argument style.
    let groovy_named = Regex::new(
        r#"^\s*(implementation|api|runtimeOnly|compile)\s*(\(\s*)?group:\s*['"]([^'"]+)['"],\s*name:\s*['"]([^'"]+)['"],\s*version:\s*['"]([^'"]+)['"]"#,
    )?;

    // Kotlin named-argument style (double quotes only).
    let kotlin_named = Regex::new(
        r#"^\s*(implementation|api|runtimeOnly|compile)\s*\(\s*group\s*=\s*"([^"]+)",\s*name\s*=\s*"([^"]+)",\s*version\s*=\s*"([^"]+)""#,
    )?;

    let mut result = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") {
            continue;
        }
        if let Some(dep) = match_shorthand(trimmed, &shorthand) {
            result.push(dep);
            continue;
        }
        if let Some(dep) = match_named(trimmed, &groovy_named, &kotlin_named) {
            result.push(dep);
        }
    }
    Ok(result)
}

/// Match a single-line `group:artifact:version` declaration.
fn match_shorthand(line: &str, re: &Regex) -> Option<ConvertedDependency> {
    let caps = re.captures(line)?;
    let configuration = caps.get(1)?.as_str();
    if !RUNTIME_CONFIGURATIONS.contains(&configuration) {
        return None;
    }
    Some(convert(
        caps.get(3)?.as_str(),
        caps.get(4)?.as_str(),
        caps.get(5)?.as_str(),
    ))
}

/// Match a `group:`, `name:`, `version:` declaration in either DSL.
fn match_named(line: &str, groovy: &Regex, kotlin: &Regex) -> Option<ConvertedDependency> {
    if let Some(caps) = groovy.captures(line) {
        return Some(convert(
            caps.get(3)?.as_str(),
            caps.get(4)?.as_str(),
            caps.get(5)?.as_str(),
        ));
    }
    let caps = kotlin.captures(line)?;
    Some(convert(
        caps.get(2)?.as_str(),
        caps.get(3)?.as_str(),
        caps.get(4)?.as_str(),
    ))
}

/// Build a dependency from captured strings, guarding against version
/// catalogs and placeholder versions.
fn convert(group: &str, artifact: &str, version: &str) -> ConvertedDependency {
    let version = version.trim();
    let looks_like_catalog = version.contains('$')
        || version.starts_with("libs.")
        || version.contains(char::is_whitespace);
    ConvertedDependency {
        group: group.trim().to_string(),
        artifact: artifact.trim().to_string(),
        version: (!looks_like_catalog && !version.is_empty()).then(|| version.to_string()),
    }
}

/// The result of asking the user whether to convert a detected project.
pub enum ConversionOffer {
    /// The project was converted; use this fresh configuration.
    Converted(ProjectConfig),
    /// Nothing to convert — proceed with the usual defaults.
    Proceed,
    /// A project was detected but the user declined; stop here.
    Declined,
}

/// When no `jip.toml` exists and a Maven/Gradle project is detected, offer
/// to convert it before `jip run` / `jip build` continue.
///
/// Interactive only: without a terminal the offer is skipped and an error
/// points at `jip init` instead, so CI runs never hang on a prompt.
pub fn offer_conversion(client: &reqwest::blocking::Client) -> anyhow::Result<ConversionOffer> {
    if Path::new(CONFIG_FILE).exists() {
        return Ok(ConversionOffer::Proceed);
    }
    let Some(project_type) = detect() else {
        return Ok(ConversionOffer::Proceed);
    };

    if !std::io::stdin().is_terminal() {
        bail!("no {CONFIG_FILE} found — run `jip init` to convert the {project_type} project");
    }

    print!("jip: detected {project_type} project — convert to {CONFIG_FILE} and continue? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        println!("ok — leaving the project untouched");
        return Ok(ConversionOffer::Declined);
    }

    let config = convert_project(client)?;
    println!("converted {project_type} project — created {CONFIG_FILE} and {LOCK_FILE}");
    Ok(ConversionOffer::Converted(config))
}

/// Build a fresh `jip.toml` (and lock file) for the current directory,
/// converting the detected Maven/Gradle dependencies when present.
pub fn convert_project(client: &reqwest::blocking::Client) -> anyhow::Result<ProjectConfig> {
    let mut config = ProjectConfig {
        project: ProjectSettings {
            name: Some(current_directory_name()),
            java: Some("21".to_string()),
            main: None,
            source: None,
        },
        cache: CacheSettings::default(),
        dependencies: BTreeMap::new(),
        test_dependencies: BTreeMap::new(),
    };

    if let Some(project_type) = detect() {
        let deps = collect_dependencies(project_type)?;
        config.dependencies = convert_to_config(client, deps)?;
        println!(
            "converted {project_type} project — {} dependencies",
            config.dependencies.len()
        );
    }

    // Remember an existing main class so `jip run` just works.
    match build::main_target(&config, None) {
        Ok(Some(target)) => {
            config.project.main = Some(main_value(target));
        }
        Ok(None) => {}
        Err(err) => println!("  warning: {err}"),
    }

    config.save(Path::new(CONFIG_FILE))?;

    let resolution = resolve(client, &config)?;
    write_lock(&resolution.flat, &[])?;
    Ok(config)
}

/// Turn converted dependencies into the `group:artifact` -> version map.
///
/// Dependencies without a known version are looked up on Maven Central;
/// if that fails too, they are skipped with a warning.
fn convert_to_config(
    client: &reqwest::blocking::Client,
    deps: Vec<ConvertedDependency>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for dep in deps {
        let version = match &dep.version {
            Some(version) => version.clone(),
            None => match central::latest_version(client, &dep.group, &dep.artifact)? {
                Some(latest) => {
                    println!("  {}: using latest version {latest}", dep.key());
                    latest
                }
                None => {
                    println!("  warning: no version found for {} — skipped", dep.key());
                    continue;
                }
            },
        };
        result.insert(dep.key(), version);
    }
    Ok(result)
}

/// The name of the directory jip was invoked in.
fn current_directory_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|dir| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "project".to_string())
}

/// The `[project] main` value for a detected run target.
fn main_value(target: MainTarget) -> String {
    match target {
        MainTarget::SourceFile(path) | MainTarget::Jar(path) => path.to_string_lossy().into_owned(),
        MainTarget::Class(fqcn) => fqcn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_conversion_uses_explicit_versions() {
        let xml = r#"
            <project xmlns="http://maven.apache.org/POM/4.0.0">
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <properties><guava.version>33.0.0-jre</guava.version></properties>
              <dependencies>
                <dependency>
                  <groupId>com.google.guava</groupId>
                  <artifactId>guava</artifactId>
                  <version>${guava.version}</version>
                </dependency>
                <dependency>
                  <groupId>junit</groupId>
                  <artifactId>junit</artifactId>
                  <version>4.13.2</version>
                  <scope>test</scope>
                </dependency>
              </dependencies>
            </project>
        "#;
        let deps = maven_dependencies_from_xml(xml).unwrap();
        assert_eq!(deps.len(), 1); // junit (test scope) is skipped
        assert_eq!(deps[0].key(), "com.google.guava:guava");
        assert_eq!(deps[0].version.as_deref(), Some("33.0.0-jre"));
    }

    #[test]
    fn maven_conversion_uses_dependency_management() {
        let xml = r#"
            <project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <dependencyManagement>
                <dependencies>
                  <dependency>
                    <groupId>org.slf4j</groupId>
                    <artifactId>slf4j-api</artifactId>
                    <version>2.0.13</version>
                  </dependency>
                </dependencies>
              </dependencyManagement>
              <dependencies>
                <dependency>
                  <groupId>org.slf4j</groupId>
                  <artifactId>slf4j-api</artifactId>
                </dependency>
              </dependencies>
            </project>
        "#;
        let deps = maven_dependencies_from_xml(xml).unwrap();
        assert_eq!(deps[0].version.as_deref(), Some("2.0.13"));
    }

    #[test]
    fn gradle_groovy_conversion() {
        let script = r#"
            plugins { id 'java' }
            dependencies {
                implementation 'com.google.guava:guava:33.0.0-jre'
                api "org.slf4j:slf4j-api:2.0.13"
                testImplementation 'junit:junit:4.13.2'
                compileOnly 'org.projectlombok:lombok:1.18.30'
                implementation group: 'com.fasterxml.jackson.core', name: 'jackson-databind', version: '2.17.0'
            }
        "#;
        let deps = gradle_dependencies_from_content(script).unwrap();
        let keys: Vec<String> = deps.iter().map(|d| d.key()).collect();
        assert_eq!(
            keys,
            vec![
                "com.google.guava:guava",
                "org.slf4j:slf4j-api",
                "com.fasterxml.jackson.core:jackson-databind",
            ]
        );
    }

    #[test]
    fn gradle_kotlin_conversion() {
        let script = r#"
            dependencies {
                implementation("com.google.guava:guava:33.0.0-jre")
                runtimeOnly("org.slf4j:slf4j-simple:2.0.13") { exclude(group = "x") }
                testImplementation("junit:junit:4.13.2")
                implementation(group = "com.fasterxml.jackson.core", name = "jackson-databind", version = "2.17.0")
            }
        "#;
        let deps = gradle_dependencies_from_content(script).unwrap();
        let keys: Vec<String> = deps.iter().map(|d| d.key()).collect();
        assert_eq!(
            keys,
            vec![
                "com.google.guava:guava",
                "org.slf4j:slf4j-simple",
                "com.fasterxml.jackson.core:jackson-databind",
            ]
        );
    }
}
