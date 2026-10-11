use super::schema::{NodeSpec, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

impl ProviderResult {
    pub fn pass_observed(message: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Pass,
            confidence: ProviderConfidence::Observed,
            stdout: None,
            stderr: None,
            message: Some(message.into()),
        }
    }

    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Unknown,
            confidence: ProviderConfidence::Unknown,
            stdout: None,
            stderr: None,
            message: Some(message.into()),
        }
    }
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
    fn execute(&self, node: &NodeSpec, capability: &str) -> Result<ProviderResult, String>;
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
                && required.iter().all(|capability| provider.supports(capability))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub;

    impl Provider for Stub {
        fn name(&self) -> &str {
            "stub"
        }

        fn supports(&self, capability: &str) -> bool {
            capability == "example.inspect"
        }

        fn execute(&self, _node: &NodeSpec, _capability: &str) -> Result<ProviderResult, String> {
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
}
