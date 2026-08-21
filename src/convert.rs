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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, bail};
use regex::Regex;

use crate::cache::download_repo_text;
use crate::central;
use crate::commands::build::{self, MainDecision, MainTarget};
use crate::commands::{resolve, resolve_provided, resolve_tests, write_lock};
use crate::config::{
    CONFIG_FILE, CacheSettings, MultiModuleConfig, ProjectConfig, ProjectSettings,
};
use crate::lock::LOCK_FILE;
use crate::pom::{Pom, PomDependency, is_runtime_dependency, parse_pom};
use crate::resolver::{DEFAULT_REPO_URL, interpolate};

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
///
/// Uses the repositories (custom first, Maven Central last) to resolve the
/// `<dependencyManagement>` of every imported Maven BOM and every Gradle
/// `platform(...)` declaration, so version-less dependencies get their pins.
pub fn collect_dependencies(
    client: &reqwest::blocking::Client,
    project_type: ProjectType,
) -> anyhow::Result<ConvertedDependencies> {
    let repos = repositories_from_files(project_type);
    match project_type {
        ProjectType::Maven => {
            let xml = fs::read_to_string("pom.xml").context("cannot read pom.xml")?;
            let pom = parse_pom(&xml)?;
            let (properties, managed) = maven_context(&pom);
            let managed = merge_bom_imports(client, &repos, &pom, &properties, managed)?;
            Ok(ConvertedDependencies {
                runtime: collect_scope(&pom, &properties, &managed, is_runtime_dependency),
                provided: collect_scope(&pom, &properties, &managed, |d| d.scope == "provided"),
                test: collect_scope(&pom, &properties, &managed, |d| d.scope == "test"),
                repositories: maven_repositories_from_xml(&pom),
            })
        }
        ProjectType::GradleGroovy => {
            let content = fs::read_to_string("build.gradle").context("cannot read build.gradle")?;
            let (runtime, test) = gradle_dependencies(client, &repos, &content)?;
            Ok(ConvertedDependencies {
                runtime,
                provided: gradle_provided_dependencies(&content)?,
                test,
                repositories: gradle_repositories_from_content(&content),
            })
        }
        ProjectType::GradleKotlin => {
            let content =
                fs::read_to_string("build.gradle.kts").context("cannot read build.gradle.kts")?;
            let (runtime, test) = gradle_dependencies(client, &repos, &content)?;
            Ok(ConvertedDependencies {
                runtime,
                provided: gradle_provided_dependencies(&content)?,
                test,
                repositories: gradle_repositories_from_content(&content),
            })
        }
    }
}

/// The repository list used by the conversion: the URLs from the build file,
/// with Maven Central appended last.
fn repositories_from_files(project_type: ProjectType) -> Vec<String> {
    let mut repos = match project_type {
        ProjectType::Maven => fs::read_to_string("pom.xml")
            .ok()
            .and_then(|xml| parse_pom(&xml).ok())
            .map(|pom| {
                maven_repositories_from_xml(&pom)
                    .into_values()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        ProjectType::GradleGroovy => fs::read_to_string("build.gradle")
            .ok()
            .map(|content| {
                gradle_repositories_from_content(&content)
                    .into_values()
                    .collect()
            })
            .unwrap_or_default(),
        ProjectType::GradleKotlin => fs::read_to_string("build.gradle.kts")
            .ok()
            .map(|content| {
                gradle_repositories_from_content(&content)
                    .into_values()
                    .collect()
            })
            .unwrap_or_default(),
    };
    repos.push(DEFAULT_REPO_URL.to_string());
    repos
}

/// Collect the versions a Maven project inherits from every imported BOM
/// (`<type>pom</type>` + `<scope>import</scope>`) and merge them into the
/// local `<dependencyManagement>` map.
///
/// Priority rules (Maven semantics): the POM's own `<dependencyManagement>`
/// always wins over imports, and among several imports the last one wins.
/// BOM POMs are downloaded one level deep — a BOM that imports another BOM
/// is not followed further.
fn merge_bom_imports(
    client: &reqwest::blocking::Client,
    repos: &[String],
    pom: &Pom,
    properties: &HashMap<String, String>,
    mut managed: HashMap<(String, String), String>,
) -> anyhow::Result<HashMap<(String, String), String>> {
    let mut imported: HashMap<(String, String), String> = HashMap::new();
    for dep in &pom.managed_dependencies {
        let is_import = dep.typ.as_deref() == Some("pom") && dep.scope == "import";
        if !is_import {
            continue;
        }
        let group = interpolate(&dep.group_id, properties);
        let artifact = interpolate(&dep.artifact_id, properties);
        let Some(version) = dep.version.as_deref() else {
            continue;
        };
        let version = interpolate(version, properties);
        let Some(pom) = download_bom_pom(client, repos, &group, &artifact, &version)? else {
            println!(
                "  {} imported BOM {group}:{artifact}:{version} not found — its versions are not applied",
                crate::console::yellow("warning:")
            );
            continue;
        };
        let (_, bom_managed) = maven_context(&pom);
        for (key, value) in bom_managed {
            // Later imports overwrite earlier ones.
            imported.insert(key, value);
        }
    }
    // Local <dependencyManagement> entries win over imported ones.
    for (key, value) in imported {
        managed.entry(key).or_insert(value);
    }
    Ok(managed)
}

/// Download and parse a BOM POM (`type=pom`) from the repositories.
fn download_bom_pom(
    client: &reqwest::blocking::Client,
    repos: &[String],
    group: &str,
    artifact: &str,
    version: &str,
) -> anyhow::Result<Option<Pom>> {
    let relative_path = format!(
        "{}/{}/{}/{}-{}.pom",
        group.replace('.', "/"),
        artifact,
        version,
        artifact,
        version
    );
    for repo in repos {
        match download_repo_text(client, repo, &relative_path) {
            Ok(xml) => return parse_pom(&xml).map(Some),
            Err(_) => continue,
        }
    }
    Ok(None)
}

/// Extract the runtime dependencies from a `pom.xml`.
///
/// Versions are taken from the `<dependency>` element or, when missing,
/// from `<dependencyManagement>` in the same POM.  `${...}` placeholders
/// are substituted using the POM's own properties.
#[cfg_attr(not(test), allow(dead_code))]
pub fn maven_dependencies_from_xml(xml: &str) -> anyhow::Result<Vec<ConvertedDependency>> {
    let pom = parse_pom(xml)?;
    let (properties, managed) = maven_context(&pom);
    Ok(collect_scope(&pom, &properties, &managed, |dep| {
        is_runtime_dependency(dep)
    }))
}

/// Extract the `provided`-scope dependencies, which are required to compile
/// but never land on the runtime classpath.
#[cfg_attr(not(test), allow(dead_code))]
pub fn maven_provided_dependencies_from_xml(xml: &str) -> anyhow::Result<Vec<ConvertedDependency>> {
    let pom = parse_pom(xml)?;
    let (properties, managed) = maven_context(&pom);
    Ok(collect_scope(&pom, &properties, &managed, |dep| {
        dep.scope == "provided"
    }))
}

/// Extract the test-scope dependencies from a `pom.xml` the same way as the
/// runtime ones, e.g. `junit:junit:4.13.2`.
#[cfg_attr(not(test), allow(dead_code))]
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

/// Convert a native absolute directory path into URL form: forward slashes,
/// and a leading slash for drive-letter paths (`C:/x` -> `/C:/x`) so a
/// `file://` prefix yields a valid `file:///C:/x` URL on Windows.
fn url_path(dir: &str) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
    // Keep path structure intact: `/`, `:`, `@`, `.`, `-`, `_`, `~`
    const FILE_PATH: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'/')
        .remove(b':')
        .remove(b'@')
        .remove(b'.')
        .remove(b'-')
        .remove(b'_')
        .remove(b'~');
    let mut url = dir.replace('\\', "/");
    if url.len() >= 3 && url.as_bytes()[1] == b':' {
        url.insert(0, '/');
    }
    utf8_percent_encode(&url, FILE_PATH).to_string()
}

/// Convert the custom `<repositories>` of a Maven project, resolving the
/// built-in `${project.basedir}` property (and any POM property that builds
/// on it) against the directory jip is running in.
pub fn maven_repositories_from_xml(pom: &Pom) -> BTreeMap<String, String> {
    let (mut properties, _) = maven_context(pom);
    let basedir = std::env::current_dir()
        .ok()
        .map(|dir| url_path(&dir.to_string_lossy()))
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
#[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
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

/// The result of parsing a version catalog file.
///
/// Maps every `libs.x` accessor alias to its coordinates.  The version is
/// `None` when the library entry carries no version (e.g. a BOM alias used
/// by `platform(...)`).
type VersionCatalog = HashMap<String, (String, String, Option<String>)>;

/// Read `gradle/libs.versions.toml` when present, mapping each `[libraries]`
/// alias to `(group, artifact, Optional<version>)`.  Versions are resolved
/// from the `[versions]` table through `version.ref`.
fn load_version_catalog() -> Option<VersionCatalog> {
    let raw = fs::read_to_string("gradle/libs.versions.toml").ok()?;
    parse_version_catalog(&raw)
}

/// Parse the content of `gradle/libs.versions.toml` into the alias map.
///
/// Entries without a `group`/`name` (or `artifact`) column are skipped, as
/// are any whose `version.ref` has no matching `[versions]` entry.
fn parse_version_catalog(raw: &str) -> Option<VersionCatalog> {
    let value: toml::Value = toml::from_str(raw).ok()?;
    let versions = value.get("versions").and_then(toml::Value::as_table)?;
    let libraries = value.get("libraries").and_then(toml::Value::as_table)?;
    let catalog = libraries
        .iter()
        .filter_map(|(alias, entry)| {
            let table = entry.as_table()?;
            let group = table.get("group")?.as_str()?;
            let artifact = table
                .get("name")
                .or_else(|| table.get("artifact"))?
                .as_str()?;
            let version = table
                .get("version")
                .and_then(toml::Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    // `version.ref = "junit"` (or `version = { ref = "junit" }`)
                    // is parsed as a nested `version` table by the TOML crate.
                    table
                        .get("version")
                        .and_then(toml::Value::as_table)
                        .and_then(|t| t.get("ref"))
                        .and_then(toml::Value::as_str)
                        .and_then(|name| {
                            versions
                                .get(name)
                                .and_then(toml::Value::as_str)
                                .map(str::to_string)
                        })
                });
            Some((
                alias.clone(),
                (group.to_string(), artifact.to_string(), version),
            ))
        })
        .collect();
    Some(catalog)
}

/// Extract the runtime and test dependencies from a Gradle build script,
/// resolving `platform(...)` BOMs and version-catalog accessors.
///
/// Priority for picking a version (highest first):
///   1. explicit `group:artifact:version` in the declaration
///   2. version catalog (`libs.x` accessor)
///   3. `platform(...)` / `enforcedPlatform(...)` BOM
///   4. `latest_version` fallback (in `convert_to_config`)
pub fn gradle_dependencies(
    client: &reqwest::blocking::Client,
    repos: &[String],
    content: &str,
) -> anyhow::Result<(Vec<ConvertedDependency>, Vec<ConvertedDependency>)> {
    let catalog = load_version_catalog();
    let configurations = RUNTIME_CONFIGURATIONS
        .iter()
        .chain(&TEST_CONFIGURATIONS)
        .copied()
        .collect::<Vec<_>>()
        .join("|");

    // `implementation platform("g:a:v")` / `implementation(platform(...))`,
    // Groovy and Kotlin.
    let platform = Regex::new(&format!(
        r#"^\s*({configurations})\s*(\(\s*)?(enforced)?platform\(\s*["']([^"']+)["']\s*\)\s*\)?"#
    ))?;
    // Shorthand with a version: `implementation 'g:a:v'`.
    let shorthand = Regex::new(&format!(
        r#"^\s*({configurations})\s*(\(\s*)?['"]([^:'"]+):([^:'"]+):([^'"]+)['"]"#
    ))?;
    // Shorthand without a version: `implementation 'g:a'` (needs a BOM).
    let versionless = Regex::new(&format!(
        r#"^\s*({configurations})\s*(\(\s*)?['"]([^:'"]+):([^:'"]+)['"]"#
    ))?;
    // Catalog accessor: `implementation(libs.guava)`.
    let accessor = Regex::new(&format!(
        r#"^\s*({configurations})\s*(\(\s*)?libs\.([A-Za-z0-9_.-]+)\s*\)?"#
    ))?;
    // Named argument style (with a version), e.g.
    //   implementation group: 'g', name: 'a', version: 'v'
    //   implementation(group = "g", name = "a", version = "v")
    let groovy_named = Regex::new(
        r#"^\s*(implementation|api|runtimeOnly|compile)\s*(\(\s*)?group:\s*['"]([^'"]+)['"],\s*name:\s*['"]([^'"]+)['"],\s*version:\s*['"]([^'"]+)['"]"#,
    )?;
    let kotlin_named = Regex::new(
        r#"^\s*(implementation|api|runtimeOnly|compile)\s*\(\s*group\s*=\s*"([^"]+)",\s*name\s*=\s*"([^"]+)",\s*version\s*=\s*"([^"]+)""#,
    )?;

    let mut runtime = Vec::new();
    let mut test = Vec::new();
    let mut runtime_platforms = Vec::new();
    let mut test_platforms = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") {
            continue;
        }

        if let Some(caps) = platform.captures(trimmed) {
            let is_test = TEST_CONFIGURATIONS.contains(&caps.get(1).unwrap().as_str());
            if let Some(coords) = caps.get(4) {
                let dep = parse_coordinates(coords.as_str());
                if let Some(dep) = dep {
                    if is_test {
                        test_platforms.push(dep);
                    } else {
                        runtime_platforms.push(dep);
                    }
                }
            }
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

        if let Some(caps) = versionless.captures(trimmed) {
            let is_test = TEST_CONFIGURATIONS.contains(&caps.get(1).unwrap().as_str());
            let group = caps.get(3).unwrap().as_str().to_string();
            let artifact = caps.get(4).unwrap().as_str().to_string();
            let dep = ConvertedDependency {
                group,
                artifact,
                version: None,
            };
            if is_test {
                test.push(dep);
            } else {
                runtime.push(dep);
            }
            continue;
        }

        if let Some(caps) = accessor.captures(trimmed) {
            let is_test = TEST_CONFIGURATIONS.contains(&caps.get(1).unwrap().as_str());
            let alias = caps.get(3).unwrap().as_str();
            match resolve_catalog_accessor(&catalog, alias) {
                Some(dep) if is_test => test.push(dep),
                Some(dep) => runtime.push(dep),
                None if alias.starts_with("bundles.") => println!(
                    "  {} version catalog bundle libs.{alias} not resolved — add each library from the bundle manually",
                    crate::console::yellow("warning:")
                ),
                None => println!(
                    "  {} version catalog accessor libs.{alias} not found — skipped",
                    crate::console::yellow("warning:")
                ),
            }
            continue;
        }

        // Named argument style.
        if let Some(dep) = match_named(trimmed, &groovy_named, &kotlin_named) {
            runtime.push(dep);
        }
    }

    // In Gradle, `implementation platform(...)` also applies to
    // `testImplementation` (test extends implementation).
    let runtime_versions = resolve_platforms(client, repos, &runtime_platforms)?;
    let test_versions = resolve_platforms(client, repos, &test_platforms)?;
    apply_platform_versions(&mut runtime, &runtime_versions);
    apply_platform_versions(&mut test, &runtime_versions);
    apply_platform_versions(&mut test, &test_versions);

    Ok((runtime, test))
}

/// Parse `group:artifact:version` into a dependency, or `None` when the
/// string is not a full `g:a:v` triple.
fn parse_coordinates(coords: &str) -> Option<ConvertedDependency> {
    let mut parts = coords.splitn(3, ':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let version = parts.next()?;
    if group.is_empty() || artifact.is_empty() || version.is_empty() {
        return None;
    }
    Some(ConvertedDependency {
        group: group.to_string(),
        artifact: artifact.to_string(),
        version: Some(version.to_string()),
    })
}

/// Resolve a `libs.alias` accessor against the version catalog.  The alias
/// may be nested (`libs.spring.boot`) or kebab-cased (`libs.jakarta-servlet`).
fn resolve_catalog_accessor(
    catalog: &Option<VersionCatalog>,
    alias: &str,
) -> Option<ConvertedDependency> {
    let alias = alias.strip_prefix("libs.").unwrap_or(alias);
    let catalog = catalog.as_ref()?;
    let key = catalog.get(alias).or_else(|| {
        // `libs.jakartaServletApi` -> catalog key `jakarta-servlet-api`.
        let kebab = camel_to_kebab(alias);
        catalog.get(&kebab)
    })?;
    Some(ConvertedDependency {
        group: key.0.clone(),
        artifact: key.1.clone(),
        version: key.2.clone(),
    })
}

/// Convert a camelCase accessor segment to the catalog's kebab-case key form.
fn camel_to_kebab(input: &str) -> String {
    let mut out = String::new();
    for c in input.chars() {
        if c.is_ascii_uppercase() && !out.is_empty() {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// Download every `platform(...)` POM and collect its `<dependencyManagement>`
/// versions, keyed by `(group, artifact)`.
fn resolve_platforms(
    client: &reqwest::blocking::Client,
    repos: &[String],
    platforms: &[ConvertedDependency],
) -> anyhow::Result<HashMap<(String, String), String>> {
    let mut versions = HashMap::new();
    for platform in platforms {
        let Some(version) = platform.version.as_deref() else {
            continue;
        };
        if let Some(pom) =
            download_bom_pom(client, repos, &platform.group, &platform.artifact, version)?
        {
            let (_, managed) = maven_context(&pom);
            versions.extend(managed);
        } else {
            println!(
                "  {} platform {}:{version} not found — its versions are not applied",
                crate::console::yellow("warning:"),
                platform.key()
            );
        }
    }
    Ok(versions)
}

/// Fill version-less dependencies from the platform-provided `<dependencyManagement>`.
fn apply_platform_versions(
    deps: &mut [ConvertedDependency],
    platform_versions: &HashMap<(String, String), String>,
) {
    for dep in deps.iter_mut() {
        if dep.version.is_some() {
            continue;
        }
        if let Some(version) = platform_versions.get(&(dep.group.clone(), dep.artifact.clone())) {
            dep.version = Some(version.clone());
        }
    }
}

/// Extract `compileOnly` dependencies from a Gradle build script, with
/// version-catalog accessors.  Platforms are not tracked for `compileOnly`
/// — the declared version (or catalog version) is used directly.
pub fn gradle_provided_dependencies(content: &str) -> anyhow::Result<Vec<ConvertedDependency>> {
    let catalog = load_version_catalog();
    let configurations = COMPILE_ONLY_CONFIGURATIONS.join("|");
    let shorthand = Regex::new(&format!(
        r#"^\s*({configurations})\s*(\(\s*)?['"]([^:'"]+):([^:'"]+):([^'"]+)['"]"#
    ))?;
    let accessor = Regex::new(&format!(
        r#"^\s*({configurations})\s*(\(\s*)?libs\.([A-Za-z0-9_.-]+)\s*\)?"#
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
            continue;
        }
        if let Some(caps) = accessor.captures(trimmed)
            && let Some(dep) = resolve_catalog_accessor(&catalog, caps.get(3).unwrap().as_str())
        {
            provided.push(dep);
        }
    }
    Ok(provided)
}

/// Extract custom Maven repository URLs from a Gradle build script.
///
/// Recognises `maven { url = uri("...") }` blocks and Kotlin inline
/// `maven("...")` declarations.  `$projectDir` is resolved against the
/// current directory.  Well-known repositories (`mavenCentral()`,
/// `mavenLocal()`, `gradlePluginPortal()`) are ignored — jip adds
/// Maven Central automatically.
pub fn gradle_repositories_from_content(content: &str) -> BTreeMap<String, String> {
    let re_url_uri =
        Regex::new(r#"url\s*=\s*uri\(["']([^"']+)["']\)"#).expect("valid url=uri regex");
    let re_url_eq = Regex::new(r#"url\s*=\s*["']([^"']+)["']"#).expect("valid url= regex");
    let re_url_space = Regex::new(r#"url\s+["']([^"']+)["']"#).expect("valid url regex");
    let re_maven_inline =
        Regex::new(r#"maven\(["']([^"']+)["']\)"#).expect("valid maven inline regex");

    let basedir = std::env::current_dir()
        .ok()
        .map(|dir| url_path(&dir.to_string_lossy()))
        .unwrap_or_default();

    let resolve = |url: &str| -> String {
        let url = url.replace("$projectDir", &basedir);
        url.replace("${projectDir}", &basedir)
    };

    let mut repos = BTreeMap::new();
    let mut depth = 0i32;
    let mut in_repositories = 0i32;
    let mut in_maven = 0i32;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") {
            continue;
        }

        let open = trimmed.chars().filter(|&c| c == '{').count();
        let close = trimmed.chars().filter(|&c| c == '}').count();

        // Enter repositories block
        if in_repositories == 0 && open > 0 && trimmed.contains("repositories") {
            in_repositories = depth + 1;
        }
        // Enter maven block inside repositories
        if in_repositories > 0 && in_maven == 0 && open > 0 && trimmed.contains("maven") {
            in_maven = depth + 1;
        }

        depth += open as i32;

        // Extract URLs inside maven { } blocks
        if in_maven > 0 {
            if let Some(caps) = re_url_uri.captures(trimmed) {
                let url = resolve(caps.get(1).unwrap().as_str());
                repos.insert(url.clone(), url);
            } else if let Some(caps) = re_url_eq.captures(trimmed) {
                let url = resolve(caps.get(1).unwrap().as_str());
                repos.insert(url.clone(), url);
            } else if let Some(caps) = re_url_space.captures(trimmed) {
                let url = resolve(caps.get(1).unwrap().as_str());
                repos.insert(url.clone(), url);
            }
        }

        // Kotlin inline maven("url") inside repositories { }
        if in_repositories > 0
            && in_maven == 0
            && let Some(caps) = re_maven_inline.captures(trimmed)
        {
            let url = resolve(caps.get(1).unwrap().as_str());
            repos.insert(url.clone(), url);
        }

        depth -= close as i32;

        // Exit blocks
        if in_maven > 0 && depth < in_maven {
            in_maven = 0;
        }
        if in_repositories > 0 && depth < in_repositories {
            in_repositories = 0;
        }
    }

    repos
}

/// Extract dependencies declared in `subprojects { dependencies { ... } }` and
/// `allprojects { dependencies { ... } }` blocks from a root `build.gradle`.
///
/// Returns `(runtime, test, provided, repositories)` — the inherited deps and
/// repos that should be merged into every child module.
#[allow(clippy::type_complexity)]
pub fn gradle_subprojects_deps(
    content: &str,
) -> (
    Vec<ConvertedDependency>,
    Vec<ConvertedDependency>,
    Vec<ConvertedDependency>,
    BTreeMap<String, String>,
) {
    // Regexes for dependency lines (same patterns as gradle_dependencies_from_content).
    let configurations = RUNTIME_CONFIGURATIONS
        .iter()
        .chain(&TEST_CONFIGURATIONS)
        .copied()
        .collect::<Vec<_>>()
        .join("|");
    let shorthand = Regex::new(&format!(
        r#"^\s*({configurations})\s*(\(\s*)?['"]([^:'"]+):([^:'"]+):([^'"]+)['"]"#
    ))
    .unwrap();
    let groovy_named = Regex::new(
        r#"^\s*(implementation|api|runtimeOnly|compile)\s*(\(\s*)?group:\s*['"]([^'"]+)['"],\s*name:\s*['"]([^'"]+)['"],\s*version:\s*['"]([^'"]+)['"]"#,
    ).unwrap();
    let kotlin_named = Regex::new(
        r#"^\s*(implementation|api|runtimeOnly|compile)\s*\(\s*group\s*=\s*"([^"]+)",\s*name\s*=\s*"([^"]+)",\s*version\s*=\s*"([^"]+)""#,
    ).unwrap();
    let compile_only_configs = COMPILE_ONLY_CONFIGURATIONS.join("|");
    let compile_shorthand = Regex::new(&format!(
        r#"^\s*({compile_only_configs})\s*(\(\s*)?['"]([^:'"]+):([^:'"]+):([^'"]+)['"]"#
    ))
    .unwrap();
    let re_url_uri = Regex::new(r#"url\s*=\s*uri\(["']([^"']+)["']\)"#).unwrap();
    let re_url_eq = Regex::new(r#"url\s*=\s*["']([^"']+)["']"#).unwrap();
    let re_url_space = Regex::new(r#"url\s+["']([^"']+)["']"#).unwrap();
    let re_maven_inline = Regex::new(r#"maven\(["']([^"']+)["']\)"#).unwrap();

    let basedir = std::env::current_dir()
        .ok()
        .map(|dir| url_path(&dir.to_string_lossy()))
        .unwrap_or_default();
    let resolve_url = |url: &str| -> String {
        let url = url.replace("$projectDir", &basedir);
        url.replace("${projectDir}", &basedir)
    };

    let mut runtime = Vec::new();
    let mut test = Vec::new();
    let mut provided = Vec::new();
    let mut repos = BTreeMap::new();

    let mut depth = 0i32;
    let mut in_block = 0i32; // inside subprojects/allprojects
    let mut in_deps = 0i32; // inside dependencies { } inside that block
    let mut in_repos = 0i32; // inside repositories { } inside that block
    let mut in_maven = 0i32; // inside maven { } inside repositories

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") {
            continue;
        }

        let open = trimmed.chars().filter(|&c| c == '{').count();
        let close = trimmed.chars().filter(|&c| c == '}').count();

        // Enter subprojects/allprojects block
        if in_block == 0 && open > 0 {
            let dominated = trimmed.contains("subprojects") || trimmed.contains("allprojects");
            if dominated {
                in_block = depth + 1;
            }
        }
        // Enter dependencies block inside subprojects/allprojects
        if in_block > 0 && in_deps == 0 && open > 0 && trimmed.contains("dependencies") {
            in_deps = depth + 1;
        }
        // Enter repositories block inside subprojects/allprojects
        if in_block > 0 && in_repos == 0 && open > 0 && trimmed.contains("repositories") {
            in_repos = depth + 1;
        }
        // Enter maven block inside repositories
        if in_repos > 0 && in_maven == 0 && open > 0 && trimmed.contains("maven") {
            in_maven = depth + 1;
        }

        depth += open as i32;

        // Parse dependency lines when inside dependencies block
        if in_deps > 0 {
            if let Some((dep, is_test)) = match_shorthand(trimmed, &shorthand) {
                if is_test {
                    test.push(dep);
                } else {
                    runtime.push(dep);
                }
            } else if let Some(dep) = match_named(trimmed, &groovy_named, &kotlin_named) {
                runtime.push(dep);
            } else if let Some(caps) = compile_shorthand.captures(trimmed)
                && let (Some(group), Some(artifact), Some(version)) =
                    (caps.get(3), caps.get(4), caps.get(5))
            {
                provided.push(convert(group.as_str(), artifact.as_str(), version.as_str()));
            }
        }

        // Parse repository URLs inside repositories block
        if in_maven > 0 {
            if let Some(caps) = re_url_uri.captures(trimmed) {
                let url = resolve_url(caps.get(1).unwrap().as_str());
                repos.insert(url.clone(), url);
            } else if let Some(caps) = re_url_eq.captures(trimmed) {
                let url = resolve_url(caps.get(1).unwrap().as_str());
                repos.insert(url.clone(), url);
            } else if let Some(caps) = re_url_space.captures(trimmed) {
                let url = resolve_url(caps.get(1).unwrap().as_str());
                repos.insert(url.clone(), url);
            }
        }
        if in_repos > 0
            && in_maven == 0
            && let Some(caps) = re_maven_inline.captures(trimmed)
        {
            let url = resolve_url(caps.get(1).unwrap().as_str());
            repos.insert(url.clone(), url);
        }

        depth -= close as i32;

        // Exit blocks
        if in_maven > 0 && depth < in_maven {
            in_maven = 0;
        }
        if in_repos > 0 && depth < in_repos {
            in_repos = 0;
        }
        if in_deps > 0 && depth < in_deps {
            in_deps = 0;
        }
        if in_block > 0 && depth < in_block {
            in_block = 0;
        }
    }

    (runtime, test, provided, repos)
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
    // Check for multi-module project first.
    if let Some(layout) = crate::multi::detect_multi_module() {
        return convert_multi_module(client, &layout);
    }

    // A parent POM / dynamic-include Gradle layout that jip could not detect
    // as multi-module is not supported — tell the user why building the
    // converted single module will find no sources.
    if let Some(hint) = crate::multi::undetected_multi_module_hint()
        .or_else(crate::multi::undetected_gradle_multi_module_hint)
    {
        println!("  {} {hint}", crate::console::yellow("warning:"));
    }

    convert_single_module(client)
}

/// Convert a single-module project (the original path).
fn convert_single_module(client: &reqwest::blocking::Client) -> anyhow::Result<ProjectConfig> {
    let project_type = detect();
    let mut config = ProjectConfig {
        project: ProjectSettings {
            name: Some(current_directory_name()),
            java: Some(java_default_for(project_type)),
            main: None,
            source: None,
        },
        cache: CacheSettings::default(),
        proxy: crate::config::ProxySettings::default(),
        classpath: crate::config::ClasspathSettings::default(),
        repositories: BTreeMap::new(),
        dependencies: BTreeMap::new(),
        provided_dependencies: BTreeMap::new(),
        test_dependencies: BTreeMap::new(),
        modules: None,
    };

    if let Some(project_type) = project_type {
        let converted = collect_dependencies(client, project_type)?;
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

    let resolution = resolve(client, &config, false)?;
    let provided = resolve_provided(client, &config, false)?;
    let tests = resolve_tests(client, &config, false)?;
    write_lock(&resolution.flat, &provided.flat, &tests.flat)?;
    Ok(config)
}

/// Convert a multi-module project: create a root `jip.toml` and per-module
/// configs, skipping inter-module dependencies from external resolution.
fn convert_multi_module(
    client: &reqwest::blocking::Client,
    layout: &crate::multi::MultiModuleLayout,
) -> anyhow::Result<ProjectConfig> {
    let project_type = detect().unwrap_or(match layout.build_system {
        crate::multi::BuildSystem::Maven => ProjectType::Maven,
        crate::multi::BuildSystem::GradleGroovy => ProjectType::GradleGroovy,
        crate::multi::BuildSystem::GradleKotlin => ProjectType::GradleKotlin,
    });
    let root_dir = std::env::current_dir().context("cannot determine working directory")?;

    // Build the module map for the root config.
    let module_map: BTreeMap<String, String> = layout
        .modules
        .iter()
        .map(|m| (m.name.clone(), m.path.clone()))
        .collect();

    // Build a set of all sibling artifact IDs for filtering.
    // Any dependency whose artifact ID matches a sibling module's artifact ID
    // should be resolved at build time from compiled classes, not externally.
    let inter_module_aids: HashSet<String> = layout
        .modules
        .iter()
        .filter_map(|m| m.artifact_id.clone())
        .collect();

    // Collect shared repositories from the parent POM or root build file.
    let mut shared_repositories = BTreeMap::new();

    // For Gradle multi-module projects, parse subprojects/allprojects deps
    // from the root build.gradle so child modules inherit them.
    let mut inherited_runtime = Vec::new();
    let mut inherited_test = Vec::new();
    let mut inherited_provided = Vec::new();

    if let Some(project_type_enum) = match layout.build_system {
        crate::multi::BuildSystem::Maven => Some(ProjectType::Maven),
        crate::multi::BuildSystem::GradleGroovy => Some(ProjectType::GradleGroovy),
        crate::multi::BuildSystem::GradleKotlin => Some(ProjectType::GradleKotlin),
    } {
        if matches!(
            layout.build_system,
            crate::multi::BuildSystem::GradleGroovy | crate::multi::BuildSystem::GradleKotlin
        ) {
            let root_build = if std::path::Path::new("build.gradle.kts").exists() {
                fs::read_to_string("build.gradle.kts").unwrap_or_default()
            } else {
                fs::read_to_string("build.gradle").unwrap_or_default()
            };
            let (rt, tst, prv, sub_repos) = gradle_subprojects_deps(&root_build);
            inherited_runtime = rt;
            inherited_test = tst;
            inherited_provided = prv;
            shared_repositories = sub_repos;
        } else {
            let converted = collect_dependencies(client, project_type_enum)?;
            shared_repositories = converted.repositories;
        }
    }

    let java_version = java_default_for(Some(project_type));

    // Convert each module independently.
    let mut all_module_configs = Vec::new();
    for module in &layout.modules {
        let module_dir = root_dir.join(&module.path);
        let module_type = if module_dir.join("pom.xml").exists() {
            Some(ProjectType::Maven)
        } else if module_dir.join("build.gradle.kts").exists() {
            Some(ProjectType::GradleKotlin)
        } else if module_dir.join("build.gradle").exists() {
            Some(ProjectType::GradleGroovy)
        } else {
            None
        };

        let mut module_config = ProjectConfig {
            project: ProjectSettings {
                name: Some(module.name.clone()),
                java: Some(java_version.clone()),
                main: None,
                source: None,
            },
            cache: CacheSettings::default(),
            proxy: crate::config::ProxySettings::default(),
            classpath: crate::config::ClasspathSettings::default(),
            repositories: shared_repositories.clone(),
            dependencies: BTreeMap::new(),
            provided_dependencies: BTreeMap::new(),
            test_dependencies: BTreeMap::new(),
            modules: None,
        };

        if let Some(mt) = module_type {
            // Temporarily change directory to the module to collect dependencies.
            let original_dir = std::env::current_dir()?;
            std::env::set_current_dir(&module_dir)?;

            let converted = collect_dependencies(client, mt)?;
            let repos = crate::commands::repositories_for(&module_config);

            // Merge inherited deps: module-local wins over inherited for
            // the same group:artifact.  Start with inherited, then override.
            let mut merged_runtime = inherited_runtime.clone();
            for dep in &converted.runtime {
                merged_runtime.retain(|d| d.key() != dep.key());
                merged_runtime.push(dep.clone());
            }
            let mut merged_provided = inherited_provided.clone();
            for dep in &converted.provided {
                merged_provided.retain(|d| d.key() != dep.key());
                merged_provided.push(dep.clone());
            }
            let mut merged_test = inherited_test.clone();
            for dep in &converted.test {
                merged_test.retain(|d| d.key() != dep.key());
                merged_test.push(dep.clone());
            }

            // Filter out inter-module dependencies.
            let runtime = merged_runtime
                .into_iter()
                .filter(|d| !inter_module_aids.contains(&d.artifact))
                .collect();
            let provided = merged_provided
                .into_iter()
                .filter(|d| !inter_module_aids.contains(&d.artifact))
                .collect();
            let test = merged_test
                .into_iter()
                .filter(|d| !inter_module_aids.contains(&d.artifact))
                .collect();

            module_config.dependencies = convert_to_config(client, &repos, runtime)?;
            module_config.provided_dependencies = convert_to_config(client, &repos, provided)?;
            module_config.test_dependencies = convert_to_config(client, &repos, test)?;

            // Detect main class for this module.
            match build::resolve_main(&module_config, None) {
                Ok(MainDecision::Run(target)) => {
                    module_config.project.main = Some(main_value(target));
                }
                Ok(MainDecision::Multiple(candidates)) => {
                    if std::io::stdin().is_terminal() {
                        if let Ok(target) = build::choose_main(&candidates) {
                            module_config.project.main = Some(main_value(target));
                        }
                    } else {
                        println!(
                            "  {} {}",
                            crate::console::yellow("warning:"),
                            build::multiple_main_error(&candidates).replace('\n', "\n  ")
                        );
                    }
                }
                _ => {}
            }

            std::env::set_current_dir(&original_dir)?;
        }

        // Write the module's jip.toml.
        let module_toml_path = module_dir.join(CONFIG_FILE);
        module_config.save(&module_toml_path)?;

        // Resolve and write lock for the module.
        let original_dir = std::env::current_dir()?;
        std::env::set_current_dir(&module_dir)?;
        let resolution = resolve(client, &module_config, false)?;
        let provided = resolve_provided(client, &module_config, false)?;
        let tests = resolve_tests(client, &module_config, false)?;
        write_lock(&resolution.flat, &provided.flat, &tests.flat)?;
        std::env::set_current_dir(&original_dir)?;

        let dep_count = module_config.dependencies.len();
        println!(
            "{}",
            crate::console::green(&format!(
                "converted module '{}' — {} dependencies",
                module.name, dep_count
            ))
        );

        all_module_configs.push(module_config);
    }

    // Build the root config.
    let mut root_config = ProjectConfig {
        project: ProjectSettings {
            name: Some(current_directory_name()),
            java: Some(java_version),
            main: all_module_configs
                .iter()
                .find_map(|c| c.project.main.clone()),
            source: None,
        },
        cache: CacheSettings::default(),
        proxy: crate::config::ProxySettings::default(),
        classpath: crate::config::ClasspathSettings::default(),
        repositories: shared_repositories,
        dependencies: BTreeMap::new(),
        provided_dependencies: BTreeMap::new(),
        test_dependencies: BTreeMap::new(),
        modules: Some(MultiModuleConfig {
            modules: module_map,
        }),
    };

    // Detect main class from the root (for multi-module, this is usually
    // the module that has the main class).
    match build::resolve_main(&root_config, None) {
        Ok(MainDecision::Run(target)) => {
            root_config.project.main = Some(main_value(target));
        }
        Ok(MainDecision::Multiple(candidates)) if std::io::stdin().is_terminal() => {
            if let Ok(target) = build::choose_main(&candidates) {
                root_config.project.main = Some(main_value(target));
            }
        }
        _ => {}
    }

    root_config.save(Path::new(CONFIG_FILE))?;

    println!(
        "{}",
        crate::console::green(&format!(
            "converted multi-module {} project — {} modules",
            match layout.build_system {
                crate::multi::BuildSystem::Maven => "Maven",
                crate::multi::BuildSystem::GradleGroovy => "Gradle (Groovy)",
                crate::multi::BuildSystem::GradleKotlin => "Gradle (Kotlin)",
            },
            layout.modules.len()
        ))
    );

    Ok(root_config)
}

/// The Java version jip writes into a fresh `jip.toml`: the project's own
/// value when declared (Maven/Gradle), otherwise the installed JDK on `PATH`.
fn java_default_for(project_type: Option<ProjectType>) -> String {
    if matches!(project_type, Some(ProjectType::Maven))
        && let Ok(xml) = fs::read_to_string("pom.xml")
        && let Ok(Some(version)) = maven_java_version_from_xml(&xml)
    {
        return version;
    }
    if matches!(
        project_type,
        Some(ProjectType::GradleGroovy | ProjectType::GradleKotlin)
    ) {
        let filename = match project_type.unwrap() {
            ProjectType::GradleGroovy => "build.gradle",
            ProjectType::GradleKotlin => "build.gradle.kts",
            _ => unreachable!(),
        };
        if let Ok(content) = fs::read_to_string(filename)
            && let Some(version) = gradle_java_version_from_content(&content)
        {
            return version;
        }
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

/// The Java version a Gradle project compiles for, extracted from the build
/// script's `toolchain`, `sourceCompatibility`, or `targetCompatibility`.
/// Returns `None` when no version is declared.
pub fn gradle_java_version_from_content(content: &str) -> Option<String> {
    static RE_TOOLCHAIN: OnceLock<Regex> = OnceLock::new();
    let re_toolchain = RE_TOOLCHAIN
        .get_or_init(|| Regex::new(r"languageVersion[^0-9]*(\d+)").expect("valid toolchain regex"));
    static RE_SOURCE: OnceLock<Regex> = OnceLock::new();
    let re_source = RE_SOURCE.get_or_init(|| {
        Regex::new(r"sourceCompatibility[^0-9]*(\d+)").expect("valid source compat regex")
    });
    static RE_TARGET: OnceLock<Regex> = OnceLock::new();
    let re_target = RE_TARGET.get_or_init(|| {
        Regex::new(r"targetCompatibility[^0-9]*(\d+)").expect("valid target compat regex")
    });

    re_toolchain
        .captures(content)
        .or_else(|| re_source.captures(content))
        .or_else(|| re_target.captures(content))
        .and_then(|c| c.get(1))
        .map(|m| normalize_java_major(m.as_str()))
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

    #[test]
    fn gradle_java_version_from_toolchain() {
        let script = r#"
            java {
                toolchain {
                    languageVersion = JavaLanguageVersion.of(17)
                }
            }
        "#;
        assert_eq!(
            gradle_java_version_from_content(script).as_deref(),
            Some("17")
        );
    }

    #[test]
    fn gradle_java_version_from_kotlin_toolchain() {
        let script = r#"
            java {
                toolchain {
                    languageVersion.set(JavaLanguageVersion.of(21))
                }
            }
        "#;
        assert_eq!(
            gradle_java_version_from_content(script).as_deref(),
            Some("21")
        );
    }

    #[test]
    fn gradle_java_version_from_source_compatibility() {
        let script = r#"
            sourceCompatibility = JavaVersion.VERSION_11
            targetCompatibility = JavaVersion.VERSION_11
        "#;
        assert_eq!(
            gradle_java_version_from_content(script).as_deref(),
            Some("11")
        );
    }

    #[test]
    fn gradle_java_version_from_string_source() {
        let script = r#"
            sourceCompatibility = '11'
        "#;
        assert_eq!(
            gradle_java_version_from_content(script).as_deref(),
            Some("11")
        );
    }

    #[test]
    fn gradle_java_version_without_configuration_is_none() {
        let script = r#"
            dependencies {
                implementation 'org.apache.commons:commons-text:1.14.0'
            }
        "#;
        assert_eq!(gradle_java_version_from_content(script), None);
    }

    #[test]
    fn gradle_repositories_groovy_url_uri() {
        let script = r#"
            repositories {
                maven {
                    url = uri("$projectDir/lib/repo")
                }
                mavenCentral()
            }
        "#;
        let repos = gradle_repositories_from_content(script);
        assert_eq!(repos.len(), 1);
        let expected = format!(
            "{}/lib/repo",
            url_path(&std::env::current_dir().unwrap().to_string_lossy())
        );
        let url = &repos[&expected];
        assert!(url.starts_with("/"));
        assert!(url.ends_with("/lib/repo"));
    }

    #[test]
    fn gradle_repositories_kotlin_inline() {
        let script = r#"
            repositories {
                maven("file:///custom/repo")
                mavenCentral()
            }
        "#;
        let repos = gradle_repositories_from_content(script);
        assert_eq!(repos.len(), 1);
        assert_eq!(repos["file:///custom/repo"], "file:///custom/repo");
    }

    #[test]
    fn gradle_repositories_groovy_url_shorthand() {
        let script = r#"
            repositories {
                maven {
                    url 'file:///local/repo'
                }
            }
        "#;
        let repos = gradle_repositories_from_content(script);
        assert_eq!(repos.len(), 1);
        assert_eq!(repos["file:///local/repo"], "file:///local/repo");
    }

    #[test]
    fn gradle_repositories_skips_maven_central() {
        let script = r#"
            repositories {
                mavenCentral()
                mavenLocal()
                gradlePluginPortal()
            }
        "#;
        let repos = gradle_repositories_from_content(script);
        assert!(repos.is_empty());
    }

    #[test]
    fn maven_bom_import_is_detected() {
        let xml = r#"
            <project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <dependencyManagement>
                <dependencies>
                  <dependency>
                    <groupId>org.springframework.boot</groupId>
                    <artifactId>spring-boot-dependencies</artifactId>
                    <version>3.2.0</version>
                    <type>pom</type>
                    <scope>import</scope>
                  </dependency>
                </dependencies>
              </dependencyManagement>
            </project>
        "#;
        let pom = parse_pom(xml).unwrap();
        let import = &pom.managed_dependencies[0];
        assert_eq!(import.typ.as_deref(), Some("pom"));
        assert_eq!(import.scope, "import");
    }

    #[test]
    fn parse_version_catalog_resolves_version_ref() {
        let raw = r#"
            [versions]
            junit = "5.10.0"
            slf4j = "2.0.13"

            [libraries]
            junit = { group = "org.junit.jupiter", name = "junit-jupiter", version.ref = "junit" }
            slf4j = { group = "org.slf4j", name = "slf4j-api", version = "2.0.13" }
            bom = { group = "org.springframework.boot", name = "spring-boot-bom", version.ref = "slf4j" }
        "#;
        let catalog = parse_version_catalog(raw).expect("eligible catalog");
        assert_eq!(
            catalog.get("junit"),
            Some(&(
                "org.junit.jupiter".to_string(),
                "junit-jupiter".to_string(),
                Some("5.10.0".to_string())
            ))
        );
        assert_eq!(catalog.get("slf4j").unwrap().2.as_deref(), Some("2.0.13"));
        assert_eq!(catalog.get("bom").unwrap().2.as_deref(), Some("2.0.13"));
    }

    #[test]
    fn catalog_accessor_maps_camel_case_to_kebab() {
        let alias = "springBootStarterWeb";
        assert_eq!(camel_to_kebab(alias), "spring-boot-starter-web");
        assert_eq!(camel_to_kebab("guava"), "guava");
    }

    #[test]
    fn unknown_catalog_accessor_is_none() {
        let catalog = parse_version_catalog(
            "[versions]\nj = \"1.0\"\n[libraries]\nguava = { group = \"g\", name = \"a\", version = \"1.0\" }",
        );
        let dep = resolve_catalog_accessor(&catalog, "libs.missing");
        assert!(dep.is_none());
    }

    #[test]
    fn gradle_platform_resolves_versions_from_local_repo() {
        // A local Maven-style repo with a platform POM.  `platform(...)` is
        // resolved and version-less dependencies pick up the pin.
        let dir = std::env::temp_dir().join(format!("jip-platform-{}", std::process::id()));
        let platform_dir = dir.join("repo/com/example/platform/1.0.0");
        std::fs::create_dir_all(&platform_dir).unwrap();
        std::fs::write(
            platform_dir.join("platform-1.0.0.pom"),
            r#"
                <project>
                  <groupId>com.example</groupId>
                  <artifactId>platform</artifactId>
                  <version>1.0.0</version>
                  <dependencyManagement>
                    <dependencies>
                      <dependency>
                        <groupId>com.example</groupId>
                        <artifactId>widget</artifactId>
                        <version>3.3.3</version>
                      </dependency>
                    </dependencies>
                  </dependencyManagement>
                </project>
            "#,
        )
        .unwrap();

        let repo_url = format!("file://{}", dir.join("repo").display());
        let client = reqwest::blocking::Client::new();
        let script = r#"
            dependencies {
                implementation platform("com.example:platform:1.0.0")
                implementation 'com.example:widget'
                testImplementation platform("com.example:test-platform:1.0.0")
                testImplementation 'com.example:missing-anything'
            }
        "#;
        let (runtime, test) = gradle_dependencies(&client, &[repo_url], script).unwrap();
        // Test platform POM is absent -> test side stays version-less.
        assert_eq!(test.len(), 1);
        assert_eq!(test[0].version, None);
        // Runtime platform POM was found -> widget gets 3.3.3.
        assert_eq!(test[0].key(), "com.example:missing-anything");
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime[0].key(), "com.example:widget");
        assert_eq!(runtime[0].version.as_deref(), Some("3.3.3"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gradle_explicit_version_beats_platform() {
        // Explicit version is priority 1: it must survive the BOM fill.
        let dir = std::env::temp_dir().join(format!("jip-platform-prio-{}", std::process::id()));
        let platform_dir = dir.join("repo/com/example/platform/1.0.0");
        std::fs::create_dir_all(&platform_dir).unwrap();
        std::fs::write(
            platform_dir.join("platform-1.0.0.pom"),
            r#"
                <project>
                  <groupId>com.example</groupId>
                  <artifactId>platform</artifactId>
                  <version>1.0.0</version>
                  <dependencyManagement>
                    <dependencies>
                      <dependency>
                        <groupId>com.example</groupId>
                        <artifactId>widget</artifactId>
                        <version>3.3.3</version>
                      </dependency>
                    </dependencies>
                  </dependencyManagement>
                </project>
            "#,
        )
        .unwrap();

        let repo_url = format!("file://{}", dir.join("repo").display());
        let client = reqwest::blocking::Client::new();
        let script = r#"
            dependencies {
                implementation platform("com.example:platform:1.0.0")
                implementation 'com.example:widget:9.9.9'
            }
        "#;
        let (runtime, _) = gradle_dependencies(&client, &[repo_url], script).unwrap();
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime[0].version.as_deref(), Some("9.9.9"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gradle_accessor_resolves_from_catalog() {
        // Pure path: accessor regex + catalog lookup + ConvertedDependency.
        let catalog = parse_version_catalog(
            r#"
                [versions]
                junit = "5.10.0"
                [libraries]
                junit = { group = "org.junit.jupiter", name = "junit-jupiter", version.ref = "junit" }
            "#,
        );
        let alias = Regex::new(r#"^\s*testImplementation\s*(\(\s*)?libs\.([A-Za-z0-9_.-]+)\s*\)?"#)
            .unwrap();
        let line = "testImplementation(libs.junit)";
        let caps = alias.captures(line.trim()).unwrap();
        let dep = resolve_catalog_accessor(&catalog, caps.get(2).unwrap().as_str()).unwrap();
        assert_eq!(dep.key(), "org.junit.jupiter:junit-jupiter");
        assert_eq!(dep.version.as_deref(), Some("5.10.0"));
    }

    #[test]
    fn versionless_shorthand_is_kept_for_bom_resolution() {
        let script = r#"
            dependencies {
                implementation 'com.example:widget'
                implementation 'com.other:thing:1.2.3'
            }
        "#;
        // No platforms declared -> empty local repo, version-less dep survives
        // for the later BOM/`latest_version` step.
        let (runtime, _) =
            gradle_dependencies(&reqwest::blocking::Client::new(), &[], script).unwrap();
        assert_eq!(runtime.len(), 2);
        assert_eq!(runtime[0].version, None); // resolved later via BOM
        assert_eq!(runtime[1].version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn bom_merge_imports_last_one_wins() {
        // Build a tiny local Maven-style repo with two BOM POMs.
        let dir = std::env::temp_dir().join(format!("jip-bom-{}", std::process::id()));
        let repo_a = dir.join("repo/com/example/boma/1.0.0");
        let repo_b = dir.join("repo/com/example/bomb/1.0.0");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();
        std::fs::write(
            repo_a.join("boma-1.0.0.pom"),
            r#"
                <project>
                  <groupId>com.example</groupId>
                  <artifactId>boma</artifactId>
                  <version>1.0.0</version>
                  <dependencyManagement>
                    <dependencies>
                      <dependency>
                        <groupId>com.example</groupId>
                        <artifactId>widget</artifactId>
                        <version>1.0.0</version>
                      </dependency>
                    </dependencies>
                  </dependencyManagement>
                </project>
            "#,
        )
        .unwrap();
        std::fs::write(
            repo_b.join("bomb-1.0.0.pom"),
            r#"
                <project>
                  <groupId>com.example</groupId>
                  <artifactId>bomb</artifactId>
                  <version>1.0.0</version>
                  <dependencyManagement>
                    <dependencies>
                      <dependency>
                        <groupId>com.example</groupId>
                        <artifactId>widget</artifactId>
                        <version>2.0.0</version>
                      </dependency>
                    </dependencies>
                  </dependencyManagement>
                </project>
            "#,
        )
        .unwrap();

        let repo_url = format!("file://{}", dir.join("repo").display());
        let client = reqwest::blocking::Client::new();
        let xml = r#"
            <project>
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <dependencyManagement>
                <dependencies>
                  <dependency>
                    <groupId>com.example</groupId>
                    <artifactId>boma</artifactId>
                    <version>1.0.0</version>
                    <type>pom</type>
                    <scope>import</scope>
                  </dependency>
                  <dependency>
                    <groupId>com.example</groupId>
                    <artifactId>bomb</artifactId>
                    <version>1.0.0</version>
                    <type>pom</type>
                    <scope>import</scope>
                  </dependency>
                </dependencies>
              </dependencyManagement>
              <dependencies>
                <dependency>
                  <groupId>com.example</groupId>
                  <artifactId>widget</artifactId>
                </dependency>
              </dependencies>
            </project>
        "#;
        let pom = parse_pom(xml).unwrap();
        let (properties, managed) = maven_context(&pom);
        let managed = merge_bom_imports(&client, &[repo_url], &pom, &properties, managed).unwrap();
        assert_eq!(
            managed
                .get(&("com.example".into(), "widget".into()))
                .map(String::as_str),
            Some("2.0.0")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gradle_subprojects_deps_parses_runtime_test_provided() {
        let root = r#"
            plugins {
                id 'java'
            }

            subprojects {
                apply plugin: 'java'

                dependencies {
                    implementation 'org.slf4j:slf4j-api:1.7.26'
                    implementation 'com.google.guava:guava:29.0-jre'
                    compileOnly 'org.projectlombok:lombok:1.18.12'
                    testImplementation 'org.junit.jupiter:junit-jupiter-api:5.6.2'
                    testRuntimeOnly 'org.junit.jupiter:junit-jupiter-engine:5.6.2'
                }

                repositories {
                    maven { url = uri("https://maven.example.com/releases") }
                }
            }
        "#;

        let (runtime, test, provided, repos) = gradle_subprojects_deps(root);

        assert_eq!(runtime.len(), 2);
        assert_eq!(runtime[0].artifact, "slf4j-api");
        assert_eq!(runtime[1].artifact, "guava");

        assert_eq!(provided.len(), 1);
        assert_eq!(provided[0].artifact, "lombok");

        assert_eq!(test.len(), 1);
        assert_eq!(test[0].artifact, "junit-jupiter-api");

        assert!(repos.contains_key("https://maven.example.com/releases"));
    }

    #[test]
    fn gradle_subprojects_deps_allprojects_also_works() {
        let root = r#"
            allprojects {
                dependencies {
                    implementation 'com.google.guava:guava:30.0-jre'
                }
            }
        "#;

        let (runtime, test, provided, repos) = gradle_subprojects_deps(root);
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime[0].artifact, "guava");
        assert!(test.is_empty());
        assert!(provided.is_empty());
        assert!(repos.is_empty());
    }

    #[test]
    fn gradle_subprojects_deps_empty_when_no_subprojects() {
        let root = r#"
            plugins { id 'java' }

            dependencies {
                implementation 'com.google.guava:guava:30.0-jre'
            }
        "#;

        let (runtime, test, provided, repos) = gradle_subprojects_deps(root);
        assert!(runtime.is_empty());
        assert!(test.is_empty());
        assert!(provided.is_empty());
        assert!(repos.is_empty());
    }

    #[test]
    fn gradle_subprojects_deps_kotlin_named_style() {
        let root = r#"
            subprojects {
                dependencies {
                    implementation(group = "org.slf4j", name = "slf4j-api", version = "1.7.26")
                    compileOnly 'org.projectlombok:lombok:1.18.12'
                }
            }
        "#;

        let (runtime, _test, provided, _repos) = gradle_subprojects_deps(root);
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime[0].artifact, "slf4j-api");
        assert_eq!(provided.len(), 1);
        assert_eq!(provided[0].artifact, "lombok");
    }
}
