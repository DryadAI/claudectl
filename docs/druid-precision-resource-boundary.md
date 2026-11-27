# Precision worker resource boundary — review candidate

Status: code and offline tests only. No worker, model, transient systemd unit,
protected policy, or durable provider was activated by this change.

The headless adapter remains unregistered. In a production build, both the
adapter and the lower-level `run` transport reject a `WorkerConfig` without
host-supplied resource limits. This is not graph input. The core's direct
Bubblewrap path remains only for unit-test builds, not activation. The limits
are bounded to 64 MiB–2 GiB memory, 8–128
tasks, and 1–100% of one CPU. The existing transport still enforces its own
deadline, output cap, turn cap, and CLI budget.
The adapter also rejects graph retry declarations and actual attempt numbers
above the protected standing-policy retry ceiling. The graph supervisor still
owns retry scheduling. The standing-policy loader accepts only
`max_concurrency: 1`; a policy promising more fails closed.

For a configured production attempt, the root supervisor builds a transient
systemd **service**, not a scope or a second scheduler. `systemd-run --pipe`
waits and forwards the worker's stdin/stdout/stderr. The unit sets `MemoryMax`,
`TasksMax`, `CPUQuota`, `RuntimeMaxSec` and `KillMode=control-group` before its
command executes. Stop is bounded by `TimeoutStopSec=3s`. The command is
`setpriv` dropping to the configured worker
UID/GID while clearing supplementary groups and capabilities, then Bubblewrap
with a cleared environment. A unique per-attempt unit name lets cancellation
request `systemctl stop`; the unit runtime limit is a second backstop if the
supervisor dies. This is the intended execution boundary, not a live durable
graph route: durable V1 explicitly rejects `claude_code_headless` agent nodes,
and its agent path uses the existing session supervisor. The non-durable graph
runner now rejects a fake CLI without host resource limits; lower-level core
unit tests still exercise fake transport. Do not infer durable scheduling,
retry or evidence integration from those tests. A separate reviewed bridge must preserve the existing
supervisor's dependencies, retries, approval and evidence authority before
headless registration.

As an additional fail-closed guard, the actuator rechecks a graph agent's
stored provider before any legacy session action. Only the existing
`claude_code` provider (or the canonical default) may use that lane; a future
`claude_code_headless` admission cannot silently fall through to `Spawn`,
`Retry`, or verifier/session-policy actions. This guard does not implement the
new lane or enable the provider.
Likewise, a task with a graph-run identity but a missing execution snapshot
fails closed even if its policy marker is absent; it is never reinterpreted as
a legacy task.

Before `setpriv`, the transient service takes a nonblocking exclusive lock on
`/run/druid-worker-concurrency.lock` via root-owned `/usr/bin/flock`. A second
service exits before worker execution while the slot is held. The lock belongs
to the service process, so a surviving unit keeps it if its supervisor dies.
This is a single host-wide slot, not a distributed scheduler. Live contention
and crash behavior still need independent proof.

This is a candidate, not live isolation proof. Before activation, independently
test on a disposable host/worktree that (1) the worker enters the intended
cgroup before model/tool execution, (2) exact CPU, memory and PID ceilings are
effective, (3) a deliberately detached descendant dies on cancel, timeout and
supervisor crash, (4) failed unit creation never starts a worker, (5) the
effective worker identity has no supplementary groups/capabilities, and (6)
proxy-mode UID egress rules still apply. Verify that a failed `systemctl stop`
cannot leave an indefinitely running unit; inspect the actual unit and cgroup
after each failure. Prove that two simultaneous attempts cannot both enter the
worker and that a surviving unit holds the slot after supervisor failure.
Keep provider registration and the protected standing policy inactive until
this evidence and isolated subscription authentication exist. Retry
recovery/idempotence must be proven with a disposable graph.

Offline evidence: `cargo test -p claudectl-core --lib headless::`,
`cargo test --lib graph::`, targeted Clippy and `git diff --check`. A regression
test confirms durable submission of a headless agent fails atomically rather
than silently changing launch paths. These tests check argv, fail-closed graph
registration and fake core transport behavior; they do not create a real
systemd unit.

References: [systemd-run](https://www.freedesktop.org/software/systemd/man/latest/systemd-run.html),
[systemd.resource-control](https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html),
[systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html).
