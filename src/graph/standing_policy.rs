//! Protected, machine-readable authority for a bounded development worker.
//!
//! This is intentionally a supervisor-side contract.  A graph and a worker
//! never supply this object: the supervisor reads it from a directory which is
//! not writable by the worker identity, verifies its mode and checks the
//! protected revocation ledger before constructing a `WorkerConfig`.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StandingPolicy {
    pub policy_id: String,
    pub status: PolicyStatus,
    pub expires_at_unix_ms: u128,
    pub repositories: Vec<RepositoryGrant>,
    pub limits: Limits,
    pub fresh_human_approval_required: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyStatus {
    Active,
    InactiveUntilHostSandboxProbePasses,
    Revoked,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryGrant {
    pub remote: String,
    pub operations: BTreeSet<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub timeout_seconds: u64,
    pub max_output_bytes: usize,
    pub max_turns: u32,
    pub max_budget_usd: f64,
    pub max_concurrency: u32,
    pub max_retries: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Revocations {
    #[serde(default)]
    pub policy_ids: BTreeSet<String>,
    #[serde(default)]
    pub task_ids: BTreeSet<String>,
}

/// A permit is short-lived, bound to one task and only constructible after all
/// supervisor-owned checks have passed.  It must be retained with the durable
/// task/attempt evidence, never written in the worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyPermit {
    pub policy_id: String,
    pub task_id: String,
    pub repository_remote: String,
}

pub fn authorize(
    policy_path: &Path,
    revocations_path: &Path,
    required_owner_uid: u32,
    now_unix_ms: u128,
    task_id: &str,
    repository_remote: &str,
    operation: &str,
) -> Result<(StandingPolicy, PolicyPermit), String> {
    protect(policy_path, required_owner_uid)?;
    protect(revocations_path, required_owner_uid)?;
    let policy: StandingPolicy = serde_json::from_slice(
        &fs::read(policy_path).map_err(|_| "read protected standing policy failed")?,
    )
    .map_err(|_| "invalid protected standing policy")?;
    let revocations: Revocations = serde_json::from_slice(
        &fs::read(revocations_path).map_err(|_| "read protected revocation ledger failed")?,
    )
    .map_err(|_| "invalid protected revocation ledger")?;
    if policy.status != PolicyStatus::Active || now_unix_ms >= policy.expires_at_unix_ms {
        return Err("standing policy is inactive or expired".into());
    }
    if revocations.policy_ids.contains(&policy.policy_id) || revocations.task_ids.contains(task_id)
    {
        return Err("standing policy or task is revoked".into());
    }
    let grant = policy
        .repositories
        .iter()
        .find(|grant| grant.remote == repository_remote)
        .ok_or("repository is not authorized by standing policy")?;
    if !grant.operations.contains(operation)
        || policy
            .fresh_human_approval_required
            .iter()
            .any(|op| op == operation)
    {
        return Err("operation requires fresh human approval or is not granted".into());
    }
    if policy.limits.timeout_seconds == 0
        || policy.limits.max_output_bytes == 0
        || policy.limits.max_turns == 0
        || policy.limits.max_concurrency == 0
        || policy.limits.max_budget_usd <= 0.0
        || !policy.limits.max_budget_usd.is_finite()
    {
        return Err("invalid standing-policy limits".into());
    }
    Ok((
        policy.clone(),
        PolicyPermit {
            policy_id: policy.policy_id,
            task_id: task_id.into(),
            repository_remote: repository_remote.into(),
        },
    ))
}

fn protect(path: &Path, owner_uid: u32) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("protected authority path must be absolute".into());
    }
    let metadata = fs::metadata(path).map_err(|_| "protected authority metadata unavailable")?;
    if metadata.uid() != owner_uid || metadata.mode() & 0o022 != 0 {
        return Err("authority state is worker-writable or wrong-owner".into());
    }
    let parent = path.parent().ok_or("authority path lacks parent")?;
    let parent_meta = fs::metadata(parent).map_err(|_| "protected authority parent unavailable")?;
    if parent_meta.uid() != owner_uid || parent_meta.mode() & 0o022 != 0 {
        return Err("authority directory is worker-writable or wrong-owner".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn policy() -> StandingPolicy {
        StandingPolicy {
            policy_id: "p1".into(),
            status: PolicyStatus::Active,
            expires_at_unix_ms: 100,
            repositories: vec![RepositoryGrant {
                remote: "https://github.com/DryadAI/claudectl".into(),
                operations: ["edit".into(), "test".into()].into_iter().collect(),
            }],
            limits: Limits {
                timeout_seconds: 1,
                max_output_bytes: 1,
                max_turns: 1,
                max_budget_usd: 1.0,
                max_concurrency: 1,
                max_retries: 0,
            },
            fresh_human_approval_required: vec!["merge".into(), "deploy".into()],
        }
    }
    fn write_authority(
        dir: &Path,
        policy: &StandingPolicy,
        revocations: &Revocations,
    ) -> (PathBuf, PathBuf) {
        let p = dir.join("policy.json");
        let r = dir.join("revocations.json");
        fs::write(&p, serde_json::to_vec(policy).unwrap()).unwrap();
        fs::write(&r, serde_json::to_vec(revocations).unwrap()).unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&r, fs::Permissions::from_mode(0o600)).unwrap();
        (p, r)
    }
    #[test]
    fn authorizes_only_exact_active_unrevoked_repository_operation() {
        let d = tempdir().unwrap();
        let (p, r) = write_authority(d.path(), &policy(), &Revocations::default());
        let uid = fs::metadata(&p).unwrap().uid();
        let (_, permit) = authorize(
            &p,
            &r,
            uid,
            99,
            "task-1",
            "https://github.com/DryadAI/claudectl",
            "edit",
        )
        .unwrap();
        assert_eq!(permit.task_id, "task-1");
        assert!(
            authorize(
                &p,
                &r,
                uid,
                99,
                "task-1",
                "https://github.com/DryadAI/other",
                "edit"
            )
            .is_err()
        );
        assert!(
            authorize(
                &p,
                &r,
                uid,
                100,
                "task-1",
                "https://github.com/DryadAI/claudectl",
                "edit"
            )
            .is_err()
        );
        assert!(
            authorize(
                &p,
                &r,
                uid,
                99,
                "task-1",
                "https://github.com/DryadAI/claudectl",
                "merge"
            )
            .is_err()
        );
    }
    #[test]
    fn revocation_and_worker_writable_authority_fail_closed() {
        let d = tempdir().unwrap();
        let (p, r) = write_authority(
            d.path(),
            &policy(),
            &Revocations {
                policy_ids: ["p1".into()].into_iter().collect(),
                task_ids: BTreeSet::new(),
            },
        );
        let uid = fs::metadata(&p).unwrap().uid();
        assert!(
            authorize(
                &p,
                &r,
                uid,
                99,
                "task-1",
                "https://github.com/DryadAI/claudectl",
                "edit"
            )
            .is_err()
        );
        fs::write(&r, b"{\"policy_ids\":[],\"task_ids\":[]}").unwrap();
        fs::set_permissions(&r, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(
            authorize(
                &p,
                &r,
                uid,
                99,
                "task-1",
                "https://github.com/DryadAI/claudectl",
                "edit"
            )
            .is_err()
        );
    }
}
