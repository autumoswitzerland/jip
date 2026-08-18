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
//  jip — POM Parsing
//  ---------------------------------------------------------------------------
//  A POM (Project Object Model) is the XML description of a Maven artifact.
//  It declares the artifact's own dependencies and properties.
//
//  jip only reads the parts that matter for dependency resolution:
//  the coordinates, the parent pointer, the properties, the
//  dependency management section, and the dependency list.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use anyhow::Context;

/// The effective version for a `<scope>` we do not want to resolve.
const SCOPES_TO_IGNORE: [&str; 3] = ["test", "provided", "system"];

/// One artifact's dependency declaration, extracted from a POM.
#[derive(Debug, Clone)]
pub struct PomDependency {
    pub group_id: String,
    pub artifact_id: String,
    pub version: Option<String>,
    /// Maven scope ("compile", "runtime", "test", ...).  Defaults to "compile".
    pub scope: String,
    /// External dependency type; `pom` marks a BOM import.
    pub typ: Option<String>,
    /// `optional` dependencies are excluded from the transitive resolution.
    pub optional: bool,
    /// Coordinates of excluded transitive dependencies.
    pub exclusions: Vec<(String, String)>,
}

/// A reference to another POM this POM inherits from.
#[derive(Debug, Clone)]
pub struct PomParent {
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
}

/// The parsed content of a POM.
#[derive(Debug, Clone)]
pub struct Pom {
    pub group_id: Option<String>,
    pub artifact_id: Option<String>,
    pub version: Option<String>,
    pub parent: Option<PomParent>,
    /// `<properties>` map, used for `${...}` placeholder substitution.
    pub properties: std::collections::HashMap<String, String>,
    /// Versions taken from `<dependencyManagement>`.
    pub managed_dependencies: Vec<PomDependency>,
    /// The artifact's own dependencies.
    pub dependencies: Vec<PomDependency>,
    /// Custom `<repositories>` as `(id, url)` pairs.
    pub repositories: Vec<(String, String)>,
    /// Java version from the `maven-compiler-plugin` `<release>` config.
    pub compiler_release: Option<String>,
    /// Java version from the `maven-compiler-plugin` `<source>` config.
    pub compiler_source: Option<String>,
}

/// Parse POM XML into a [`Pom`] structure.
///
/// The parser is deliberately permissive: any missing element simply results
/// in `None` or an empty list, because not every POM carries every field.
pub fn parse_pom(xml: &str) -> anyhow::Result<Pom> {
    let doc = roxmltree::Document::parse(xml).context("POM is not valid XML")?;
    let project = doc.root_element();

    let parent = child(project, "parent").map(|parent_node| PomParent {
        group_id: text_of(parent_node, "groupId")
            .unwrap_or_default()
            .to_string(),
        artifact_id: text_of(parent_node, "artifactId")
            .unwrap_or_default()
            .to_string(),
        version: text_of(parent_node, "version")
            .unwrap_or_default()
            .to_string(),
    });

    let properties = child(project, "properties")
        .map(|props| {
            props
                .children()
                .filter(|n| n.is_element())
                .filter_map(|n| {
                    n.text()
                        .map(|value| (n.tag_name().name().to_string(), value.trim().to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    let managed_dependencies = child(project, "dependencyManagement")
        .and_then(|dm| child(dm, "dependencies"))
        .map(parse_dependency_list)
        .unwrap_or_default();

    let dependencies = child(project, "dependencies")
        .map(parse_dependency_list)
        .unwrap_or_default();

    let repositories = child(project, "repositories")
        .map(|repos| {
            repos
                .children()
                .filter(|n| n.is_element() && n.tag_name().name() == "repository")
                .filter_map(|repo| {
                    Some((
                        text_of(repo, "id")?.to_string(),
                        text_of(repo, "url")?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    let (compiler_release, compiler_source) = compiler_plugin_java(child(project, "build"));

    Ok(Pom {
        group_id: text_of(project, "groupId").map(str::to_string),
        artifact_id: text_of(project, "artifactId").map(str::to_string),
        version: text_of(project, "version").map(str::to_string),
        parent,
        properties,
        managed_dependencies,
        dependencies,
        repositories,
        compiler_release,
        compiler_source,
    })
}

/// Read the `maven-compiler-plugin` `<release>` and `<source>` values from a
/// `<build>` element, when that plugin is configured.
fn compiler_plugin_java(
    build: Option<roxmltree::Node<'_, '_>>,
) -> (Option<String>, Option<String>) {
    let Some(plugins) = build.and_then(|b| child(b, "plugins")) else {
        return (None, None);
    };
    for plugin in plugins
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "plugin")
    {
        if text_of(plugin, "artifactId") == Some("maven-compiler-plugin") {
            let config = child(plugin, "configuration");
            return (
                config
                    .and_then(|c| text_of(c, "release"))
                    .map(str::to_string),
                config
                    .and_then(|c| text_of(c, "source"))
                    .map(str::to_string),
            );
        }
    }
    (None, None)
}

/// Return the first child element with the given local tag name.
fn child<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> Option<roxmltree::Node<'a, 'a>> {
    node.children()
        .find(|n| n.is_element() && n.tag_name().name() == name)
}

/// Return the trimmed text of the first child element with the given name.
fn text_of<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    child(node, name).and_then(|n| n.text()).map(str::trim)
}

/// Parse a `<dependencies>` element into a list of dependency declarations.
fn parse_dependency_list<'a>(dependencies_node: roxmltree::Node<'a, 'a>) -> Vec<PomDependency> {
    dependencies_node
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "dependency")
        .filter_map(parse_dependency)
        .collect()
}

/// Parse a single `<dependency>` element.
fn parse_dependency(node: roxmltree::Node<'_, '_>) -> Option<PomDependency> {
    let group_id = text_of(node, "groupId")?.to_string();
    let artifact_id = text_of(node, "artifactId")?.to_string();

    let exclusions = child(node, "exclusions")
        .map(|ex| {
            ex.children()
                .filter(|n| n.is_element() && n.tag_name().name() == "exclusion")
                .filter_map(|e| {
                    Some((
                        text_of(e, "groupId")?.to_string(),
                        text_of(e, "artifactId")?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    Some(PomDependency {
        group_id,
        artifact_id,
        version: text_of(node, "version").map(str::to_string),
        scope: text_of(node, "scope").unwrap_or("compile").to_string(),
        typ: text_of(node, "type").map(str::to_string),
        optional: text_of(node, "optional").is_some_and(|o| o.eq_ignore_ascii_case("true")),
        exclusions,
    })
}

/// Decide whether a dependency should be part of the runtime classpath.
///
/// `test` and `provided` dependencies are needed only during compilation or
/// in the test harness, never at runtime, so jip skips them.
pub fn is_runtime_dependency(dep: &PomDependency) -> bool {
    if dep.optional {
        return false;
    }
    !SCOPES_TO_IGNORE.contains(&dep.scope.as_str())
}
