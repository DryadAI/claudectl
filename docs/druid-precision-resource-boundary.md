# Precision worker resource boundary — review candidate

Status: code and offline tests only. No worker, model, transient systemd unit,
protected policy, or durable provider was activated by this change.

The headless adapter remains unregistered. In a production build, it rejects a
`WorkerConfig` without host-supplied resource limits. This is not graph input.
The core's direct Bubblewrap path remains for disposable transport tests; it is
not an activation path. The limits are bounded to 64 MiB–2 GiB memory, 8–128
tasks, and 1–100% of one CPU. The existing transport still enforces its own
deadline, output cap, turn cap, and CLI budget.
The adapter also rejects graph retry declarations and actual attempt numbers
above the protected standing-policy retry ceiling. The graph supervisor still
owns retry scheduling; cross-process concurrency is not yet enforced.

For a configured production attempt, the root supervisor builds a transient
systemd **service**, not a scope or a second scheduler. `systemd-run --pipe`
waits and forwards the worker's stdin/stdout/stderr. The unit sets `MemoryMax`,
`TasksMax`, `CPUQuota`, `RuntimeMaxSec` and `KillMode=control-group` before its
command executes. Stop is bounded by `TimeoutStopSec=3s`. The command is
`setpriv` dropping to the configured worker
UID/GID while clearing supplementary groups and capabilities, then Bubblewrap
with a cleared environment. A unique per-attempt unit name lets cancellation
request `systemctl stop`; the unit runtime limit is a second backstop if the
supervisor dies. The existing graph supervisor retains dependencies, retries,
approval and evidence authority.

This is a candidate, not live isolation proof. Before activation, independently
test on a disposable host/worktree that (1) the worker enters the intended
cgroup before model/tool execution, (2) exact CPU, memory and PID ceilings are
effective, (3) a deliberately detached descendant dies on cancel, timeout and
supervisor crash, (4) failed unit creation never starts a worker, (5) the
effective worker identity has no supplementary groups/capabilities, and (6)
proxy-mode UID egress rules still apply. Verify that a failed `systemctl stop`
cannot leave an indefinitely running unit; inspect the actual unit and cgroup
after each failure. Keep provider registration and the protected standing
policy inactive until this evidence and isolated subscription authentication
exist. Concurrency enforcement remains a separate supervisor gate; retry
recovery/idempotence must be proven with a disposable graph.

Offline evidence: `cargo test -p claudectl-core --lib headless::`,
`cargo test --lib graph::`, targeted Clippy and `git diff --check`. These tests
check argv and fake transport behavior; they do not create a real systemd unit.

References: [systemd-run](https://www.freedesktop.org/software/systemd/man/latest/systemd-run.html),
[systemd.resource-control](https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html),
[systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html).
