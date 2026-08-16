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
//  jip — Dependency Resolver
//  ---------------------------------------------------------------------------
//  The core of jip.  Starting from the direct dependencies declared in
//  `jip.toml`, it walks the POM files to discover all transitive
//  dependencies and picks one version for every artifact.
//
//  Version conflicts are resolved with Maven's "nearest wins" rule:
//  the version closest to the project root (smallest depth) wins.
//  POMs are fetched once and kept in memory for the whole run.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use anyhow::{Context, bail};

use crate::artifact::Artifact;
use crate::cache::download_repo_text;
use crate::config::ProjectConfig;
use crate::pom::{Pom, is_runtime_dependency, parse_pom};

/// Default repository jip downloads from.
pub const DEFAULT_REPO_URL: &str = "https://repo1.maven.org/maven2";

/// How far the POM parent chain may be followed before giving up.
const MAX_PARENT_CHAIN: usize = 20;

/// One transitive dependency with a fully resolved version.
#[derive(Debug, Clone)]
pub struct ResolvedDependency {
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
    /// Artifacts excluded for this dependency edge (Maven `<exclusions>`).
    pub exclusions: Vec<(String, String)>,
}

impl ResolvedDependency {
    fn key(&self) -> String {
        format!("{}:{}", self.group_id, self.artifact_id)
    }

    fn to_artifact(&self) -> Artifact {
        Artifact {
            group: self.group_id.clone(),
            artifact: self.artifact_id.clone(),
            version: self.version.clone(),
        }
    }
}

/// An artifact that won a version conflict.
#[derive(Debug, Clone)]
struct Chosen {
    artifact: Artifact,
    depth: usize,
}

/// One item waiting to be resolved during the breadth-first walk.
struct WorkItem {
    artifact: Artifact,
    depth: usize,
    /// Artifacts to skip below this item (from Maven `<exclusions>`).
    skip: HashSet<String>,
}

/// The result of a full resolution.
#[derive(Debug)]
pub struct Resolution {
    /// All artifacts that end up on the classpath, one entry per artifact.
    pub flat: Vec<Artifact>,
    /// The dependency tree, rooted at the direct dependencies.
    pub tree: Vec<DependencyNode>,
}

/// A node in the resolved dependency tree.
#[derive(Debug)]
pub struct DependencyNode {
    pub artifact: Artifact,
    pub children: Vec<DependencyNode>,
}

/// Property map and managed-version map inherited from a POM chain.
struct EffectiveContext {
    properties: HashMap<String, String>,
    managed: HashMap<(String, String), String>,
}

/// Fetches POMs from a repository and resolves the dependency graph.
pub struct Resolver {
    client: reqwest::blocking::Client,
    /// Repositories tried in order, Maven Central last.
    repos: Vec<String>,
    /// POMs fetched during this run, keyed by `group:artifact:version`.
    pom_cache: HashMap<String, Pom>,
}

impl Resolver {
    pub fn new(client: reqwest::blocking::Client, repos: &[String]) -> Self {
        Self {
            client,
            repos: repos.to_vec(),
            pom_cache: HashMap::new(),
        }
    }

    /// Resolve the runtime dependencies declared in `config` into a flat
    /// classpath list plus a tree for display.
    pub fn resolve_project(&mut self, config: &ProjectConfig) -> anyhow::Result<Resolution> {
        self.resolve_dependencies(&config.dependencies)
    }

    /// Resolve the compile-only (`provided`) dependencies declared in
    /// `config` the same way.
    pub fn resolve_project_provided(
        &mut self,
        config: &ProjectConfig,
    ) -> anyhow::Result<Resolution> {
        self.resolve_dependencies(&config.provided_dependencies)
    }

    /// Resolve the test dependencies declared in `config` the same way.
    pub fn resolve_project_tests(&mut self, config: &ProjectConfig) -> anyhow::Result<Resolution> {
        self.resolve_dependencies(&config.test_dependencies)
    }

    /// Walk one set of direct dependencies (runtime or test) into a flat
    /// classpath list plus a tree for display.
    fn resolve_dependencies(
        &mut self,
        deps: &BTreeMap<String, String>,
    ) -> anyhow::Result<Resolution> {
        let mut chosen: HashMap<String, Chosen> = HashMap::new();
        let mut queue: VecDeque<WorkItem> = VecDeque::new();

        // Seed the queue with the direct dependencies at depth 0.
        for (key, version) in deps {
            let artifact = Artifact::parse(&format!("{key}:{version}"))
                .with_context(|| format!("dependency \"{key}:{version}\" in jip.toml"))?;
            queue.push_back(WorkItem {
                artifact,
                depth: 0,
                skip: HashSet::new(),
            });
        }

        // Breadth-first walk.  Because we visit by increasing depth, the
        // first version seen for an artifact is the nearest one (Maven rule).
        while let Some(item) = queue.pop_front() {
            let key = item.artifact.key();
            if item.skip.contains(&key) {
                continue;
            }
            if let Some(entry) = chosen.get(&key) {
                // A version already chosen at equal or shallower depth wins.
                if entry.depth <= item.depth {
                    continue;
                }
            }
            chosen.insert(
                key.clone(),
                Chosen {
                    artifact: item.artifact.clone(),
                    depth: item.depth,
                },
            );

            // Walk this artifact's own dependencies.
            for dep in self.effective_dependencies(&item.artifact)? {
                let dep_key = dep.key();
                if item.skip.contains(&dep_key) {
                    continue;
                }
                let mut child_skip = item.skip.clone();
                for (excluded_group, excluded_artifact) in &dep.exclusions {
                    child_skip.insert(format!("{excluded_group}:{excluded_artifact}"));
                }
                queue.push_back(WorkItem {
                    artifact: dep.to_artifact(),
                    depth: item.depth + 1,
                    skip: child_skip,
                });
            }
        }

        let flat: Vec<Artifact> = chosen
            .values()
            .map(|entry| entry.artifact.clone())
            .collect();

        // Build the display tree from the final (nearest-wins) versions.
        let mut visited = HashSet::new();
        let mut tree = Vec::new();
        for (key, entry) in &chosen {
            if entry.depth == 0
                && let Some(node) = self.build_tree(&chosen, key, &mut visited)
            {
                tree.push(node);
            }
        }

        Ok(Resolution { flat, tree })
    }

    /// The effective dependency list of one artifact: POM dependencies with
    /// versions fully resolved (explicit, via `dependencyManagement`, or via
    /// properties) and filtered to runtime scope.
    fn effective_dependencies(
        &mut self,
        artifact: &Artifact,
    ) -> anyhow::Result<Vec<ResolvedDependency>> {
        let context = self.effective_context(artifact)?;
        let pom_key = format!("{}:{}", artifact.key(), artifact.version);
        let pom = self
            .pom_cache
            .get(&pom_key)
            .expect("effective_context caches the POM");

        let mut result = Vec::new();
        for dep in &pom.dependencies {
            if !is_runtime_dependency(dep) {
                continue;
            }
            let group = interpolate(&dep.group_id, &context.properties);
            let name = interpolate(&dep.artifact_id, &context.properties);
            if group.contains("${") || name.contains("${") {
                bail!("unresolved placeholder in dependency \"{group}:{name}\" of {artifact}");
            }

            let version = match &dep.version {
                Some(v) => interpolate(v, &context.properties),
                None => context
                    .managed
                    .get(&(group.clone(), name.clone()))
                    .map(|v| interpolate(v, &context.properties))
                    .unwrap_or_default(),
            };
            if version.is_empty() || version.contains("${") {
                bail!(
                    "no version for \"{group}:{name}\" declared by {artifact} \
                     (set one explicitly or via dependencyManagement)"
                );
            }

            result.push(ResolvedDependency {
                group_id: group,
                artifact_id: name,
                version,
                exclusions: dep.exclusions.clone(),
            });
        }
        Ok(result)
    }

    /// Collect the properties and managed versions that apply to a POM,
    /// merging its whole parent chain (closer POMs override farther ones).
    fn effective_context(&mut self, artifact: &Artifact) -> anyhow::Result<EffectiveContext> {
        self.fetch_pom(artifact)?;
        let pom_key = format!("{}:{}", artifact.key(), artifact.version);
        let pom = self
            .pom_cache
            .get(&pom_key)
            .expect("fetch_pom just inserted the POM")
            .clone();

        // Start with the built-in properties Maven always provides.
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

        // Walk the parent chain, nearest POM first.
        let mut chain = vec![pom.clone()];
        let mut parent = pom.parent.clone();
        let mut hops = 0;
        while let Some(parent_ref) = parent {
            hops += 1;
            if hops > MAX_PARENT_CHAIN {
                bail!("parent chain of {artifact} is longer than {MAX_PARENT_CHAIN} POMs");
            }
            let parent_group = interpolate(&parent_ref.group_id, &properties);
            let parent_name = interpolate(&parent_ref.artifact_id, &properties);
            let parent_version = interpolate(&parent_ref.version, &properties);
            if parent_group.contains("${")
                || parent_name.contains("${")
                || parent_version.contains("${")
            {
                bail!(
                    "cannot resolve parent coordinates of {artifact}: \
                     {}:{}:{}",
                    parent_ref.group_id,
                    parent_ref.artifact_id,
                    parent_ref.version
                );
            }
            let parent_artifact = Artifact {
                group: parent_group,
                artifact: parent_name,
                version: parent_version,
            };
            self.fetch_pom(&parent_artifact)?;
            let parent_key = format!("{}:{}", parent_artifact.key(), parent_artifact.version);
            let parent_pom = self
                .pom_cache
                .get(&parent_key)
                .expect("fetch_pom just inserted the parent POM")
                .clone();
            parent = parent_pom.parent.clone();
            chain.push(parent_pom);
        }

        // Inherit missing coordinates from the parent chain (nearest wins), so
        // built-ins like `${project.version}` reflect the effective POM.
        for pom in &chain {
            if let (Some(group), false) = (
                pom.group_id.as_ref(),
                properties.contains_key("project.groupId"),
            ) {
                properties.insert("project.groupId".to_string(), group.clone());
            }
            if let (Some(name), false) = (
                pom.artifact_id.as_ref(),
                properties.contains_key("project.artifactId"),
            ) {
                properties.insert("project.artifactId".to_string(), name.clone());
            }
            if let (Some(version), false) = (
                pom.version.as_ref(),
                properties.contains_key("project.version"),
            ) {
                properties.insert("project.version".to_string(), version.clone());
            }
        }

        // Merge farthest first so closer POMs override.
        let mut managed = HashMap::new();
        for pom in chain.iter().rev() {
            for (key, value) in &pom.properties {
                properties.insert(key.clone(), interpolate(value, &properties));
            }
            for dep in &pom.managed_dependencies {
                let group = interpolate(&dep.group_id, &properties);
                let name = interpolate(&dep.artifact_id, &properties);
                let version = dep
                    .version
                    .as_deref()
                    .map(|v| interpolate(v, &properties))
                    .unwrap_or_default();
                managed.insert((group, name), version);
            }
        }

        Ok(EffectiveContext {
            properties,
            managed,
        })
    }

    /// Download and parse the POM of `artifact`, unless already cached.
    fn fetch_pom(&mut self, artifact: &Artifact) -> anyhow::Result<()> {
        let key = format!("{}:{}", artifact.key(), artifact.version);
        if self.pom_cache.contains_key(&key) {
            return Ok(());
        }
        let pom_path = format!("{}{}", artifact.directory(), artifact.pom_file_name());
        let mut tried = Vec::new();
        for repo in &self.repos {
            match download_repo_text(&self.client, repo, &pom_path) {
                Ok(xml) => {
                    let pom = parse_pom(&xml)
                        .with_context(|| format!("invalid POM at {repo}/{pom_path}"))?;
                    self.pom_cache.insert(key, pom);
                    return Ok(());
                }
                Err(err) => tried.push(format!("  {repo}: {err}")),
            }
        }
        // Maven tolerates a missing POM when the jar is available: it warns
        // "The POM for X is missing, no dependency information available"
        // and keeps the artifact without transitive dependencies.  Mirror
        // that so jar-only local repositories resolve.
        if let Some(found_in) = self.jar_exists(artifact) {
            crate::console::warn(&format!(
                "POM for {key} is missing, no dependency information available — using {found_in}"
            ));
            self.pom_cache.insert(
                key,
                Pom {
                    group_id: Some(artifact.group.clone()),
                    artifact_id: Some(artifact.artifact.clone()),
                    version: Some(artifact.version.clone()),
                    parent: None,
                    properties: HashMap::new(),
                    managed_dependencies: Vec::new(),
                    dependencies: Vec::new(),
                    repositories: Vec::new(),
                    compiler_release: None,
                    compiler_source: None,
                },
            );
            return Ok(());
        }
        bail!(
            "cannot download POM for {key} — not found in any repository:\n{}\n\
             check the version, or fix the [repositories] entries in {}",
            tried.join("\n"),
            crate::config::CONFIG_FILE
        )
    }

    /// The repository that carries the artifact's jar, when the POM is
    /// missing.  `file://` repositories are checked on disk; HTTP
    /// repositories via a HEAD request.
    fn jar_exists(&self, artifact: &Artifact) -> Option<String> {
        let jar_path = format!("{}{}", artifact.directory(), artifact.jar_file_name());
        for repo in &self.repos {
            if let Some(base_dir) = crate::cache::file_url_path(repo) {
                if base_dir.join(&jar_path).is_file() {
                    return Some(repo.clone());
                }
            } else {
                let url = format!("{repo}/{jar_path}");
                if self
                    .client
                    .head(&url)
                    .send()
                    .is_ok_and(|response| response.status().is_success())
                {
                    return Some(repo.clone());
                }
            }
        }
        None
    }

    /// Build the display tree for the final chosen versions, following the
    /// `chosen` map so that only winning versions appear.
    fn build_tree(
        &mut self,
        chosen: &HashMap<String, Chosen>,
        key: &str,
        visited: &mut HashSet<String>,
    ) -> Option<DependencyNode> {
        let entry = chosen.get(key)?;
        // Guard against dependency cycles (A -> B -> A).
        if !visited.insert(key.to_string()) {
            return None;
        }
        let artifact = entry.artifact.clone();
        let mut node = DependencyNode {
            artifact: artifact.clone(),
            children: Vec::new(),
        };

        let dependencies = self.effective_dependencies(&artifact).ok()?;
        for dep in dependencies {
            let child_key = dep.key();
            if let Some(child) = self.build_tree(chosen, &child_key, visited) {
                node.children.push(child);
            }
        }
        visited.remove(key);
        Some(node)
    }
}

/// Replace `${...}` placeholders using the given properties.
///
/// A few passes are made so that nested references like
/// `${foo}` where `foo` = `${bar}` also resolve.
pub(crate) fn interpolate(value: &str, properties: &HashMap<String, String>) -> String {
    let mut result = value.to_string();
    for _ in 0..5 {
        let mut changed = false;
        let mut output = String::new();
        let mut rest = result.as_str();
        while let Some(start) = rest.find("${") {
            output.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            match after.find('}') {
                Some(end) => {
                    let key = &after[..end];
                    if let Some(replacement) = properties.get(key) {
                        output.push_str(replacement);
                        changed = true;
                    } else {
                        // Keep the placeholder literal; the caller will detect it.
                        output.push_str(&rest[start..=start + 2 + key.len()]);
                    }
                    rest = &after[end + 1..];
                }
                None => {
                    output.push_str(rest);
                    rest = "";
                }
            }
        }
        output.push_str(rest);
        result = output;
        if !changed {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_placeholders() {
        let mut properties = HashMap::new();
        properties.insert("revision".to_string(), "1.2.3".to_string());
        properties.insert("nested".to_string(), "${revision}".to_string());
        assert_eq!(interpolate("${revision}", &properties), "1.2.3");
        assert_eq!(interpolate("v${nested}", &properties), "v1.2.3");
        // Unknown placeholders stay literal so the caller can report them.
        assert_eq!(interpolate("${unknown}", &properties), "${unknown}");
    }

    #[test]
    fn parses_a_minimal_pom() {
        let xml = r#"
            <project xmlns="http://maven.apache.org/POM/4.0.0">
              <groupId>com.example</groupId>
              <artifactId>demo</artifactId>
              <version>1.0.0</version>
              <dependencies>
                <dependency>
                  <groupId>junit</groupId>
                  <artifactId>junit</artifactId>
                  <version>4.13.2</version>
                  <scope>test</scope>
                </dependency>
              </dependencies>
            </project>
        "#;
        let pom = parse_pom(xml).unwrap();
        assert_eq!(pom.group_id.as_deref(), Some("com.example"));
        assert_eq!(pom.dependencies.len(), 1);
        assert!(!is_runtime_dependency(&pom.dependencies[0])); // test scope
    }

    #[test]
    fn resolves_jar_when_pom_is_missing() {
        // A local repository with the jar but no POM (like the BeetRoot lib/repo).
        let dir = std::env::temp_dir().join(format!("jip-resolver-{}", std::process::id()));
        let artifact_dir = dir.join("repo/com/example/widget/1.0.0");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        std::fs::write(artifact_dir.join("widget-1.0.0.jar"), b"jar bytes").unwrap();

        let repo_url = format!("file://{}", dir.join("repo").display());
        let mut resolver = Resolver::new(reqwest::blocking::Client::new(), &[repo_url]);
        let artifact = Artifact::parse("com.example:widget:1.0.0").unwrap();
        resolver.fetch_pom(&artifact).unwrap();

        let key = "com.example:widget:1.0.0";
        let pom = resolver.pom_cache.get(key).expect("leaf POM inserted");
        assert!(pom.dependencies.is_empty());
        assert_eq!(pom.artifact_id.as_deref(), Some("widget"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_pom_and_jar_is_an_error() {
        let mut resolver = Resolver::new(reqwest::blocking::Client::new(), &[]);
        let artifact = Artifact::parse("com.example:missing:1.0.0").unwrap();
        let err = resolver.fetch_pom(&artifact).unwrap_err();
        assert!(format!("{err:#}").contains("cannot download POM"));
    }
}
