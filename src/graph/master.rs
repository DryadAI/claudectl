//! DRUID planning metadata lowered to the existing graph IR. This module does
//! not execute nodes, schedule work, grant authority, or record human approval.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::PathBuf;

use clap::Subcommand;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::compiler::{self, CompiledGraph};
use super::runner::{EvidenceConfidence, RunReport};
use super::schema::{GraphSpec, NodeKind, Outcome, RiskLevel};

const COMPILER_VERSION: &str = "druid-master-v1";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MasterPlan {
    pub version: u32,
    pub revision: u32,
    /// SHA-256 of the preceding compiled plan, required after revision 1.
    pub previous_digest: Option<String>,
    pub source: SourceRevision,
    pub requirements: Vec<Requirement>,
    pub work: Vec<WorkItem>,
    /// The one canonical execution IR, including all authority and policy.
    pub graph: GraphSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceRevision {
    pub repository: String,
    /// Full immutable Git object ID. This is a provenance claim, not a fetch.
    pub commit: String,
    pub references: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub id: String,
    pub description: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkItem {
    pub node: String,
    pub phase: Phase,
    pub requirements: Vec<String>,
    /// Intended artifacts; listing one here is never evidence of its existence.
    pub deliverables: Vec<String>,
    /// Independent downstream check nodes whose observed PASS verifies this item.
    pub verify_with: Vec<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Discovery,
    Inventory,
    Architecture,
    Dependencies,
    Specification,
    Implementation,
    Verification,
    Approval,
    Deployment,
    Recovery,
}

const PHASES: [Phase; 10] = [
    Phase::Discovery,
    Phase::Inventory,
    Phase::Architecture,
    Phase::Dependencies,
    Phase::Specification,
    Phase::Implementation,
    Phase::Verification,
    Phase::Approval,
    Phase::Deployment,
    Phase::Recovery,
];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompiledMasterPlan {
    pub version: u32,
    pub compiler_version: String,
    pub digest: String,
    pub source: MasterPlan,
    pub execution: CompiledGraph,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    Planned,
    Implemented,
    Verified,
}

#[derive(Debug, Serialize)]
pub struct WorkStatus {
    pub phase: Phase,
    pub state: WorkState,
    pub outcome: Outcome,
    pub requirements: Vec<String>,
    pub intended_deliverables: Vec<String>,
    /// JSON pointers within the local report or embedded durable evidence, including verifiers.
    pub evidence: Vec<String>,
    pub blockers: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PlanStatus {
    pub plan_digest: String,
    pub revision: u32,
    pub run_dir: Option<String>,
    pub work: BTreeMap<String, WorkStatus>,
}

fn nonempty(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{label} must not be empty"))
    } else {
        Ok(())
    }
}

fn unique(values: &[String], label: &str) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for value in values {
        nonempty(value, label)?;
        if !seen.insert(value) {
            return Err(format!("duplicate {label}: {value}"));
        }
    }
    Ok(())
}

fn is_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Reachability validation only. Scheduling remains compiler::compile's job.
fn ancestors(node: &str, graph: &CompiledGraph) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut pending = graph.dependencies[node].clone();
    while let Some(id) = pending.pop() {
        if found.insert(id.clone()) {
            pending.extend(graph.dependencies[&id].iter().cloned());
        }
    }
    found
}

pub fn compile(source: MasterPlan) -> Result<CompiledMasterPlan, String> {
    if source.version != 1 || source.revision == 0 {
        return Err("master plan requires version 1 and a positive revision".into());
    }
    match (source.revision, source.previous_digest.as_deref()) {
        (1, None) => {}
        (2.., Some(digest))
            if digest.strip_prefix("sha256:").is_some_and(|v| v.len() == 64 && is_hex(v)) => {}
        _ => return Err("revision 1 must have no previous_digest; later revisions require sha256:<64 lowercase hex>".into()),
    }
    nonempty(&source.source.repository, "source repository")?;
    if !matches!(source.source.commit.len(), 40 | 64) || !is_hex(&source.source.commit) {
        return Err("source commit must be a full lowercase Git object ID".into());
    }
    if source.source.references.is_empty() {
        return Err("source references must not be empty".into());
    }
    unique(&source.source.references, "source reference")?;
    if source.requirements.is_empty() {
        return Err("master plan must contain requirements".into());
    }
    let mut requirements = BTreeSet::new();
    for requirement in &source.requirements {
        nonempty(&requirement.id, "requirement id")?;
        nonempty(&requirement.description, "requirement description")?;
        nonempty(&requirement.source, "requirement source")?;
        if !requirements.insert(requirement.id.clone()) {
            return Err(format!("duplicate requirement '{}'", requirement.id));
        }
    }

    // First validate the unchanged canonical IR, including typed edges, cycles,
    // capability declarations, retries, network/push policy and risk contracts.
    let validated = compiler::compile(source.graph.clone())?;
    let nodes: BTreeMap<_, _> = validated
        .spec
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();
    let mut work = BTreeMap::new();
    let mut covered = BTreeSet::new();
    let mut phases = BTreeSet::new();
    for item in &source.work {
        if !nodes.contains_key(item.node.as_str()) {
            return Err(format!("work references missing node '{}'", item.node));
        }
        if work.insert(item.node.as_str(), item).is_some() {
            return Err(format!("duplicate work node '{}'", item.node));
        }
        phases.insert(item.phase);
        if item.requirements.is_empty() || item.deliverables.is_empty() {
            return Err(format!(
                "node '{}' needs requirements and intended deliverables",
                item.node
            ));
        }
        unique(&item.requirements, "requirement link")?;
        unique(&item.deliverables, "deliverable")?;
        unique(&item.verify_with, "verifier link")?;
        for requirement in &item.requirements {
            if !requirements.contains(requirement) {
                return Err(format!(
                    "node '{}' references missing requirement '{requirement}'",
                    item.node
                ));
            }
            covered.insert(requirement.clone());
        }
    }
    if work.len() != nodes.len() {
        return Err("every graph node must have exactly one work item".into());
    }
    if covered != requirements {
        return Err("every requirement must be linked to work".into());
    }
    for phase in PHASES {
        if !phases.contains(&phase) {
            return Err(format!("master plan is missing phase {phase:?}"));
        }
    }
    for item in &source.work {
        let node = nodes[item.node.as_str()];
        let upstream = ancestors(&item.node, &validated);
        let prerequisite = match item.phase {
            Phase::Discovery => None,
            Phase::Inventory => Some(Phase::Discovery),
            Phase::Architecture => Some(Phase::Inventory),
            Phase::Dependencies => Some(Phase::Architecture),
            Phase::Specification => Some(Phase::Dependencies),
            Phase::Implementation => Some(Phase::Specification),
            Phase::Verification => Some(Phase::Implementation),
            Phase::Approval => Some(Phase::Verification),
            Phase::Deployment | Phase::Recovery => Some(Phase::Approval),
        };
        if let Some(phase) = prerequisite
            && !upstream.iter().any(|id| work[id.as_str()].phase == phase)
        {
            return Err(format!(
                "node '{}' needs upstream phase {phase:?}",
                item.node
            ));
        }
        if item.phase != Phase::Discovery
            && !upstream
                .iter()
                .any(|id| work[id.as_str()].phase == Phase::Discovery)
        {
            return Err(format!("node '{}' needs upstream discovery", item.node));
        }
        if (item.phase == Phase::Approval) != (node.kind == NodeKind::Approval) {
            return Err(format!(
                "node '{}' approval phase and kind must agree",
                item.node
            ));
        }
        if item.phase == Phase::Verification && node.kind != NodeKind::Check {
            return Err(format!("verification node '{}' must be a check", item.node));
        }
        for verifier in &item.verify_with {
            if !work
                .get(verifier.as_str())
                .is_some_and(|w| w.phase == Phase::Verification)
                || !ancestors(verifier, &validated).contains(&item.node)
            {
                return Err(format!(
                    "node '{}' verifier '{verifier}' must be an independent downstream verification check",
                    item.node
                ));
            }
        }
        if item.phase == Phase::Implementation && item.verify_with.is_empty() {
            return Err(format!(
                "implementation node '{}' needs independent verification",
                item.node
            ));
        }
        if item.phase == Phase::Approval
            && !upstream
                .iter()
                .any(|id| work[id.as_str()].phase == Phase::Verification)
        {
            return Err(format!(
                "approval '{}' needs upstream verification",
                item.node
            ));
        }
        if matches!(item.phase, Phase::Deployment | Phase::Recovery)
            || matches!(node.risk, RiskLevel::Recoverable | RiskLevel::Irreversible)
            || node.authority.git_push
        {
            let gated = validated.dependencies[&item.node].iter().any(|id| {
                if nodes[id.as_str()].kind != NodeKind::Approval {
                    return false;
                }
                // A risky implementation can be approved before it executes;
                // its own downstream verifier cannot precede that approval.
                // Deployment/recovery gates, however, require the entire
                // implementation verification set to have completed first.
                if !matches!(item.phase, Phase::Deployment | Phase::Recovery) {
                    return true;
                }
                let before_approval = ancestors(id, &validated);
                source
                    .work
                    .iter()
                    .filter(|w| w.phase == Phase::Implementation)
                    .flat_map(|w| &w.verify_with)
                    .all(|check| before_approval.contains(check))
            });
            if !gated {
                return Err(format!(
                    "risky node '{}' needs a direct human approval dependency after all implementation verifiers",
                    item.node
                ));
            }
        }
    }

    // Hash the fully deserialized source (defaults explicit; object keys sorted
    // by serde_json). Array order remains significant and is documented.
    let value = serde_json::to_value((&COMPILER_VERSION, &source)).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    let digest = format!("sha256:{:x}", Sha256::digest(bytes));
    let mut graph = source.graph.clone();
    // Bind trace metadata into the runtime identity without adding a second IR
    // or changing GraphSpec. Old reports cannot match a changed master plan.
    graph.id = format!("{}@r{}-{}", graph.id, source.revision, &digest[7..]);
    let execution = compiler::compile(graph)?;
    Ok(CompiledMasterPlan {
        version: 1,
        compiler_version: COMPILER_VERSION.into(),
        digest,
        source,
        execution,
    })
}

/// Never trust serialized waves, policy, digest or provenance independently.
pub fn validate_bundle(bundle: &CompiledMasterPlan) -> Result<(), String> {
    let rebuilt = compile(bundle.source.clone())?;
    if serde_json::to_value(bundle).map_err(|e| e.to_string())?
        != serde_json::to_value(rebuilt).map_err(|e| e.to_string())?
    {
        return Err("compiled plan differs from recompilation; stale or modified bundle".into());
    }
    Ok(())
}

fn pointer(node: &str) -> String {
    format!(
        "/nodes/{}/evidence",
        node.replace('~', "~0").replace('/', "~1")
    )
}

pub fn status(
    bundle: &CompiledMasterPlan,
    report: Option<&RunReport>,
) -> Result<PlanStatus, String> {
    validate_bundle(bundle)?;
    let graph = &bundle.execution;
    let mut observed = BTreeSet::new();
    if let Some(report) = report {
        if report.graph_id != graph.spec.id || report.graph_fingerprint != graph.fingerprint {
            return Err("runtime report belongs to a different plan/graph revision".into());
        }
        if report.nodes.keys().collect::<Vec<_>>() != graph.dependencies.keys().collect::<Vec<_>>()
        {
            return Err("runtime report must contain exactly the compiled graph's nodes".into());
        }
        for (id, execution) in &report.nodes {
            let node = graph
                .spec
                .nodes
                .iter()
                .find(|n| &n.id == id)
                .expect("validated nodes");
            if execution.evidence.is_empty() {
                return Err(format!("node '{id}' has no runtime evidence"));
            }
            let last = execution.evidence.last().expect("nonempty evidence");
            if last.outcome != execution.outcome
                || execution.attempts > node.retry.max_attempts
                || execution
                    .evidence
                    .iter()
                    .any(|record| record.node_id != *id)
            {
                return Err(format!("node '{id}' has inconsistent runtime evidence"));
            }
            if execution.attempts == 0 {
                if execution.evidence.len() != 1
                    || last.attempt != 0
                    || last.outcome != Outcome::Unknown
                {
                    return Err(format!("node '{id}' has invalid blocked evidence"));
                }
            } else if execution.evidence.len() != execution.attempts as usize
                || execution
                    .evidence
                    .iter()
                    .enumerate()
                    .any(|(i, record)| record.attempt as usize != i + 1)
            {
                return Err(format!("node '{id}' has inconsistent attempt evidence"));
            }
            if execution.attempts > 0
                && last.outcome == Outcome::Pass
                && last.confidence == EvidenceConfidence::Observed
            {
                if serde_json::to_value(&last.declared_authority).map_err(|e| e.to_string())?
                    != serde_json::to_value(&node.authority).map_err(|e| e.to_string())?
                {
                    return Err(format!("node '{id}' evidence authority differs from plan"));
                }
                if let Some(super::schema::ExecutorSpec::Shell { command, .. }) = &node.executor
                    && (last.command.as_ref() != Some(command) || last.exit_code != Some(0))
                {
                    return Err(format!(
                        "node '{id}' shell PASS lacks matching command and zero exit evidence"
                    ));
                }
                observed.insert(id.clone());
            }
        }
        // A report cannot claim that a node ran past a failed prerequisite.
        for (id, execution) in &report.nodes {
            if execution.attempts > 0
                && graph.dependencies[id]
                    .iter()
                    .any(|dep| report.nodes[dep].outcome != Outcome::Pass)
            {
                return Err(format!("node '{id}' ran despite a non-passing dependency"));
            }
        }
    }
    let outcomes = report
        .map(|r| {
            r.nodes
                .iter()
                .map(|(id, n)| (id.clone(), n.outcome))
                .collect()
        })
        .unwrap_or_default();
    let links = report
        .map(|r| {
            r.nodes
                .keys()
                .map(|id| (id.clone(), vec![pointer(id)]))
                .collect()
        })
        .unwrap_or_default();
    Ok(project_status(
        bundle,
        &observed,
        &outcomes,
        &links,
        report.map(|r| r.run_dir.clone()),
    ))
}

fn project_status(
    bundle: &CompiledMasterPlan,
    observed: &BTreeSet<String>,
    outcomes: &BTreeMap<String, Outcome>,
    links: &BTreeMap<String, Vec<String>>,
    run_dir: Option<String>,
) -> PlanStatus {
    let graph = &bundle.execution;
    let mut work = BTreeMap::new();
    for item in &bundle.source.work {
        let mut evidence = Vec::new();
        evidence.extend(links.get(&item.node).into_iter().flatten().cloned());
        evidence.extend(
            item.verify_with
                .iter()
                .filter_map(|id| links.get(id))
                .flatten()
                .cloned(),
        );
        let implemented = observed.contains(&item.node);
        let verified = implemented
            && !item.verify_with.is_empty()
            && item.verify_with.iter().all(|id| observed.contains(id));
        let mut blockers: Vec<_> = graph.dependencies[&item.node]
            .iter()
            .filter(|id| !observed.contains(*id))
            .cloned()
            .collect();
        blockers.extend(
            item.verify_with
                .iter()
                .filter(|id| !observed.contains(*id))
                .cloned(),
        );
        blockers.sort();
        blockers.dedup();
        work.insert(
            item.node.clone(),
            WorkStatus {
                phase: item.phase,
                state: if verified {
                    WorkState::Verified
                } else if implemented {
                    WorkState::Implemented
                } else {
                    WorkState::Planned
                },
                outcome: outcomes
                    .get(&item.node)
                    .copied()
                    .unwrap_or(Outcome::Unknown),
                requirements: item.requirements.clone(),
                intended_deliverables: item.deliverables.clone(),
                evidence,
                blockers,
            },
        );
    }
    PlanStatus {
        plan_digest: bundle.digest.clone(),
        revision: bundle.source.revision,
        run_dir,
        work,
    }
}

#[cfg(feature = "coord")]
#[derive(Debug, Serialize)]
pub struct DurablePlanStatus {
    #[serde(flatten)]
    pub plan: PlanStatus,
    /// Embedded evidence makes every work.evidence JSON pointer independently resolvable.
    pub durable: super::durable::DurableRunStatus,
}

#[cfg(feature = "coord")]
pub(crate) fn status_from_durable(
    conn: &mut rusqlite::Connection,
    bundle: &CompiledMasterPlan,
    run: &str,
) -> Result<DurablePlanStatus, String> {
    validate_bundle(bundle)?;
    // One read transaction prevents joining old task state to newer evidence.
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let digest = super::mixed::validate_run_graph(&tx, run, &bundle.execution)?;
    let durable = super::durable::status_with_connection(&tx, run)?;
    if durable.graph_id != bundle.execution.spec.id
        || durable.fingerprint != bundle.execution.fingerprint
    {
        return Err("durable run metadata differs from compiled graph".into());
    }
    let mut observed = BTreeSet::new();
    let mut outcomes = BTreeMap::new();
    let mut links = BTreeMap::new();
    for (index, result) in durable.nodes.iter().enumerate() {
        let node = bundle
            .execution
            .spec
            .nodes
            .iter()
            .find(|n| n.id == result.node_id)
            .ok_or("unknown durable node")?;
        outcomes.insert(node.id.clone(), result.outcome);
        let mut node_links = Vec::new();
        if !result.evidence.is_empty() {
            node_links.push(format!("/durable/nodes/{index}/evidence"));
        }
        if result.approval.is_some() {
            node_links.push(format!("/durable/nodes/{index}/approval"));
        }
        if result.latest_verification.is_some() {
            node_links.push(format!("/durable/nodes/{index}/latest_verification"));
        }
        links.insert(node.id.clone(), node_links);
        match node.kind {
            NodeKind::Agent => {
                // Session completion and attached verifier summaries are not an
                // independent observation that the planned deliverable exists.
                continue;
            }
            NodeKind::Approval => {
                if let Some(decision) = &result.approval
                    && (decision.digest != digest
                        || decision.actor.trim().is_empty()
                        || decision.reason.trim().is_empty())
                {
                    return Err("durable approval does not match the run snapshot".into());
                }
                if result.state == "DONE" {
                    if !result.approval.as_ref().is_some_and(|d| d.approved) {
                        return Err("completed approval lacks an approving decision".into());
                    }
                    observed.insert(node.id.clone());
                }
                continue;
            }
            _ => {}
        }
        if result.attempts > node.retry.max_attempts
            || result.evidence.len() > result.attempts as usize
        {
            return Err(format!(
                "node '{}' has inconsistent durable attempt counts",
                node.id
            ));
        }
        for (i, record) in result.evidence.iter().enumerate() {
            if record.node_id != node.id
                || record.attempt as usize != i + 1
                || serde_json::to_value(&record.declared_authority).map_err(|e| e.to_string())?
                    != serde_json::to_value(&node.authority).map_err(|e| e.to_string())?
            {
                return Err(format!(
                    "node '{}' has inconsistent durable evidence",
                    node.id
                ));
            }
        }
        if result.state != "DONE" {
            continue;
        }
        let last = result
            .evidence
            .last()
            .ok_or("completed durable node lacks evidence")?;
        if result.evidence.len() != result.attempts as usize || last.outcome != Outcome::Pass {
            return Err(format!(
                "node '{}' completion lacks matching successful evidence",
                node.id
            ));
        }
        if last.confidence != EvidenceConfidence::Observed {
            continue;
        }
        if let Some(super::schema::ExecutorSpec::Shell { command, .. }) = &node.executor
            && (last.command.as_ref() != Some(command) || last.exit_code != Some(0))
        {
            return Err(format!(
                "node '{}' shell PASS lacks matching command and zero exit evidence",
                node.id
            ));
        }
        observed.insert(node.id.clone());
    }
    // Late cancellation can invalidate a prerequisite after execution. Preserve
    // historical evidence, but never promote its descendants in the current view.
    observed.retain(|id| {
        ancestors(id, &bundle.execution)
            .iter()
            .all(|dep| outcomes.get(dep) == Some(&Outcome::Pass))
    });
    let plan = project_status(bundle, &observed, &outcomes, &links, None);
    tx.commit().map_err(|e| e.to_string())?;
    Ok(DurablePlanStatus { plan, durable })
}

#[derive(Debug, Subcommand)]
pub enum MasterCommand {
    /// Validate lifecycle coverage, traceability, gates and canonical graph contracts.
    Validate { path: PathBuf },
    /// Emit plan.json, graph.json and initially planned status.json in a NEW directory.
    Compile {
        path: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Project runtime evidence onto work states; never executes or grants approval.
    Status {
        path: PathBuf,
        #[arg(long)]
        report: Option<PathBuf>,
        /// Read one exact durable run without executing, approving, or migrating it.
        #[cfg(feature = "coord")]
        #[arg(long, conflicts_with = "report")]
        run: Option<String>,
    },
    /// Print the machine-readable input or compiled-bundle schema.
    Schema {
        #[arg(long)]
        compiled: bool,
    },
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> io::Result<T> {
    serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)
}

fn json<T: Serialize>(value: &T) -> io::Result<String> {
    serde_json::to_string_pretty(value).map_err(io::Error::other)
}

pub fn dispatch(command: &MasterCommand) -> io::Result<()> {
    match command {
        MasterCommand::Validate { path } => {
            let bundle = compile(read_json(path)?).map_err(io::Error::other)?;
            println!(
                "{}",
                json(
                    &serde_json::json!({"status": "valid", "digest": bundle.digest, "waves": bundle.execution.waves})
                )?
            );
        }
        MasterCommand::Compile { path, out } => {
            let bundle = compile(read_json(path)?).map_err(io::Error::other)?;
            let initial = status(&bundle, None).map_err(io::Error::other)?;
            let plan = json(&bundle)?;
            let graph = json(&bundle.execution.spec)?;
            let initial = json(&initial)?;
            // Refuse overwrite. A fresh directory keeps revisions and evidence
            // separate; no path comes from untrusted node/artifact identifiers.
            fs::create_dir(out)?;
            fs::write(out.join("plan.json"), plan)?;
            fs::write(out.join("graph.json"), graph)?;
            fs::write(out.join("status.json"), initial)?;
            println!(
                "{}",
                json(&serde_json::json!({"digest": bundle.digest, "output": out}))?
            );
        }
        MasterCommand::Status {
            path,
            report,
            #[cfg(feature = "coord")]
            run,
        } => {
            let bundle: CompiledMasterPlan = read_json(path)?;
            #[cfg(feature = "coord")]
            if let Some(run) = run {
                let mut conn = crate::coord::store::open_read_only().map_err(io::Error::other)?;
                println!(
                    "{}",
                    json(&status_from_durable(&mut conn, &bundle, run).map_err(io::Error::other)?)?
                );
                return Ok(());
            }
            let report: Option<RunReport> = report.as_ref().map(read_json).transpose()?;
            println!(
                "{}",
                json(&status(&bundle, report.as_ref()).map_err(io::Error::other)?)?
            );
        }
        MasterCommand::Schema { compiled } => {
            let schema = if *compiled {
                schemars::schema_for!(CompiledMasterPlan)
            } else {
                schemars::schema_for!(MasterPlan)
            };
            println!("{}", json(&schema)?);
        }
    }
    Ok(())
}
