# DRUID durable mixed graphs (V1)

PR #3 extends the existing supervisor bridge from PR #1 and the master compiler
from PR #2. There is still one durable scheduler: `Supervisor::tick`. Command,
check, provider, agent, and approval nodes all use the same task IDs, dependency
ledger, attempts and graph-run state. The canonical JSON IR is unchanged.

## Supported execution

| Node | Execution | Boundary |
| --- | --- | --- |
| Agent | Existing Claude Code supervisor, worktree and verifier path | PR #1 authority restrictions remain |
| Command/check/tool with shell | Existing local runner executor | Explicit `--allow-unsandboxed-shell` required |
| Command/check/tool with provider | Existing provider registry (currently REA) | Missing capability/provider yields UNKNOWN |
| Approval | Persisted human decision | Exact run/node/digest; no daemon stdin prompt |
| Subgraph or typed artifact edge | Rejected at submission | Not silently interpreted as executable transport |

Non-agent execution uses shared workspaces. Shell working directories and agent
source directories are resolved to absolute paths at submission. Provider
filesystem scopes and `request.target`, when present, must already be absolute.
Generic provider requests remain adapter-defined; there is no universal path
rewriter. Agent worktrees are independent: a downstream shared check does **not**
automatically inspect an upstream agent's worktree. Use the agent's own verifiers
for that; artifact transport and worktree integration remain unsupported.

## Submission and approval

Review the commands and provider requests before submission. Submission queues
work in the existing supervisor database; an already-running daemon can start it
immediately. Do not submit an example to a production daemon just to validate it.

```sh
# Validation only: no submission or execution.
claudectl graph validate examples/graph-durable-mixed-smoke.json
claudectl graph plan examples/graph-durable-mixed-smoke.json

# Only in a deliberately selected test environment, after reviewing the plan:
claudectl graph submit examples/graph-durable-mixed-smoke.json --allow-unsandboxed-shell
claudectl graph inspect RUN_ID
claudectl --json graph status RUN_ID

# Only when NODE_ID is AWAITING_APPROVAL, with the exact digest shown above:
claudectl graph decide RUN_ID NODE_ID --digest FULL_SHA256 --decision approve \
  --actor operator-name --reason 'Reviewed this run and its recorded evidence'
# Use --decision reject to close the gate without releasing its dependents.
```

Every new run stores a versioned execution snapshot and SHA-256 digest in the
same transaction as its tasks. The snapshot includes the canonical compilation,
resolved execution nodes, node-to-task mapping and shell opt-in. Identical plans
submitted twice have distinct run-bound approval digests. `graph inspect` prints
the saved snapshot, not a potentially edited source file. The original canonical
fingerprint remains available for graph identity; it is not the approval digest.

Approval requires a waiting node, completed dependencies, exact run digest,
nonempty actor and reason. Early, cross-run, wrong-node and duplicate decisions
are rejected. The decision, transition and parent-run update commit atomically.
`AWAITING_APPROVAL` is not failure: downstream tasks wait without being escalated.
Rejection becomes NEEDS_HUMAN/FAIL and blocks descendants. Generic task
approval/retry controls cannot override these graph gates or turn a failed
non-agent execution into success. Start a newly reviewed run to reconsider a
rejected or uncertain operation.

An approval is a local operator assertion, **not authentication or a signature**.
The digest binds the stored execution plan and run, not repository contents,
external artifacts, changing infrastructure or an independently attested human
identity. Anyone controlling the daemon account or database remains trusted.
Compiler validation and master-plan status never grant execution authority.

## Evidence, retries and restart behavior

Before invoking an executor, the actuator claims the task and inserts an attempt
in one immediate SQLite transaction. It rechecks dependencies and the expected
attempt number. Duplicate or delayed actions cannot run the same attempt or
consume a later retry. Concurrent connections can claim only one attempt.

Results use the existing runner's evidence format: outcome, confidence, command,
exit code, bounded recorded output, timing, authority declaration and artifact
links. The evidence row, attempt completion and task transition commit together.
`graph status --json` (or the global `--json` form above) includes these records
and approval decisions. Provider artifacts live under
`~/.claudectl/coord/graph-evidence/ATTEMPT_ID/`; the SQLite ledger owns the result
record. Artifact links are not content attestations and may outlive their files.

- PASS completes the task and can release dependents.
- Recorded FAIL/ERROR retries only within the declared attempt budget.
- UNKNOWN stops in NEEDS_HUMAN without automatic retry.
- A crash after claim but before result commit leaves RUNNING/UNKNOWN. Restart
  does **not** replay it, including when the command may have completed externally.
  Inspect the side effects and reconcile manually before submitting a new run.
- Cancellation preserves late evidence but never changes CANCELLED to DONE.
  It does not terminate an already-running shell/provider process.
- Completed nodes and waiting approval gates survive database reopen.

This is conservative at-most-one dispatch per attempt, **not exactly-once external
effects**. Commands/providers execute synchronously in the actuator; a hung call
can stall daemon reconciliation. There is no new timeout or process-termination
contract. Use short, bounded operations in a dedicated test daemon.

PR #2's `graph master status --report` still consumes local-runner reports. Durable
status is a separate execution view, not yet a master-plan report exporter. In
particular, agent DONE is not automatically proof of independent verification;
inspect its recorded verifier evidence. No production verification is inferred.

## Authority and upgrade safety

Default submission rejects shell nodes. The explicit shell opt-in acknowledges
that shell commands run with **all daemon account permissions**, including access
not declared in graph authority. Declarations are not an OS sandbox. The opt-in
does not grant general Bash access to agent sessions, relax their tool allowlist,
or remove worktree isolation. Providers must enforce their own authority contract.
Recoverable and irreversible nodes additionally require an upstream approval.
Risk labels are declarations; the compiler cannot infer arbitrary command effects.
No deployment or cluster rollout is performed by this increment's validation.

Schema v7 adds snapshots, node evidence, approval decisions and the
AWAITING_APPROVAL task state. Existing agent-only ledger rows remain readable.
The version check now runs **before** migration and rejects future schemas without
restamping them. PR #2 and older binaries checked too late: **stop all old daemons
and CLI writers before upgrading, and do not point those binaries at a v7 DB**.
Back up the ledger using the established SQLite backup procedure before upgrade;
rollback requires restoring that backup, not starting an older writer on v7.

## Validation scope

Unit tests use temporary directories and in-memory/file-backed SQLite. They cover
mixed scheduling, exact approval binding, rejection, generic-action bypasses,
atomic submission/result rollback, two-connection claims, stale retry actions,
restart uncertainty, cancellation, capability absence, provider errors and bounded
retries. Real short shell commands are exercised; provider behavior is mocked.
Agent routing is simulated, not a real Claude session. The smoke example is
validated but not submitted. See the [validation record](validation/druid-durable-mixed-v1.md).
