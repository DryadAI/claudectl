# DRUID headless Claude Code worker — development adapter

Status: implemented bounded Unix transport and an explicitly injected graph
provider; **not enabled in the CLI or durable supervisor, not deployed**.
OpenHands is unchanged. No real Claude inference or credentials are used by tests.

## Existing runtime, not another scheduler

DRUID already accepts `agent.execute` and executes durable agent tasks through
terminal/profile launchers. This adapter reuses `Provider`, `ProviderRegistry`
and the graph runner. Its core transport executes exactly one attempt; it owns
neither a task queue nor retries. Scheduling, approvals and verification stay in
DRUID. The current durable path still rejects provider `claude_code_headless`;
do not remove that rejection before provisioning the missing trust boundary.

The graph request contains only `cwd` and `prompt`. Unknown fields reject, and
cwd must match the canonical host-configured workspace. Executable, credentials,
deadline, output cap, turns and spend ceiling come from trusted Rust configuration,
not graph/model fields. The provider currently accepts only agent/worktree nodes
with whole-workspace read/write declarations and no Git or network-tool authority.
It does not implement finer file scopes; it rejects them rather than widening them.

## Invocation and output contract

The transport invokes an absolute binary without a shell. Prompt bytes travel on
stdin, not argv. Fixed flags request `--bare -p`, JSON, no session persistence,
Read/Edit/Write tools only, `dontAsk`, max turns and max budget. No Bash, arbitrary
extra flags, automatic resume, plugin/MCP discovery or permission bypass is exposed.
Only explicitly provided HOME/PATH/TMPDIR/API key/base URL variables are accepted;
the process inherits no environment. These values must point to a dedicated
worker context, never a user's administrator home. No real credentials are copied.

Nonblocking pipe reads/writes bound the entire attempt, including blocked stdin.
Combined stdout/stderr is capped at at most 1 MiB. stderr is discarded after
counting, never added to an error. Timeout/cancellation/output overflow terminate
the process group and reap the direct child; no partial result becomes success.
The CLI turn/spend limits are additional controls, not a hard financial guarantee.

A successful exit alone is insufficient: the final JSON must identify a successful
result with no permission denials, a bounded session ID, textual output and a
finite nonnegative reported cost within budget. Missing/duplicate known fields,
malformed JSON and error results fail closed. Unknown metadata fields are ignored.
Graph evidence contains session ID, reported cost and a result-text hash, not model
prose. Completion is `Derived`, not independent verification. The output hash is
not proof of truth and does not preserve the output itself; candidate files need
separate verifier/artifact collection before acceptance.

## Security boundaries that this code does NOT provide

- A cwd/worktree is not a filesystem sandbox. Read/Edit/Write tools need external
  filesystem confinement. Tool permission flags are defense in depth only.
- Process groups are not containment: a hostile detached descendant or a killed
  supervisor requires an external cgroup/container lifecycle controller.
- Bare mode deliberately skips ambient hooks, including the current DRUID
  PreToolUse hook. This adapter must NOT silently replace that hooked launcher.
- No authenticated approval receipt, OS identities, resource-memory limits,
  executable pin verification, protected storage, network policy or recovery
  bridge has been provisioned by this PR.
- Model transport requires an explicitly approved inference route. Denied graph
  network-tool authority is not proof of network isolation or local inference.
  Bifrost compatibility and cloud fallback policy remain unverified.

## Compatibility and activation gates

The current [headless documentation](https://code.claude.com/docs/en/headless)
and [CLI reference](https://code.claude.com/docs/en/cli-reference) describe these
flags. Pin and test the actual installed version before activation. Bare mode
uses API/provider credentials rather than subscription OAuth; do not assume a
Claude subscription funds this backend. Authentication/billing and the approved
Bifrost route require an operator decision and a bounded smoke test.

Before production registration: establish separate unprivileged worker identity,
protected authority and sandbox; bind exact artifacts and workspace to approved
run/node/attempt; wire supervisor cancellation and durable result evidence;
independently verify candidates; prove denied access and crash recovery. Only
then run an approved one-task smoke test. Retire OpenHands only after dependency
review, preservation of required state and separate removal approval.

## Test evidence

Run `cargo test --offline -p claudectl-core headless --lib` for transport tests
using disposable fake executables, including timeout/cancellation/output caps.
Run `cargo test --offline --lib graph::headless` for graph/provider integration
and unregistered-provider refusal. These tests do not prove live OS isolation,
provider billing, real Claude compatibility or production readiness.

Development validation on 2026-11-01, based on merged main `22cd20b`:

- Core library: 132 tests passed, eight consecutive complete runs after fixing
  temporary-script fixture contention (13 of these are new transport tests).
- Graph regression suite: 62 tests passed, including the new adapter integration.
- `cargo clippy --offline --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

Initial repeated core runs exposed Linux ETXTBSY (errno 26) during concurrent
temporary executable setup. Fixture lifetimes are now serialized; no production
retry or permissive fallback was added. The full workspace test suite, real
model inference, isolated UID/network tests and live durable activation were not
run. Dependencies resolved from the existing local Cargo cache; this is not a
dependency supply-chain audit.
