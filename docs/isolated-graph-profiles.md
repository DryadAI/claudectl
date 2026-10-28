# Isolated graph execution profiles

An opt-in Linux execution profile separates development graph state from the
operator's normal Claude sessions. It uses the existing supervisor, not a second
scheduler. This is application-state isolation **not an OS security sandbox**.
Do not give untrusted shell checks access to secrets or production infrastructure.

## Setup

Use a stable full-featured claudectl binary and a current Claude Code executable
on PATH. Choose a new absolute directory whose parent already exists:

```sh
/path/to/claudectl profile init /private/path/druid-run
/path/to/claudectl profile check /private/path/druid-run
CLAUDE_CONFIG_DIR=/private/path/druid-run/.claude claude auth login
```

Initialization refuses existing paths. It creates private directories, pins the
claudectl executable's path and SHA-256, and installs profile-only authority and
lifecycle hooks using that absolute binary. No login data is copied, no HOME
override is made, and shared Claude settings are not edited. Login must be
performed by the operator. Rebuilding/replacing the binary invalidates the pin;
create a fresh profile rather than silently accepting a changed executable.

Commands select a profile explicitly:

```sh
CLAUDECTL_PROFILE=/private/path/druid-run /path/to/claudectl graph submit graph.json --allow-unsandboxed-shell
CLAUDECTL_PROFILE=/private/path/druid-run /path/to/claudectl --headless --json
CLAUDECTL_PROFILE=/private/path/druid-run /path/to/claudectl graph status RUN_ID
```

Only graph commands, hook ingestion, and the headless supervisor are supported
while a profile is active. The supervisor uses default configuration, ignoring
shared/project rules, automation hooks, brain settings and other CLI overrides.
Its task ledger, bus ledger, policies, worktrees, evidence, session history and
Claude transcript discovery resolve inside the profile. Normal commands without
CLAUDECTL_PROFILE retain their existing behavior.

## Launch and authority

The launch adapter starts a fresh noninteractive Claude process with a new UUID.
It registers that process for existing session discovery and writes stdout/stderr
under `logs/`. It never reuses an existing session or opens a shared terminal.
The child has a minimal environment (no inherited API keys or provider settings),
profile-specific CLAUDE_CONFIG_DIR, user-only settings sources, empty strict MCP
configuration and ordinary permissions. Structured file tools are allowed through
Claude's permission layer; the graph hook independently limits their paths.
Interactive input/resume is unsupported; the runtime must retry as a new attempt.
This adapter currently requires Linux `/proc/sys/kernel/random/uuid`.

Profile tool calls fail closed on missing/malformed authority and invalid session
identity. Missing/changed profile settings or executable pins fail startup. The
authority shell wrapper converts executable failure to blocking exit code 2.
This does not defend against another process with the same OS user's privileges,
and it does not sandbox shell verifier nodes. Platform-managed Claude settings,
network egress, CLI compatibility and operator identity still require review.

## Evidence required before development execution

`profile check` verifies local configuration and binary identity only. Automated
tests invoke the real hook entry point with synthetic payloads: scoped write
allowed, outside write/Bash/network denied, absent authority denied, settings
tampering rejected and telemetry confined to the selected profile. These are
**not** evidence that a live Claude session ran its hooks.

After separate login, run a disposable graph pilot: one permitted file edit,
attempted outside-file edit and shell use, then independent checks of the actual
files and hook events. Confirm SessionStart/PostToolUse/Stop delivery and session
discovery, including fast exits. Only then submit development graphs. Re-generate
plans from their intended clean source revisions. Keep human review gates waiting;
no production deployment is authorized by creating or testing a profile.

Keep profile logs private: they can contain prompts and agent output. Stop its
supervisor and child processes before archiving the profile. Do not remove shared
Claude state or reset global settings during cleanup.
