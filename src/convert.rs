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
//    * pom.xml              — reads <dependencies>, <dependencyManagement>,
//                             and <repositories>.
//    * build.gradle         — Groovy DSL.
//    * build.gradle.kts     — Kotlin DSL.
//
//  Runtime dependencies land in `[dependencies]`, test-scope dependencies
//  in `[test-dependencies]`; provided, optional, and system dependencies
//  are skipped.  Custom Maven repositories are converted into the
//  `[repositories]` section.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-16
// =============================================================================

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, bail};
use regex::Regex;

use crate::central;
use crate::commands::build::{self, MainDecision, MainTarget};
use crate::commands::{resolve, resolve_provided, resolve_tests, write_lock};
use crate::config::{CONFIG_FILE, CacheSettings, ProjectConfig, ProjectSettings};
use crate::lock::LOCK_FILE;
use crate::pom::{Pom, PomDependency, is_runtime_dependency, parse_pom};

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

/// Everything extracted from an existing build system.
#[derive(Debug, Clone)]
pub struct ConvertedDependencies {
    /// Dependencies that land on the runtime classpath.
    pub runtime: Vec<ConvertedDependency>,
    /// Compile-only dependencies (Maven `provided`, Gradle `compileOnly`).
    pub provided: Vec<ConvertedDependency>,
    /// Test-scope dependencies, for `[test-dependencies]`.
    pub test: Vec<ConvertedDependency>,
    /// Custom repositories as `id -> url`, for `[repositories]`.
    pub repositories: BTreeMap<String, String>,
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
pub fn collect_dependencies(project_type: ProjectType) -> anyhow::Result<ConvertedDependencies> {
    match project_type {
        ProjectType::Maven => {
            let xml = fs::read_to_string("pom.xml").context("cannot read pom.xml")?;
            let pom = parse_pom(&xml)?;
            Ok(ConvertedDependencies {
                runtime: maven_dependencies_from_xml(&xml)?,
                provided: maven_provided_dependencies_from_xml(&xml)?,
                test: maven_test_dependencies_from_xml(&xml)?,
                repositories: maven_repositories_from_xml(&pom),
            })
        }
        ProjectType::GradleGroovy => {
            let content = fs::read_to_string("build.gradle").context("cannot read build.gradle")?;
            let (runtime, test) = gradle_dependencies_from_content(&content)?;
            Ok(ConvertedDependencies {
                runtime,
                provided: gradle_provided_dependencies_from_content(&content)?,
                test,
                repositories: BTreeMap::new(),
            })
        }
        ProjectType::GradleKotlin => {
            let content =
                fs::read_to_string("build.gradle.kts").context("cannot read build.gradle.kts")?;
            let (runtime, test) = gradle_dependencies_from_content(&content)?;
            Ok(ConvertedDependencies {
                runtime,
                provided: gradle_provided_dependencies_from_content(&content)?,
                test,
                repositories: BTreeMap::new(),
            })
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
    let (properties, managed) = maven_context(&pom);
    Ok(collect_scope(&pom, &properties, &managed, |dep| {
        is_runtime_dependency(dep)
    }))
}

/// Extract the `provided`-scope dependencies, which are required to compile
/// but never land on the runtime classpath.
pub fn maven_provided_dependencies_from_xml(xml: &str) -> anyhow::Result<Vec<ConvertedDependency>> {
    let pom = parse_pom(xml)?;
    let (properties, managed) = maven_context(&pom);
    Ok(collect_scope(&pom, &properties, &managed, |dep| {
        dep.scope == "provided"
    }))
}

/// Extract the test-scope dependencies from a `pom.xml` the same way as the
/// runtime ones, e.g. `junit:junit:4.13.2`.
pub fn maven_test_dependencies_from_xml(xml: &str) -> anyhow::Result<Vec<ConvertedDependency>> {
    let pom = parse_pom(xml)?;
    let (properties, managed) = maven_context(&pom);
    Ok(collect_scope(&pom, &properties, &managed, |dep| {
        dep.scope == "test"
    }))
}

/// The properties and managed versions a Maven project inherits.
///
/// Built-in properties Maven always provides, plus the POM's own ones, plus
/// the effective versions from `<dependencyManagement>`.
fn maven_context(pom: &Pom) -> (HashMap<String, String>, HashMap<(String, String), String>) {
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
    (properties, managed)
}

/// Collect the dependencies that pass `keep`, resolving versions and
/// substituting `${...}` placeholders.
fn collect_scope(
    pom: &Pom,
    properties: &HashMap<String, String>,
    managed: &HashMap<(String, String), String>,
    keep: impl Fn(&PomDependency) -> bool,
) -> Vec<ConvertedDependency> {
    let mut result = Vec::new();
    for dep in &pom.dependencies {
        if !keep(dep) {
            continue;
        }
        let group = crate::resolver::interpolate(&dep.group_id, properties);
        let artifact = crate::resolver::interpolate(&dep.artifact_id, properties);
        let version = dep
            .version
            .as_deref()
            .map(|v| crate::resolver::interpolate(v, properties))
            .or_else(|| managed.get(&(group.clone(), artifact.clone())).cloned());
        result.push(ConvertedDependency {
            group,
            artifact,
            // A leftover placeholder (e.g. ${revision}) means the version is
            // unknown without the parent POM; leave it for the lookup step.
            version: version.filter(|v| !v.contains("${")),
        });
    }
    result
}

/// Convert the custom `<repositories>` of a Maven project, resolving the
/// built-in `${project.basedir}` property (and any POM property that builds
/// on it) against the directory jip is running in.
pub fn maven_repositories_from_xml(pom: &Pom) -> BTreeMap<String, String> {
    let (mut properties, _) = maven_context(pom);
    let basedir = std::env::current_dir()
        .ok()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    properties.insert("project.basedir".to_string(), basedir.clone());
    properties.insert("basedir".to_string(), basedir);

    let mut repositories = BTreeMap::new();
    for (id, url) in &pom.repositories {
        let url = crate::resolver::interpolate(url, &properties);
        if !url.contains("${") && !url.is_empty() {
            repositories.insert(id.clone(), url);
        }
    }
    repositories
}

/// Gradle configurations that contribute to the runtime classpath.
const RUNTIME_CONFIGURATIONS: [&str; 4] = ["implementation", "api", "runtimeOnly", "compile"];

/// Gradle configurations that contribute to the test classpath only.
const TEST_CONFIGURATIONS: [&str; 2] = ["testImplementation", "testCompile"];

/// Gradle configurations that are compile-only (`provided` in Maven terms).
const COMPILE_ONLY_CONFIGURATIONS: [&str; 2] = ["compileOnly", "compileOnlyApi"];

/// Extract the dependencies from a `build.gradle` or `build.gradle.kts`.
///
/// Two common declaration styles are recognised:
///   * `implementation 'group:artifact:version'` (and the `(...)` variant)
///   * `implementation group: 'g', name: 'a', version: 'v'` / `(group = "...", ...)`
///
/// The return value is `(runtime, test)` split by configuration.
/// Multi-line declarations and version catalogs (`libs.versions.*`) are not
/// supported and are simply skipped.
pub fn gradle_dependencies_from_content(
    content: &str,
) -> anyhow::Result<(Vec<ConvertedDependency>, Vec<ConvertedDependency>)> {
    // Single-line "group:artifact:version" style, e.g.
    //   implementation 'com.google.guava:guava:33.0.0-jre'
    //   testImplementation("org.junit:junit-bom:5.10.0")
    let configurations = RUNTIME_CONFIGURATIONS
        .iter()
        .chain(&TEST_CONFIGURATIONS)
        .copied()
        .collect::<Vec<_>>()
        .join("|");
    let shorthand = Regex::new(&format!(
        r#"^\s*({configurations})\s*(\(\s*)?['"]([^:'"]+):([^:'"]+):([^'"]+)['"]"#
    ))?;

    // Groovy named-argument style.
    let groovy_named = Regex::new(
        r#"^\s*(implementation|api|runtimeOnly|compile)\s*(\(\s*)?group:\s*['"]([^'"]+)['"],\s*name:\s*['"]([^'"]+)['"],\s*version:\s*['"]([^'"]+)['"]"#,
    )?;

    // Kotlin named-argument style (double quotes only).
    let kotlin_named = Regex::new(
        r#"^\s*(implementation|api|runtimeOnly|compile)\s*\(\s*group\s*=\s*"([^"]+)",\s*name\s*=\s*"([^"]+)",\s*version\s*=\s*"([^"]+)""#,
    )?;

    let mut runtime = Vec::new();
    let mut test = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") {
            continue;
        }
        if let Some((dep, is_test)) = match_shorthand(trimmed, &shorthand) {
            if is_test {
                test.push(dep);
            } else {
                runtime.push(dep);
            }
            continue;
        }
        if let Some(dep) = match_named(trimmed, &groovy_named, &kotlin_named) {
            runtime.push(dep);
        }
    }
    Ok((runtime, test))
}

/// Extract `compileOnly` dependencies from a `build.gradle`/`.kts`, Gradle's
/// compile-only configuration.  Only the shorthand
/// `compileOnly 'group:artifact:version'` style is recognised.
pub fn gradle_provided_dependencies_from_content(
    content: &str,
) -> anyhow::Result<Vec<ConvertedDependency>> {
    let configurations = COMPILE_ONLY_CONFIGURATIONS.join("|");
    let shorthand = Regex::new(&format!(
        r#"^\s*({configurations})\s*(\(\s*)?['"]([^:'"]+):([^:'"]+):([^'"]+)['"]"#
    ))?;
    let mut provided = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") {
            continue;
        }
        if let Some(caps) = shorthand.captures(trimmed)
            && let (Some(group), Some(artifact), Some(version)) =
                (caps.get(3), caps.get(4), caps.get(5))
        {
            provided.push(convert(group.as_str(), artifact.as_str(), version.as_str()));
        }
    }
    Ok(provided)
}

/// Match a single-line `group:artifact:version` declaration, reporting
/// whether the configuration is a test one.
fn match_shorthand(line: &str, re: &Regex) -> Option<(ConvertedDependency, bool)> {
    let caps = re.captures(line)?;
    let configuration = caps.get(1)?.as_str();
    let is_test = TEST_CONFIGURATIONS.contains(&configuration);
    Some((
        convert(
            caps.get(3)?.as_str(),
            caps.get(4)?.as_str(),
            caps.get(5)?.as_str(),
        ),
        is_test,
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
    Converted(Box<ProjectConfig>),
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
        println!(
            "{} — leaving the project untouched",
            crate::console::green("ok")
        );
        return Ok(ConversionOffer::Declined);
    }

    let config = convert_project(client)?;
    println!(
        "{} — created {CONFIG_FILE} and {LOCK_FILE}",
        crate::console::green(&format!("converted {project_type} project"))
    );
    Ok(ConversionOffer::Converted(Box::new(config)))
}

/// Build a fresh `jip.toml` (and lock file) for the current directory,
/// converting the detected Maven/Gradle dependencies when present.
pub fn convert_project(client: &reqwest::blocking::Client) -> anyhow::Result<ProjectConfig> {
    let project_type = detect();
    let mut config = ProjectConfig {
        project: ProjectSettings {
            name: Some(current_directory_name()),
            java: Some(java_default_for(project_type)),
            main: None,
            source: None,
        },
        cache: CacheSettings::default(),
        classpath: crate::config::ClasspathSettings::default(),
        repositories: BTreeMap::new(),
        dependencies: BTreeMap::new(),
        provided_dependencies: BTreeMap::new(),
        test_dependencies: BTreeMap::new(),
    };

    if let Some(project_type) = project_type {
        let converted = collect_dependencies(project_type)?;
        let repos = crate::commands::repositories_for(&config);
        let provided_count = converted.provided.len();
        config.dependencies = convert_to_config(client, &repos, converted.runtime)?;
        config.provided_dependencies = convert_to_config(client, &repos, converted.provided)?;
        config.test_dependencies = convert_to_config(client, &repos, converted.test)?;
        config.repositories = converted.repositories;
        let summary = if provided_count == 0 {
            format!(
                "converted {project_type} project — {} dependencies",
                config.dependencies.len()
            )
        } else {
            format!(
                "converted {project_type} project — {} dependencies, {provided_count} provided",
                config.dependencies.len()
            )
        };
        println!("{}", crate::console::green(&summary));
    }

    // Remember an existing main class so `jip run` just works.
    match build::resolve_main(&config, None) {
        Ok(MainDecision::Run(target)) => {
            config.project.main = Some(main_value(target));
        }
        Ok(MainDecision::Multiple(candidates)) => {
            if std::io::stdin().is_terminal() {
                if let Ok(target) = build::choose_main(&candidates) {
                    config.project.main = Some(main_value(target));
                }
            } else {
                println!(
                    "  {} {}",
                    crate::console::yellow("warning:"),
                    build::multiple_main_error(&candidates).replace('\n', "\n  ")
                );
            }
        }
        Ok(MainDecision::None) => {}
        Err(err) => println!("  {} {err}", crate::console::yellow("warning:")),
    }

    config.save(Path::new(CONFIG_FILE))?;

    let resolution = resolve(client, &config)?;
    let provided = resolve_provided(client, &config)?;
    let tests = resolve_tests(client, &config)?;
    write_lock(&resolution.flat, &provided.flat, &tests.flat)?;
    Ok(config)
}

/// The Java version jip writes into a fresh `jip.toml`: the project's own
/// value when declared (Maven), otherwise the installed JDK on `PATH`.
fn java_default_for(project_type: Option<ProjectType>) -> String {
    if matches!(project_type, Some(ProjectType::Maven))
        && let Ok(xml) = fs::read_to_string("pom.xml")
        && let Ok(Some(version)) = maven_java_version_from_xml(&xml)
    {
        return version;
    }
    installed_java_default()
}

/// The major version of the JDK on `PATH`, or a conservative fallback.
fn installed_java_default() -> String {
    crate::commands::java_major_version()
        .map(|major| major.to_string())
        .unwrap_or_else(|_| "21".to_string())
}

/// The Java version a Maven project compiles for, taken from the
/// `maven-compiler-plugin` configuration (`<release>`, then `<source>`) or
/// the `maven.compiler.*` / `java.version` properties.  Placeholders are
/// resolved against the POM's own properties.  Returns `None` when the POM
/// does not pin a Java version.
pub fn maven_java_version_from_xml(xml: &str) -> anyhow::Result<Option<String>> {
    let pom = parse_pom(xml)?;
    let (properties, _) = maven_context(&pom);
    let resolve = |value: &String| -> Option<String> {
        let resolved = crate::resolver::interpolate(value, &properties);
        (!resolved.is_empty()).then_some(resolved)
    };

    let from_plugin = pom
        .compiler_release
        .as_ref()
        .and_then(resolve)
        .or_else(|| pom.compiler_source.as_ref().and_then(resolve));
    let from_properties = properties
        .get("maven.compiler.release")
        .and_then(resolve)
        .or_else(|| properties.get("maven.compiler.source").and_then(resolve))
        .or_else(|| properties.get("java.version").and_then(resolve));

    Ok(from_plugin
        .or(from_properties)
        .map(|version| normalize_java_major(&version)))
}

/// Normalize a Java version to its major number, e.g. `17.0.1` -> 17 and
/// `1.8` -> 8.  Falls back to the original value when it cannot be parsed.
fn normalize_java_major(version: &str) -> String {
    crate::commands::parse_major(version)
        .map(|major| major.to_string())
        .unwrap_or_else(|_| version.to_string())
}

/// Turn converted dependencies into the `group:artifact` -> version map.
///
/// Dependencies without a known version are looked up on the repositories;
/// if that fails too, they are skipped with a warning.
fn convert_to_config(
    client: &reqwest::blocking::Client,
    repos: &[String],
    deps: Vec<ConvertedDependency>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for dep in deps {
        let version = match &dep.version {
            Some(version) => version.clone(),
            None => match central::latest_version(client, repos, &dep.group, &dep.artifact)? {
                Some(latest) => {
                    println!("  {}: using latest version {latest}", dep.key());
                    latest
                }
                None => {
                    println!(
                        "  {} no version found for {} — skipped",
                        crate::console::yellow("warning:"),
                        dep.key()
                    );
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
        assert_eq!(deps.len(), 1); // junit (test scope) is not a runtime dep
        assert_eq!(deps[0].key(), "com.google.guava:guava");
        assert_eq!(deps[0].version.as_deref(), Some("33.0.0-jre"));

        let test_deps = maven_test_dependencies_from_xml(xml).unwrap();
        assert_eq!(test_deps.len(), 1);
        assert_eq!(test_deps[0].key(), "junit:junit");
        assert_eq!(test_deps[0].version.as_deref(), Some("4.13.2"));
    }

    #[test]
    fn maven_conversion_carries_custom_repositories() {
        let xml = r#"
            <project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <properties>
                <localrepo>${project.basedir}/lib/repo</localrepo>
              </properties>
              <repositories>
                <repository>
                  <id>local-repo</id>
                  <url>file://${localrepo}</url>
                </repository>
              </repositories>
            </project>
        "#;
        let pom = parse_pom(xml).unwrap();
        let repos = maven_repositories_from_xml(&pom);
        assert_eq!(repos.len(), 1);
        let url = &repos["local-repo"];
        assert!(url.starts_with("file:///"));
        assert!(url.ends_with("/lib/repo"));
        assert!(!url.contains("${"));
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
    fn maven_java_version_from_compiler_release() {
        let xml = r#"
            <project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <build>
                <plugins>
                  <plugin>
                    <artifactId>maven-compiler-plugin</artifactId>
                    <configuration>
                      <release>17</release>
                    </configuration>
                  </plugin>
                </plugins>
              </build>
            </project>
        "#;
        assert_eq!(
            maven_java_version_from_xml(xml).unwrap().as_deref(),
            Some("17")
        );
    }

    #[test]
    fn maven_java_version_prefers_release_over_source() {
        let xml = r#"
            <project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <build>
                <plugins>
                  <plugin>
                    <artifactId>maven-compiler-plugin</artifactId>
                    <configuration>
                      <source>11</source>
                      <release>17</release>
                    </configuration>
                  </plugin>
                </plugins>
              </build>
            </project>
        "#;
        assert_eq!(
            maven_java_version_from_xml(xml).unwrap().as_deref(),
            Some("17")
        );
    }

    #[test]
    fn maven_java_version_from_properties() {
        let xml = r#"
            <project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <properties>
                <maven.compiler.release>17</maven.compiler.release>
              </properties>
            </project>
        "#;
        assert_eq!(
            maven_java_version_from_xml(xml).unwrap().as_deref(),
            Some("17")
        );
    }

    #[test]
    fn maven_java_version_resolves_placeholders() {
        let xml = r#"
            <project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <properties>
                <java.version>21</java.version>
              </properties>
              <build>
                <plugins>
                  <plugin>
                    <artifactId>maven-compiler-plugin</artifactId>
                    <configuration>
                      <release>${java.version}</release>
                    </configuration>
                  </plugin>
                </plugins>
              </build>
            </project>
        "#;
        assert_eq!(
            maven_java_version_from_xml(xml).unwrap().as_deref(),
            Some("21")
        );
    }

    #[test]
    fn maven_java_version_without_configuration_is_none() {
        let xml = r#"
            <project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
            </project>
        "#;
        assert_eq!(maven_java_version_from_xml(xml).unwrap(), None);
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
        let (runtime, test) = gradle_dependencies_from_content(script).unwrap();
        let runtime_keys: Vec<String> = runtime.iter().map(|d| d.key()).collect();
        assert_eq!(
            runtime_keys,
            vec![
                "com.google.guava:guava",
                "org.slf4j:slf4j-api",
                "com.fasterxml.jackson.core:jackson-databind",
            ]
        );
        let test_keys: Vec<String> = test.iter().map(|d| d.key()).collect();
        assert_eq!(test_keys, vec!["junit:junit"]);
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
        let (runtime, test) = gradle_dependencies_from_content(script).unwrap();
        let runtime_keys: Vec<String> = runtime.iter().map(|d| d.key()).collect();
        assert_eq!(
            runtime_keys,
            vec![
                "com.google.guava:guava",
                "org.slf4j:slf4j-simple",
                "com.fasterxml.jackson.core:jackson-databind",
            ]
        );
        let test_keys: Vec<String> = test.iter().map(|d| d.key()).collect();
        assert_eq!(test_keys, vec!["junit:junit"]);
    }

    #[test]
    fn maven_provided_scope_is_collected() {
        let xml = r#"
            <project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <dependencies>
                <dependency>
                  <groupId>jakarta.servlet</groupId>
                  <artifactId>jakarta.servlet-api</artifactId>
                  <version>6.1.0</version>
                  <scope>provided</scope>
                </dependency>
                <dependency>
                  <groupId>com.google.guava</groupId>
                  <artifactId>guava</artifactId>
                  <version>33.0.0-jre</version>
                </dependency>
              </dependencies>
            </project>
        "#;
        let provided = maven_provided_dependencies_from_xml(xml).unwrap();
        assert_eq!(provided.len(), 1);
        assert_eq!(provided[0].key(), "jakarta.servlet:jakarta.servlet-api");
        assert_eq!(provided[0].version.as_deref(), Some("6.1.0"));

        let runtime = maven_dependencies_from_xml(xml).unwrap();
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime[0].key(), "com.google.guava:guava");
    }

    #[test]
    fn gradle_compile_only_is_collected() {
        let script = r#"
            plugins { id 'java' }
            dependencies {
                implementation 'com.google.guava:guava:33.0.0-jre'
                compileOnly 'org.projectlombok:lombok:1.18.30'
                compileOnlyApi("jakarta.servlet:jakarta.servlet-api:6.1.0")
            }
        "#;
        let provided = gradle_provided_dependencies_from_content(script).unwrap();
        let keys: Vec<String> = provided.iter().map(|d| d.key()).collect();
        assert_eq!(
            keys,
            vec![
                "org.projectlombok:lombok",
                "jakarta.servlet:jakarta.servlet-api"
            ]
        );

        let (runtime, test) = gradle_dependencies_from_content(script).unwrap();
        let runtime_keys: Vec<String> = runtime.iter().map(|d| d.key()).collect();
        assert_eq!(runtime_keys, vec!["com.google.guava:guava"]);
        assert!(test.is_empty());
    }
}
