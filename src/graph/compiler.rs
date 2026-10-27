use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::schema::{
    EdgeSpec, ExecutorSpec, GraphSpec, NetworkAuthority, NodeKind, NodeSpec, PortSpec, RiskLevel,
};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompiledGraph {
    pub spec: GraphSpec,
    pub dependencies: BTreeMap<String, Vec<String>>,
    pub waves: Vec<Vec<String>>,
    pub fingerprint: String,
}

pub fn compile(spec: GraphSpec) -> Result<CompiledGraph, String> {
    if spec.version != 1 {
        return Err(format!(
            "unsupported graph version {}; expected version 1",
            spec.version
        ));
    }
    if spec.id.trim().is_empty() {
        return Err("graph id must not be empty".into());
    }
    if spec.nodes.is_empty() {
        return Err("graph must contain at least one node".into());
    }
    if spec.policy.max_attempts_per_node == 0 {
        return Err("policy.max_attempts_per_node must be at least 1".into());
    }

    let mut node_map: HashMap<String, &NodeSpec> = HashMap::new();
    for node in &spec.nodes {
        validate_node(node, &spec)?;
        if node_map.insert(node.id.clone(), node).is_some() {
            return Err(format!("duplicate node id '{}'", node.id));
        }
    }

    let mut dependencies: BTreeMap<String, Vec<String>> = spec
        .nodes
        .iter()
        .map(|n| (n.id.clone(), Vec::new()))
        .collect();

    for edge in &spec.edges {
        validate_edge(edge, &node_map)?;
        let deps = dependencies
            .get_mut(&edge.to.node)
            .expect("validated destination node exists");
        if !deps.contains(&edge.from.node) {
            deps.push(edge.from.node.clone());
        }
    }
    for deps in dependencies.values_mut() {
        deps.sort();
    }

    for node in &spec.nodes {
        if let Some(ExecutorSpec::ArtifactCollect { sources }) = &node.executor {
            for source in sources {
                let dependency = node_map
                    .get(&source.node)
                    .ok_or_else(|| format!("unknown artifact source node '{}'", source.node))?;
                if dependency.kind != NodeKind::Agent
                    || !dependencies[&node.id].contains(&source.node)
                    || !dependency.artifact_paths.contains(&source.path)
                {
                    return Err(format!(
                        "integration source '{}:{}' must be a declared artifact on a direct agent dependency",
                        source.node, source.path
                    ));
                }
                let verifier = node_map
                    .get(&source.verified_by)
                    .ok_or_else(|| format!("unknown artifact verifier '{}'", source.verified_by))?;
                if verifier.kind != NodeKind::Check
                    || verifier.workspace.mode != super::schema::WorkspaceMode::Worktree
                    || !matches!(verifier.executor, Some(ExecutorSpec::Shell { .. }))
                    || !dependencies[&source.verified_by].contains(&source.node)
                    || !dependencies[&node.id].contains(&source.verified_by)
                {
                    return Err(format!(
                        "integration source '{}:{}' requires its direct worktree verifier '{}' as a dependency",
                        source.node, source.path, source.verified_by
                    ));
                }
            }
        }
    }

    validate_acyclic(&dependencies)?;

    for node in &spec.nodes {
        if node.risk == RiskLevel::Irreversible
            && !has_approval_ancestor(&node.id, &dependencies, &node_map, &mut HashSet::new())
        {
            return Err(format!(
                "irreversible node '{}' must have an upstream approval node",
                node.id
            ));
        }
    }

    let waves = build_waves(&dependencies)?;
    let fingerprint = graph_fingerprint(&spec)?;

    Ok(CompiledGraph {
        spec,
        dependencies,
        waves,
        fingerprint,
    })
}

fn validate_node(node: &NodeSpec, spec: &GraphSpec) -> Result<(), String> {
    if node.id.trim().is_empty() {
        return Err("node id must not be empty".into());
    }
    if node.retry.max_attempts == 0 {
        return Err(format!(
            "node '{}' retry.max_attempts must be at least 1",
            node.id
        ));
    }
    if node.retry.max_attempts > spec.policy.max_attempts_per_node {
        return Err(format!(
            "node '{}' requests {} attempts but graph policy allows at most {}",
            node.id, node.retry.max_attempts, spec.policy.max_attempts_per_node
        ));
    }
    if !node.artifact_paths.is_empty() && node.kind != NodeKind::Agent {
        return Err(format!(
            "node '{}' declares artifact paths but is not an agent",
            node.id
        ));
    }
    if node.kind == NodeKind::Integration {
        if node.workspace.mode != super::schema::WorkspaceMode::Worktree {
            return Err(format!(
                "integration node '{}' must use an isolated worktree",
                node.id
            ));
        }
    }
    let mut artifact_paths = std::collections::BTreeSet::new();
    for path in &node.artifact_paths {
        super::schema::validate_artifact_path(path)?;
        if !artifact_paths.insert(path) {
            return Err(format!("node '{}' repeats artifact path '{path}'", node.id));
        }
    }

    if node.authority.network == NetworkAuthority::Allowed && !spec.policy.allow_network {
        return Err(format!(
            "node '{}' requests network authority but graph policy denies network access",
            node.id
        ));
    }
    if node.authority.git_push && !spec.policy.allow_git_push {
        return Err(format!(
            "node '{}' requests git push authority but graph policy denies git push",
            node.id
        ));
    }

    validate_ports(&node.id, "input", &node.inputs)?;
    validate_ports(&node.id, "output", &node.outputs)?;

    match node.kind {
        NodeKind::Approval => {
            if node.executor.is_some() {
                return Err(format!(
                    "approval node '{}' must not declare an executor",
                    node.id
                ));
            }
            require_capability(node, "human.approval")?;
        }
        _ => {
            let executor = node.executor.as_ref().ok_or_else(|| {
                format!(
                    "node '{}' of kind {:?} requires an executor",
                    node.id, node.kind
                )
            })?;
            match executor {
                ExecutorSpec::Shell { command, .. } => {
                    if command.trim().is_empty() {
                        return Err(format!(
                            "node '{}' shell command must not be empty",
                            node.id
                        ));
                    }
                    require_capability(node, "shell.execute")?;
                }
                ExecutorSpec::Provider { capability, .. } => {
                    if capability.trim().is_empty() {
                        return Err(format!(
                            "node '{}' provider capability must not be empty",
                            node.id
                        ));
                    }
                    require_capability(node, capability)?;
                }
                ExecutorSpec::ArtifactCollect { sources } => {
                    if node.kind != NodeKind::Integration || sources.is_empty() {
                        return Err(
                            "artifact collection requires an integration node with at least one source"
                                .into(),
                        );
                    }
                    let mut destinations = BTreeSet::new();
                    for source in sources {
                        super::schema::validate_artifact_path(&source.path)?;
                        super::schema::validate_artifact_path(&source.destination)?;
                        if source.node.trim().is_empty() {
                            return Err("artifact source node must not be empty".into());
                        }
                        if !destinations.insert(&source.destination) {
                            return Err(format!(
                                "integration node '{}' has conflicting destination '{}'",
                                node.id, source.destination
                            ));
                        }
                        if !node
                            .authority
                            .filesystem_write
                            .iter()
                            .any(|scope| write_scope_covers(scope, &source.destination))
                        {
                            return Err(format!(
                                "integration destination '{}' is outside declared filesystem_write authority",
                                source.destination
                            ));
                        }
                    }
                    let ordered: Vec<_> = destinations.iter().copied().collect();
                    for pair in ordered.windows(2) {
                        if pair[1].starts_with(&format!("{}/", pair[0])) {
                            return Err(format!(
                                "integration destinations '{}' and '{}' overlap",
                                pair[0], pair[1]
                            ));
                        }
                    }
                }
            }
        }
    }

    if node.kind == NodeKind::Integration
        && !matches!(node.executor, Some(ExecutorSpec::ArtifactCollect { .. }))
    {
        return Err(format!(
            "integration node '{}' must use artifact_collect executor",
            node.id
        ));
    }

    Ok(())
}

fn write_scope_covers(scope: &str, destination: &str) -> bool {
    let scope = scope.trim().trim_start_matches("./").replace('\\', "/");
    let destination = destination.trim_start_matches("./");
    scope == "**"
        || destination == scope
        || scope.strip_suffix("/**").is_some_and(|prefix| {
            destination == prefix || destination.starts_with(&format!("{prefix}/"))
        })
}

fn require_capability(node: &NodeSpec, capability: &str) -> Result<(), String> {
    if node.requires.iter().any(|required| required == capability) {
        Ok(())
    } else {
        Err(format!(
            "node '{}' executor requires capability '{}' but it is not declared in requires",
            node.id, capability
        ))
    }
}

fn validate_ports(node_id: &str, label: &str, ports: &[PortSpec]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for port in ports {
        if port.name.trim().is_empty() || port.data_type.trim().is_empty() {
            return Err(format!(
                "node '{node_id}' has an empty {label} port name or type"
            ));
        }
        if !seen.insert(port.name.as_str()) {
            return Err(format!(
                "node '{node_id}' has duplicate {label} port '{}'",
                port.name
            ));
        }
    }
    Ok(())
}

fn validate_edge(edge: &EdgeSpec, nodes: &HashMap<String, &NodeSpec>) -> Result<(), String> {
    let from = nodes
        .get(&edge.from.node)
        .ok_or_else(|| format!("edge source node '{}' does not exist", edge.from.node))?;
    let to = nodes
        .get(&edge.to.node)
        .ok_or_else(|| format!("edge destination node '{}' does not exist", edge.to.node))?;

    if edge.from.node == edge.to.node {
        return Err(format!("node '{}' cannot depend on itself", edge.from.node));
    }

    if edge.dependency_only {
        if edge.from.port.is_some() || edge.to.port.is_some() {
            return Err(format!(
                "dependency-only edge {} -> {} must not name ports",
                edge.from.node, edge.to.node
            ));
        }
        return Ok(());
    }

    let from_port = edge.from.port.as_deref().ok_or_else(|| {
        format!(
            "data edge {} -> {} must name from.port",
            edge.from.node, edge.to.node
        )
    })?;
    let to_port = edge.to.port.as_deref().ok_or_else(|| {
        format!(
            "data edge {} -> {} must name to.port",
            edge.from.node, edge.to.node
        )
    })?;

    let from_type = from
        .outputs
        .iter()
        .find(|p| p.name == from_port)
        .map(|p| p.data_type.as_str())
        .ok_or_else(|| {
            format!(
                "node '{}' has no output port '{}'",
                edge.from.node, from_port
            )
        })?;
    let to_type = to
        .inputs
        .iter()
        .find(|p| p.name == to_port)
        .map(|p| p.data_type.as_str())
        .ok_or_else(|| format!("node '{}' has no input port '{}'", edge.to.node, to_port))?;

    if from_type != to_type {
        return Err(format!(
            "edge {}.{} ({}) -> {}.{} ({}) has incompatible types",
            edge.from.node, from_port, from_type, edge.to.node, to_port, to_type
        ));
    }

    Ok(())
}

fn validate_acyclic(dependencies: &BTreeMap<String, Vec<String>>) -> Result<(), String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Visiting,
        Done,
    }

    fn visit(
        node: &str,
        deps: &BTreeMap<String, Vec<String>>,
        states: &mut HashMap<String, State>,
        stack: &mut Vec<String>,
    ) -> Result<(), String> {
        if let Some(state) = states.get(node) {
            if *state == State::Done {
                return Ok(());
            }
            if *state == State::Visiting {
                stack.push(node.to_string());
                return Err(format!("graph contains a cycle: {}", stack.join(" -> ")));
            }
        }

        states.insert(node.to_string(), State::Visiting);
        stack.push(node.to_string());
        if let Some(node_deps) = deps.get(node) {
            for dep in node_deps {
                visit(dep, deps, states, stack)?;
            }
        }
        stack.pop();
        states.insert(node.to_string(), State::Done);
        Ok(())
    }

    let mut states = HashMap::new();
    for node in dependencies.keys() {
        visit(node, dependencies, &mut states, &mut Vec::new())?;
    }
    Ok(())
}

fn has_approval_ancestor(
    node_id: &str,
    dependencies: &BTreeMap<String, Vec<String>>,
    nodes: &HashMap<String, &NodeSpec>,
    visited: &mut HashSet<String>,
) -> bool {
    if !visited.insert(node_id.to_string()) {
        return false;
    }

    dependencies.get(node_id).into_iter().flatten().any(|dep| {
        nodes
            .get(dep)
            .is_some_and(|node| node.kind == NodeKind::Approval)
            || has_approval_ancestor(dep, dependencies, nodes, visited)
    })
}

fn build_waves(dependencies: &BTreeMap<String, Vec<String>>) -> Result<Vec<Vec<String>>, String> {
    let mut indegree: BTreeMap<String, usize> = dependencies
        .iter()
        .map(|(node, deps)| (node.clone(), deps.len()))
        .collect();
    let mut children: BTreeMap<String, Vec<String>> = dependencies
        .keys()
        .map(|node| (node.clone(), Vec::new()))
        .collect();

    for (node, deps) in dependencies {
        for dep in deps {
            children
                .get_mut(dep)
                .expect("dependency node validated")
                .push(node.clone());
        }
    }
    for values in children.values_mut() {
        values.sort();
    }

    let mut ready: BTreeSet<String> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(node, _)| node.clone())
        .collect();

    let mut waves = Vec::new();
    let mut visited = 0usize;

    while !ready.is_empty() {
        let wave: Vec<String> = ready.iter().cloned().collect();
        ready.clear();
        visited += wave.len();

        let mut next = BTreeSet::new();
        for node in &wave {
            if let Some(node_children) = children.get(node) {
                for child in node_children {
                    let degree = indegree
                        .get_mut(child)
                        .expect("child has an indegree entry");
                    *degree -= 1;
                    if *degree == 0 {
                        next.insert(child.clone());
                    }
                }
            }
        }

        waves.push(wave);
        ready = next;
    }

    if visited != dependencies.len() {
        return Err("graph could not be scheduled; a cycle may remain".into());
    }

    Ok(waves)
}

fn graph_fingerprint(spec: &GraphSpec) -> Result<String, String> {
    let bytes = serde_json::to_vec(spec).map_err(|e| format!("serialize graph: {e}"))?;
    // Deterministic FNV-1a fingerprint. This is for stale-plan/change detection,
    // not a cryptographic security boundary.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Ok(format!("fnv1a64:{hash:016x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::schema::{
        Authority, EdgeEndpoint, GraphPolicy, NetworkAuthority, NodeKind, RetryPolicy,
    };

    fn shell_node(id: &str) -> NodeSpec {
        NodeSpec {
            id: id.into(),
            kind: NodeKind::Command,
            artifact_paths: vec![],
            description: None,
            requires: vec!["shell.execute".into()],
            authority: Authority::default(),
            workspace: Default::default(),
            risk: RiskLevel::Reversible,
            retry: RetryPolicy::default(),
            inputs: vec![],
            outputs: vec![],
            executor: Some(ExecutorSpec::Shell {
                command: "echo ok".into(),
                cwd: None,
            }),
        }
    }

    fn graph(nodes: Vec<NodeSpec>, edges: Vec<EdgeSpec>) -> GraphSpec {
        GraphSpec {
            version: 1,
            id: "test".into(),
            goal: None,
            policy: GraphPolicy::default(),
            nodes,
            edges,
        }
    }

    #[test]
    fn valid_graph_compiles_into_waves() {
        let a = shell_node("a");
        let b = shell_node("b");
        let edge = EdgeSpec {
            from: EdgeEndpoint {
                node: "a".into(),
                port: None,
            },
            to: EdgeEndpoint {
                node: "b".into(),
                port: None,
            },
            dependency_only: true,
        };
        let compiled = compile(graph(vec![a, b], vec![edge])).unwrap();
        assert_eq!(
            compiled.waves,
            vec![vec!["a".to_string()], vec!["b".to_string()]]
        );
        assert!(compiled.fingerprint.starts_with("fnv1a64:"));
    }

    #[test]
    fn rejects_typed_edge_mismatch() {
        let mut a = shell_node("a");
        a.outputs.push(PortSpec {
            name: "result".into(),
            data_type: "Patch".into(),
        });
        let mut b = shell_node("b");
        b.inputs.push(PortSpec {
            name: "result".into(),
            data_type: "TestReport".into(),
        });
        let edge = EdgeSpec {
            from: EdgeEndpoint {
                node: "a".into(),
                port: Some("result".into()),
            },
            to: EdgeEndpoint {
                node: "b".into(),
                port: Some("result".into()),
            },
            dependency_only: false,
        };
        let err = compile(graph(vec![a, b], vec![edge])).unwrap_err();
        assert!(err.contains("incompatible types"));
    }

    #[test]
    fn rejects_cycles() {
        let a = shell_node("a");
        let b = shell_node("b");
        let edges = vec![
            EdgeSpec {
                from: EdgeEndpoint {
                    node: "a".into(),
                    port: None,
                },
                to: EdgeEndpoint {
                    node: "b".into(),
                    port: None,
                },
                dependency_only: true,
            },
            EdgeSpec {
                from: EdgeEndpoint {
                    node: "b".into(),
                    port: None,
                },
                to: EdgeEndpoint {
                    node: "a".into(),
                    port: None,
                },
                dependency_only: true,
            },
        ];
        assert!(compile(graph(vec![a, b], edges)).is_err());
    }

    #[test]
    fn irreversible_node_requires_upstream_approval() {
        let mut mutate = shell_node("publish");
        mutate.risk = RiskLevel::Irreversible;
        let err = compile(graph(vec![mutate.clone()], vec![])).unwrap_err();
        assert!(err.contains("upstream approval"));

        let approval = NodeSpec {
            id: "approve".into(),
            kind: NodeKind::Approval,
            artifact_paths: vec![],
            description: None,
            requires: vec!["human.approval".into()],
            authority: Authority::default(),
            workspace: Default::default(),
            risk: RiskLevel::Low,
            retry: RetryPolicy::default(),
            inputs: vec![],
            outputs: vec![],
            executor: None,
        };
        let edge = EdgeSpec {
            from: EdgeEndpoint {
                node: "approve".into(),
                port: None,
            },
            to: EdgeEndpoint {
                node: "publish".into(),
                port: None,
            },
            dependency_only: true,
        };
        assert!(compile(graph(vec![approval, mutate], vec![edge])).is_ok());
    }

    #[test]
    fn executor_capability_must_be_declared() {
        let mut n = shell_node("n");
        n.requires.clear();
        let err = compile(graph(vec![n], vec![])).unwrap_err();
        assert!(err.contains("shell.execute"));

        let provider = NodeSpec {
            id: "inspect".into(),
            kind: NodeKind::Tool,
            artifact_paths: vec![],
            description: None,
            requires: vec![],
            authority: Authority::default(),
            workspace: Default::default(),
            risk: RiskLevel::Low,
            retry: RetryPolicy::default(),
            inputs: vec![],
            outputs: vec![],
            executor: Some(ExecutorSpec::Provider {
                capability: "rea.binary.inspect".into(),
                provider: Some("rea".into()),
                request: serde_json::Value::Null,
            }),
        };
        let err = compile(graph(vec![provider], vec![])).unwrap_err();
        assert!(err.contains("rea.binary.inspect"));
    }

    #[test]
    fn artifact_integration_requires_declared_direct_agent_outputs_and_write_scope() {
        let mut agent = shell_node("agent");
        agent.kind = NodeKind::Agent;
        agent.artifact_paths = vec!["src/api.rs".into()];
        agent.requires = vec!["agent.execute".into()];
        agent.executor = Some(ExecutorSpec::Provider {
            capability: "agent.execute".into(),
            provider: None,
            request: serde_json::json!({"cwd":"/repo","prompt":"implement"}),
        });

        let mut integration = shell_node("integrate");
        integration.kind = NodeKind::Integration;
        integration.requires.clear();
        integration.workspace.mode = super::super::schema::WorkspaceMode::Worktree;
        integration.authority.filesystem_write = vec!["src/**".into()];
        integration.executor = Some(ExecutorSpec::ArtifactCollect {
            sources: vec![super::super::schema::ArtifactSourceSpec {
                node: "agent".into(),
                path: "src/api.rs".into(),
                verified_by: "verify".into(),
                destination: "src/api.rs".into(),
            }],
        });
        let mut verifier = shell_node("verify");
        verifier.kind = NodeKind::Check;
        verifier.workspace.mode = super::super::schema::WorkspaceMode::Worktree;
        let edge = EdgeSpec {
            from: EdgeEndpoint {
                node: "agent".into(),
                port: None,
            },
            to: EdgeEndpoint {
                node: "verify".into(),
                port: None,
            },
            dependency_only: true,
        };
        let integration_edge = EdgeSpec {
            from: EdgeEndpoint {
                node: "agent".into(),
                port: None,
            },
            to: EdgeEndpoint {
                node: "integrate".into(),
                port: None,
            },
            dependency_only: true,
        };
        let verifier_edge = EdgeSpec {
            from: EdgeEndpoint {
                node: "verify".into(),
                port: None,
            },
            to: EdgeEndpoint {
                node: "integrate".into(),
                port: None,
            },
            dependency_only: true,
        };
        let nodes = vec![agent.clone(), verifier.clone(), integration.clone()];
        let edges = vec![
            edge.clone(),
            integration_edge.clone(),
            verifier_edge.clone(),
        ];
        assert!(compile(graph(nodes.clone(), edges.clone())).is_ok());

        let err = compile(graph(nodes.clone(), vec![])).unwrap_err();
        assert!(err.contains("direct agent dependency"));

        let mut undeclared = integration.clone();
        if let Some(ExecutorSpec::ArtifactCollect { sources }) = &mut undeclared.executor {
            sources[0].path = "src/secret.txt".into();
        }
        let err = compile(graph(
            vec![agent.clone(), verifier.clone(), undeclared],
            edges.clone(),
        ))
        .unwrap_err();
        assert!(err.contains("direct agent dependency"));

        let mut out_of_scope = integration;
        out_of_scope.authority.filesystem_write = vec!["docs/**".into()];
        let err = compile(graph(vec![agent, verifier, out_of_scope], edges)).unwrap_err();
        assert!(err.contains("outside declared filesystem_write"));
    }

    #[test]
    fn policy_blocks_network_and_git_push_authority() {
        let mut n = shell_node("n");
        n.authority.network = NetworkAuthority::Allowed;
        assert!(compile(graph(vec![n.clone()], vec![])).is_err());

        n.authority.network = NetworkAuthority::Denied;
        n.authority.git_push = true;
        assert!(compile(graph(vec![n], vec![])).is_err());
    }
}
