use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphSpec {
    pub version: u32,
    pub id: String,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub policy: GraphPolicy,
    pub nodes: Vec<NodeSpec>,
    #[serde(default)]
    pub edges: Vec<EdgeSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphPolicy {
    #[serde(default)]
    pub allow_network: bool,
    #[serde(default)]
    pub allow_git_push: bool,
    #[serde(default = "default_max_attempts_per_node")]
    pub max_attempts_per_node: u32,
}

impl Default for GraphPolicy {
    fn default() -> Self {
        Self {
            allow_network: false,
            allow_git_push: false,
            max_attempts_per_node: default_max_attempts_per_node(),
        }
    }
}

const fn default_max_attempts_per_node() -> u32 {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSpec {
    pub id: String,
    pub kind: NodeKind,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub authority: Authority,
    #[serde(default)]
    pub workspace: WorkspaceSpec,
    #[serde(default)]
    pub risk: RiskLevel,
    #[serde(default)]
    pub retry: RetryPolicy,
    #[serde(default)]
    pub inputs: Vec<PortSpec>,
    #[serde(default)]
    pub outputs: Vec<PortSpec>,
    #[serde(default)]
    pub executor: Option<ExecutorSpec>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Command,
    Check,
    Agent,
    Tool,
    Approval,
    Subgraph,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutorSpec {
    Shell {
        command: String,
        #[serde(default)]
        cwd: Option<String>,
    },
    Provider {
        capability: String,
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        request: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSpec {
    #[serde(default)]
    pub mode: WorkspaceMode,
}

impl Default for WorkspaceSpec {
    fn default() -> Self {
        Self {
            mode: WorkspaceMode::Shared,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    #[default]
    Shared,
    Worktree,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    #[serde(default)]
    pub filesystem_read: Vec<String>,
    #[serde(default)]
    pub filesystem_write: Vec<String>,
    #[serde(default)]
    pub network: NetworkAuthority,
    #[serde(default)]
    pub git_commit: bool,
    #[serde(default)]
    pub git_push: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAuthority {
    #[default]
    Denied,
    Allowed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    #[default]
    Reversible,
    Recoverable,
    Irreversible,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
        }
    }
}

const fn default_max_attempts() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeSpec {
    pub from: EdgeEndpoint,
    pub to: EdgeEndpoint,
    #[serde(default)]
    pub dependency_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeEndpoint {
    pub node: String,
    #[serde(default)]
    pub port: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Outcome {
    Pass,
    Fail,
    Unknown,
    Error,
}
