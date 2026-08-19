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
//  jip — Multi-Module Project Support
//  ---------------------------------------------------------------------------
//  Detects and orchestrates multi-module Maven and Gradle projects.
//
//  Detection:
//    * Maven: parent POM with `<packaging>pom</packaging>` and `<modules>`.
//    * Gradle: `settings.gradle(.kts)` with `include` directives.
//
//  Each module is converted independently into its own `jip.toml`, with
//  inter-module dependencies resolved to local `target/classes` directories
//  during `jip build` and `jip run`.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-19
// =============================================================================

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::bail;
use regex::Regex;

use crate::config::{CONFIG_FILE, ProjectConfig};
use crate::pom::{Pom, parse_pom};

/// A discovered module in a multi-module project.
#[derive(Debug, Clone)]
pub struct ModuleInfo {
    /// Logical name (directory name or explicit name from settings).
    pub name: String,
    /// Relative path from the project root.
    pub path: String,
    /// Artifact ID from the POM (for inter-module dependency matching).
    #[allow(dead_code)]
    pub artifact_id: Option<String>,
    /// Inter-module dependencies (artifact IDs of sibling modules).
    pub depends_on: Vec<String>,
}

/// The result of detecting a multi-module project layout.
#[derive(Debug)]
pub struct MultiModuleLayout {
    /// All modules in the project.
    pub modules: Vec<ModuleInfo>,
    /// The build system type.
    pub build_system: BuildSystem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildSystem {
    Maven,
    GradleGroovy,
    GradleKotlin,
}

impl std::fmt::Display for BuildSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildSystem::Maven => write!(f, "Maven"),
            BuildSystem::GradleGroovy => write!(f, "Gradle (Groovy)"),
            BuildSystem::GradleKotlin => write!(f, "Gradle (Kotlin)"),
        }
    }
}

/// Detect whether the current directory is a multi-module project.
///
/// Returns `Some(MultiModuleLayout)` when a parent POM or settings file
/// with multiple modules is found, `None` for single-module projects.
pub fn detect_multi_module() -> Option<MultiModuleLayout> {
    if let Some(layout) = detect_maven_multi_module() {
        return Some(layout);
    }
    if let Some(layout) = detect_gradle_multi_module() {
        return Some(layout);
    }
    None
}

/// Detect a Maven multi-module project from the parent POM.
fn detect_maven_multi_module() -> Option<MultiModuleLayout> {
    let xml = fs::read_to_string("pom.xml").ok()?;
    let pom = parse_pom(&xml).ok()?;

    let modules = parse_maven_modules(&pom)?;
    if modules.len() < 2 {
        return None;
    }

    Some(MultiModuleLayout {
        modules,
        build_system: BuildSystem::Maven,
    })
}

/// Extract `<modules>` from a parsed POM, resolving inter-module dependencies.
fn parse_maven_modules(parent_pom: &Pom) -> Option<Vec<ModuleInfo>> {
    // We need to find <modules> in the XML — but our Pom struct doesn't
    // store them. Parse the XML again for the module list.
    // This is a deliberate limitation: we only detect multi-module when
    // we can re-read the POM XML.
    let xml = fs::read_to_string("pom.xml").ok()?;
    let doc = roxmltree::Document::parse(&xml).ok()?;
    let project = doc.root_element();

    // Find <modules> element.
    let modules_node = project
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "modules")?;

    let module_names: Vec<String> = modules_node
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "module")
        .filter_map(|n| n.text().map(str::trim).map(str::to_string))
        .collect();

    if module_names.len() < 2 {
        return None;
    }

    // Resolve parent properties for groupId/version interpolation.
    let parent_group = parent_pom.group_id.as_deref().unwrap_or("");

    // Build sibling artifact IDs from child POMs.
    let sibling_aids: HashSet<String> = module_names
        .iter()
        .filter_map(|name| {
            let pom_path = Path::new(name).join("pom.xml");
            let xml = fs::read_to_string(pom_path).ok()?;
            let p = parse_pom(&xml).ok()?;
            p.artifact_id
        })
        .collect();

    // Build artifact ID → module name mapping.
    let aid_to_name: HashMap<String, String> = module_names
        .iter()
        .filter_map(|name| {
            let pom_path = Path::new(name).join("pom.xml");
            let xml = fs::read_to_string(pom_path).ok()?;
            let p = parse_pom(&xml).ok()?;
            let aid = p.artifact_id?;
            Some((aid, name.clone()))
        })
        .collect();

    let mut modules = Vec::new();
    for name in &module_names {
        let module_path = name;
        let module_pom_path = Path::new(module_path).join("pom.xml");

        let (artifact_id, depends_on) = if let Ok(module_xml) = fs::read_to_string(&module_pom_path)
        {
            if let Ok(module_pom) = parse_pom(&module_xml) {
                let aid = module_pom.artifact_id.clone();
                let deps = collect_inter_module_deps(&module_pom, parent_group, &sibling_aids);
                // Map artifact IDs back to module names for topological sort.
                let dep_names: Vec<String> = deps
                    .iter()
                    .filter_map(|aid| aid_to_name.get(aid).cloned())
                    .collect();
                (aid, dep_names)
            } else {
                (None, Vec::new())
            }
        } else {
            (None, Vec::new())
        };

        modules.push(ModuleInfo {
            name: name.clone(),
            path: module_path.clone(),
            artifact_id,
            depends_on,
        });
    }

    Some(modules)
}

/// Collect inter-module dependency artifact IDs from a child POM.
///
/// A dependency is considered inter-module when its groupId matches the
/// parent groupId AND its artifactId matches another module's artifactId.
fn collect_inter_module_deps(
    pom: &Pom,
    parent_group: &str,
    sibling_aids: &HashSet<String>,
) -> Vec<String> {
    let mut deps = Vec::new();
    for dep in &pom.dependencies {
        let group_match = dep.group_id == parent_group || dep.group_id == "${project.groupId}";
        if group_match
            && sibling_aids.contains(&dep.artifact_id)
            && !deps.contains(&dep.artifact_id)
        {
            deps.push(dep.artifact_id.clone());
        }
    }
    deps
}

/// Detect a Gradle multi-module project from `settings.gradle(.kts)`.
fn detect_gradle_multi_module() -> Option<MultiModuleLayout> {
    let (content, build_system) = if let Ok(content) = fs::read_to_string("settings.gradle") {
        (content, BuildSystem::GradleGroovy)
    } else if let Ok(content) = fs::read_to_string("settings.gradle.kts") {
        (content, BuildSystem::GradleKotlin)
    } else {
        return None;
    };

    let module_names = parse_gradle_modules(&content);
    if module_names.len() < 2 {
        return None;
    }

    let modules = module_names
        .into_iter()
        .map(|name| {
            let depends_on = collect_gradle_inter_module_deps(&name);
            ModuleInfo {
                artifact_id: Some(name.clone()),
                depends_on,
                name: name.clone(),
                path: name,
            }
        })
        .collect();

    Some(MultiModuleLayout {
        modules,
        build_system,
    })
}

/// Parse `include` directives from a Gradle settings file.
///
/// Handles:
///   * `include 'a', 'b', 'c'`
///   * `include("a", "b")`
///   * `include ':a', ':b'`
///   * `include 'app', 'lib:core'` (nested)
fn parse_gradle_modules(content: &str) -> Vec<String> {
    let mut modules = Vec::new();
    // Match the keyword `include` and capture everything after it up to
    // the next line or closing paren.
    let re = Regex::new(r"(?m)^.*?\binclude\b\s*(\([^)]*\)|[^\n]+)").unwrap();
    for cap in re.captures_iter(content) {
        let args_str = cap.get(1).unwrap().as_str();
        // Strip outer parens if present.
        let args_str = args_str
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(args_str);
        for item in args_str.split(',') {
            let item = item.trim().trim_matches('\'').trim_matches('"');
            let item = item.trim_start_matches(':');
            if !item.is_empty() {
                modules.push(item.replace('/', std::path::MAIN_SEPARATOR_STR));
            }
        }
    }
    modules.sort();
    modules.dedup();
    modules
}

/// Collect inter-module dependencies for a Gradle module.
///
/// Reads the module's `build.gradle(.kts)` and looks for `project(':...')`
/// references.
fn collect_gradle_inter_module_deps(module_name: &str) -> Vec<String> {
    let build_file = if Path::new(&format!("{module_name}/build.gradle.kts")).exists() {
        format!("{module_name}/build.gradle.kts")
    } else if Path::new(&format!("{module_name}/build.gradle")).exists() {
        format!("{module_name}/build.gradle")
    } else {
        return Vec::new();
    };

    let content = match fs::read_to_string(&build_file) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let re = Regex::new(r#"project\s*\(\s*['"]:([^'"]+)['"]\s*\)"#).unwrap();
    re.captures_iter(&content)
        .filter_map(|cap| {
            let dep = cap
                .get(1)?
                .as_str()
                .replace('/', std::path::MAIN_SEPARATOR_STR);
            Some(dep)
        })
        .collect()
}

/// Topological sort of modules based on their inter-module dependencies.
///
/// Returns modules in build order (dependencies first).  Returns an error
/// when a cycle is detected.
pub fn topological_sort(modules: &[ModuleInfo]) -> anyhow::Result<Vec<ModuleInfo>> {
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

    for m in modules {
        in_degree.entry(m.name.clone()).or_insert(0);
        for dep in &m.depends_on {
            dependents
                .entry(dep.clone())
                .or_default()
                .push(m.name.clone());
            *in_degree.entry(m.name.clone()).or_insert(0) += 1;
        }
    }

    let mut queue: VecDeque<String> = in_degree
        .iter()
        .filter(|entry| *entry.1 == 0)
        .map(|(name, _)| name.clone())
        .collect();

    let mut sorted = Vec::new();
    let module_map: HashMap<String, &ModuleInfo> =
        modules.iter().map(|m| (m.name.clone(), m)).collect();

    while let Some(name) = queue.pop_front() {
        if let Some(m) = module_map.get(&name) {
            sorted.push((*m).clone());
        }
        if let Some(deps) = dependents.get(&name) {
            for dep in deps {
                let degree = in_degree.get_mut(dep).unwrap();
                *degree -= 1;
                if *degree == 0 {
                    queue.push_back(dep.clone());
                }
            }
        }
    }

    if sorted.len() != modules.len() {
        let remaining: Vec<String> = modules
            .iter()
            .filter(|m| !sorted.iter().any(|s| s.name == m.name))
            .map(|m| m.name.clone())
            .collect();
        bail!(
            "cyclic module dependency detected: {}",
            remaining.join(", ")
        );
    }

    Ok(sorted)
}

/// Check if the project is a multi-module project.
pub fn is_multi_module(config: &ProjectConfig) -> bool {
    config
        .modules
        .as_ref()
        .is_some_and(|m| m.modules.len() >= 2)
}

/// Load the `ProjectConfig` for a specific module.
pub fn load_module_config(root: &Path, module_path: &str) -> anyhow::Result<ProjectConfig> {
    let config_path = root.join(module_path).join(CONFIG_FILE);
    ProjectConfig::load(&config_path)
}

/// The compiled classes directory for a module.
pub fn module_classes_dir(root: &Path, module_path: &str) -> PathBuf {
    root.join(module_path)
        .join(crate::commands::build::CLASSES_DIR)
}

/// Build the flat classpath for a module, including compiled classes from
/// all of its transitive inter-module dependencies.
pub fn module_classpath(
    root: &Path,
    layout: &MultiModuleLayout,
    module: &ModuleInfo,
    external_classpath: &[PathBuf],
) -> Vec<PathBuf> {
    let mut classpath = Vec::new();

    // Add compiled classes from transitive inter-module dependencies.
    let transitive = transitive_dependencies(layout, &module.name);
    for dep_name in &transitive {
        if let Some(dep) = layout.modules.iter().find(|m| &m.name == dep_name) {
            let classes = module_classes_dir(root, &dep.path);
            if classes.exists() {
                classpath.push(classes);
            }
        }
    }

    // Add the module's own compiled classes.
    let own_classes = module_classes_dir(root, &module.path);
    if own_classes.exists() {
        classpath.push(own_classes);
    }

    // Add external dependency jars.
    classpath.extend(external_classpath.iter().cloned());

    classpath
}

/// Compute the transitive closure of inter-module dependencies for a module.
fn transitive_dependencies(layout: &MultiModuleLayout, module_name: &str) -> Vec<String> {
    let mut visited = HashSet::new();
    let mut result = Vec::new();
    let module_map: HashMap<String, &ModuleInfo> =
        layout.modules.iter().map(|m| (m.name.clone(), m)).collect();

    fn dfs(
        name: &str,
        module_map: &HashMap<String, &ModuleInfo>,
        visited: &mut HashSet<String>,
        result: &mut Vec<String>,
    ) {
        if !visited.insert(name.to_string()) {
            return;
        }
        if let Some(m) = module_map.get(name) {
            for dep in &m.depends_on {
                dfs(dep, module_map, visited, result);
                if !result.contains(dep) {
                    result.push(dep.clone());
                }
            }
        }
    }

    dfs(module_name, &module_map, &mut visited, &mut result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gradle_include_single() {
        let settings = "include 'common', 'base', 'app'";
        let modules = parse_gradle_modules(settings);
        assert_eq!(modules, vec!["app", "base", "common"]);
    }

    #[test]
    fn parse_gradle_include_parens() {
        let settings = r#"include("lib:core", "lib:api")"#;
        let modules = parse_gradle_modules(settings);
        assert_eq!(modules, vec!["lib:api", "lib:core"]);
    }

    #[test]
    fn parse_gradle_include_colon_prefix() {
        let settings = "include ':module-a', ':module-b'";
        let modules = parse_gradle_modules(settings);
        assert_eq!(modules, vec!["module-a", "module-b"]);
    }

    #[test]
    fn parse_gradle_include_double_quotes() {
        let settings = r#"include "app", "lib""#;
        let modules = parse_gradle_modules(settings);
        assert_eq!(modules, vec!["app", "lib"]);
    }

    #[test]
    fn topological_sort_linear() {
        let modules = vec![
            ModuleInfo {
                name: "c".into(),
                path: "c".into(),
                artifact_id: None,
                depends_on: vec!["b".into()],
            },
            ModuleInfo {
                name: "a".into(),
                path: "a".into(),
                artifact_id: None,
                depends_on: vec![],
            },
            ModuleInfo {
                name: "b".into(),
                path: "b".into(),
                artifact_id: None,
                depends_on: vec!["a".into()],
            },
        ];
        let sorted = topological_sort(&modules).unwrap();
        let names: Vec<&str> = sorted.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn topological_sort_diamond() {
        let modules = vec![
            ModuleInfo {
                name: "root".into(),
                path: "root".into(),
                artifact_id: None,
                depends_on: vec!["left".into(), "right".into()],
            },
            ModuleInfo {
                name: "left".into(),
                path: "left".into(),
                artifact_id: None,
                depends_on: vec!["base".into()],
            },
            ModuleInfo {
                name: "right".into(),
                path: "right".into(),
                artifact_id: None,
                depends_on: vec!["base".into()],
            },
            ModuleInfo {
                name: "base".into(),
                path: "base".into(),
                artifact_id: None,
                depends_on: vec![],
            },
        ];
        let sorted = topological_sort(&modules).unwrap();
        let names: Vec<&str> = sorted.iter().map(|m| m.name.as_str()).collect();
        // base must come before left and right; left and right before root.
        let base_pos = names.iter().position(|&n| n == "base").unwrap();
        let left_pos = names.iter().position(|&n| n == "left").unwrap();
        let right_pos = names.iter().position(|&n| n == "right").unwrap();
        let root_pos = names.iter().position(|&n| n == "root").unwrap();
        assert!(base_pos < left_pos);
        assert!(base_pos < right_pos);
        assert!(left_pos < root_pos);
        assert!(right_pos < root_pos);
    }

    #[test]
    fn topological_sort_cycle_detected() {
        let modules = vec![
            ModuleInfo {
                name: "a".into(),
                path: "a".into(),
                artifact_id: None,
                depends_on: vec!["b".into()],
            },
            ModuleInfo {
                name: "b".into(),
                path: "b".into(),
                artifact_id: None,
                depends_on: vec!["a".into()],
            },
        ];
        assert!(topological_sort(&modules).is_err());
    }

    #[test]
    fn transitive_deps_linear() {
        let layout = MultiModuleLayout {
            modules: vec![
                ModuleInfo {
                    name: "a".into(),
                    path: "a".into(),
                    artifact_id: None,
                    depends_on: vec![],
                },
                ModuleInfo {
                    name: "b".into(),
                    path: "b".into(),
                    artifact_id: None,
                    depends_on: vec!["a".into()],
                },
                ModuleInfo {
                    name: "c".into(),
                    path: "c".into(),
                    artifact_id: None,
                    depends_on: vec!["b".into()],
                },
            ],
            build_system: BuildSystem::Maven,
        };
        let deps = transitive_dependencies(&layout, "c");
        assert!(deps.contains(&"a".to_string()));
        assert!(deps.contains(&"b".to_string()));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    fn make_pom(deps_xml: &str) -> Pom {
        let xml = format!(
            "<project>\
                <groupId>org.nanohttpd</groupId>\
                <artifactId>test</artifactId>\
                <version>1.0</version>\
                <dependencies>{}</dependencies>\
            </project>",
            deps_xml
        );
        parse_pom(&xml).unwrap()
    }

    #[test]
    fn collect_inter_module_deps_with_property_group() {
        let parent_group = "org.nanohttpd";
        let sibling_aids: HashSet<String> = [
            "nanohttpd",
            "nanohttpd-samples",
            "nanohttpd-webserver",
            "nanohttpd-websocket",
            "nanohttpd-webserver-markdown-plugin",
            "nanohttpd-nanolets",
            "nanohttpd-apache-fileupload",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        // core: no deps
        let pom = make_pom("");
        let deps = collect_inter_module_deps(&pom, parent_group, &sibling_aids);
        assert!(deps.is_empty());

        // samples: depends on core + webserver via ${project.groupId}
        let pom = make_pom(
            "<dependency><groupId>${project.groupId}</groupId><artifactId>nanohttpd</artifactId><version>1.0</version></dependency>\
             <dependency><groupId>${project.groupId}</groupId><artifactId>nanohttpd-webserver</artifactId><version>1.0</version></dependency>",
        );
        let deps = collect_inter_module_deps(&pom, parent_group, &sibling_aids);
        assert_eq!(deps, vec!["nanohttpd", "nanohttpd-webserver"]);

        // markdown-plugin: depends on core + webserver + external junit
        let pom = make_pom(
            "<dependency><groupId>${project.groupId}</groupId><artifactId>nanohttpd</artifactId><version>1.0</version></dependency>\
             <dependency><groupId>${project.groupId}</groupId><artifactId>nanohttpd-webserver</artifactId><version>1.0</version></dependency>\
             <dependency><groupId>junit</groupId><artifactId>junit</artifactId><version>4.12</version><scope>test</scope></dependency>",
        );
        let deps = collect_inter_module_deps(&pom, parent_group, &sibling_aids);
        assert_eq!(deps, vec!["nanohttpd", "nanohttpd-webserver"]);
        assert!(!deps.contains(&"junit".to_string()));

        // fileupload: depends on core via explicit groupId
        let pom = make_pom(
            "<dependency><groupId>org.nanohttpd</groupId><artifactId>nanohttpd</artifactId><version>1.0</version></dependency>",
        );
        let deps = collect_inter_module_deps(&pom, parent_group, &sibling_aids);
        assert_eq!(deps, vec!["nanohttpd"]);
    }
}
