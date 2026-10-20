use std::path::Path;

use serde_json::Value;

use super::schema::{NodeSpec, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Public adapter vocabulary; the embedded binary currently uses only a subset.
#[allow(dead_code)]
pub enum ProviderConfidence {
    Observed,
    Derived,
    Inferred,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ProviderResult {
    pub outcome: Outcome,
    pub confidence: ProviderConfidence,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub message: Option<String>,
    pub artifacts: Vec<String>,
}

impl ProviderResult {
    // Convenience API for external providers and test adapters.
    #[allow(dead_code)]
    pub fn pass_observed(message: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Pass,
            confidence: ProviderConfidence::Observed,
            stdout: None,
            stderr: None,
            message: Some(message.into()),
            artifacts: Vec::new(),
        }
    }

    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Unknown,
            confidence: ProviderConfidence::Unknown,
            stdout: None,
            stderr: None,
            message: Some(message.into()),
            artifacts: Vec::new(),
        }
    }
}

pub struct ProviderContext<'a> {
    pub run_dir: &'a Path,
    pub attempt: u32,
}

/// Runtime adapter for an external capability provider.
///
/// Graphs name capabilities, not implementations. A provider may be REA,
/// Claude Code, the local brain, the agent bus, or a future implementation.
/// Providers are deliberately runtime-only and never appear in the canonical
/// graph type system beyond an optional preferred provider name.
pub trait Provider {
    fn name(&self) -> &str;
    fn supports(&self, capability: &str) -> bool;
    fn execute(
        &self,
        node: &NodeSpec,
        capability: &str,
        request: &Value,
        context: &ProviderContext<'_>,
    ) -> Result<ProviderResult, String>;
}

#[derive(Default)]
pub struct ProviderRegistry<'a> {
    providers: Vec<&'a dyn Provider>,
}

impl<'a> ProviderRegistry<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: &'a dyn Provider) {
        self.providers.push(provider);
    }

    pub fn resolve(
        &self,
        required: &[String],
        preferred: Option<&str>,
    ) -> Option<&'a dyn Provider> {
        self.providers.iter().copied().find(|provider| {
            preferred.is_none_or(|name| provider.name() == name)
                && required
                    .iter()
                    .all(|capability| provider.supports(capability))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::schema::{Authority, ExecutorSpec, NodeKind, RetryPolicy, RiskLevel};

    struct Stub;

    impl Provider for Stub {
        fn name(&self) -> &str {
            "stub"
        }

        fn supports(&self, capability: &str) -> bool {
            capability == "example.inspect"
        }

        fn execute(
            &self,
            _node: &NodeSpec,
            _capability: &str,
            _request: &Value,
            _context: &ProviderContext<'_>,
        ) -> Result<ProviderResult, String> {
            Ok(ProviderResult::pass_observed("ok"))
        }
    }

    #[test]
    fn registry_requires_all_declared_capabilities() {
        let stub = Stub;
        let mut registry = ProviderRegistry::new();
        registry.register(&stub);

        assert!(
            registry
                .resolve(&["example.inspect".into()], Some("stub"))
                .is_some()
        );
        assert!(
            registry
                .resolve(
                    &["example.inspect".into(), "network.observe".into()],
                    Some("stub")
                )
                .is_none()
        );
        assert!(
            registry
                .resolve(&["example.inspect".into()], Some("other"))
                .is_none()
        );
    }

    #[test]
    fn provider_request_remains_runtime_data() {
        let node = NodeSpec {
            id: "inspect".into(),
            kind: NodeKind::Tool,
            description: None,
            requires: vec!["example.inspect".into()],
            authority: Authority::default(),
            workspace: Default::default(),
            risk: RiskLevel::Low,
            retry: RetryPolicy::default(),
            inputs: vec![],
            outputs: vec![],
            executor: Some(ExecutorSpec::Provider {
                capability: "example.inspect".into(),
                provider: Some("stub".into()),
                request: serde_json::json!({"target": "/tmp/example"}),
            }),
        };
        let request = match &node.executor {
            Some(ExecutorSpec::Provider { request, .. }) => request,
            _ => panic!("provider executor"),
        };
        assert_eq!(request["target"], "/tmp/example");
    }
}
