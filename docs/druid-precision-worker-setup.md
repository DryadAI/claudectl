# Precision bounded DRUID worker

The headless provider is intentionally inactive until this packet is satisfied.
It uses the existing graph supervisor, graph worktree preparation, attempt ledger,
evidence records, verifier gates and approval tables. It does not create a scheduler.

## Host setup packet

Create an unprivileged `druid-worker` account with no sudo, Docker membership,
SSH agent, or readable administrator home. Put the authoritative policy, approval
ledger, revocations and audit log in `/var/lib/claudectl/druid-policy`, owned by
the supervisor identity and unwritable by `druid-worker`. Install a pinned worker
client and `/usr/bin/bwrap` from reviewed packages. The client must be accessible
through approved read-only runtime mounts; never bind the operator home or Docker
socket. Run the Bubblewrap probe as `druid-worker`; it must create a user, PID and
network namespace and must fail to reach a non-loopback destination.

This Precision session fails that probe: Bubblewrap cannot create its NETLINK_ROUTE
socket. Therefore the adapter must stay unregistered here. Do not replace it with
an unsandboxed launcher or `--allow-unsandboxed-shell`.

The supervisor reads the policy and revocation JSON with a protected owner and
no group/other write permissions, validates its expiration/repository/operation
and returns a one-task permit to the adapter. The adapter refuses a graph node
whose task identity does not match that permit. Put the policy, revocations,
approval ledger and audit records outside every worker worktree and outside the
worker-owned home. A worker editing a copied policy file cannot alter the permit.

## Recovery and revocation

On cancellation, timeout or output overflow the transport kills its process group
and returns failure. The durable supervisor keeps the attempt `UNKNOWN` until it
records an outcome; it must not replay an uncertain side effect. Revocation adds
the policy/run identity to the protected revocation ledger, disables provider
registration, and leaves existing evidence and worktrees for review. Retry only
uses a new supervisor attempt after independent verifier failure and the configured
budget permits it. A missing artifact or verifier failure blocks every downstream
node.

## Pilot acceptance

Use a disposable repository and a graph with one worktree agent node, declared
artifact collection, and an independent test verifier. Confirm durable task ID,
base revision, attempt, artifact hash and verifier result. Exercise cancellation,
denied home/credential/network access, an expired policy, revocation, quota
exhaustion and recovery before registering the provider for DRUID work.
