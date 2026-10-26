# DRUID Master Graph Compiler & Planning System (V1)

DRUID master plans are versioned planning documents over the canonical graph IR
introduced in PR #1. A master plan adds requirements, source provenance, lifecycle
phases, intended deliverables and independent verification links. Compilation
emits an ordinary `GraphSpec` consumed by `graph validate|plan|run`.

The master compiler introduces no scheduler or executor. The graph compiler owns
dependency validation and execution waves; the local runner owns local execution
and JSONL evidence. PR #3 adds [durable mixed execution](druid-durable-mixed-graphs.md)
through the existing supervisor, including a persistent approval ledger. Compiled
master `graph.json` files can use that path subject to its submission restrictions
and explicit shell opt-in. Durable evidence is not yet exported to master status.

## Quick start

From the repository root, after building `claudectl`:

```sh
claudectl graph master validate examples/druid-master-plan-v1.json
claudectl graph master compile examples/druid-master-plan-v1.json --out /tmp/druid-plan-r1
claudectl graph validate /tmp/druid-plan-r1/graph.json
claudectl graph plan /tmp/druid-plan-r1/graph.json
claudectl graph master status /tmp/druid-plan-r1/plan.json
```

The output directory must not exist. Compilation writes:

| File | Purpose |
| --- | --- |
| `plan.json` | Source plan, SHA-256 digest, compiler version, compiled graph and dependency waves |
| `graph.json` | Executable canonical V1 graph, with a digest-bound graph ID |
| `status.json` | Initial projection: all tasks planned, all outcomes UNKNOWN |

Compilation performs validation and serialization only. It neither runs commands
nor contacts repositories or grants approval. If output fails partway through,
the partial directory remains for inspection; use a new output directory.

The example inventories the repository, checks architecture/schema/implementation
files and runs the planning acceptance suite. Its deployment and recovery nodes
name **unconfigured** `druid_operator` capabilities. No deployment or recovery
provider is shipped. Intended runbook paths in those requests are placeholders,
not claims that approved runbooks exist. To exercise the actual checks while
declining the approval gate:

```sh
claudectl --json graph run /tmp/druid-plan-r1/graph.json < /dev/null
# Expected nonzero exit: approval FAIL, deployment/recovery UNKNOWN.
# Use run_dir from the JSON output for the next command:
claudectl graph master status /tmp/druid-plan-r1/plan.json \
  --report <run_dir>/summary.json
```

The separate integration-test fixture uses harmless `printf` checks in a temporary
directory and real runner evidence. Passing that fixture does not prove that any
Dryad infrastructure has been deployed or recovered.

## Contract

The authored document is strict JSON: unknown fields, including a user-supplied
`state: verified`, are rejected. See the generated input and compiled-bundle
schemas in `schemas/`. Export them with:

```sh
claudectl graph master schema
claudectl graph master schema --compiled
```

Schemas describe structural types. The Rust compiler additionally enforces the
cross-reference, lifecycle, version, digest, policy and DAG rules below. Schema
validation alone is not sufficient to accept a plan.

- `version` is 1; `revision` is positive. Revision 1 has no `previous_digest`.
  Later revisions require the preceding plan's `sha256:<64 lowercase hex>` digest.
  The link is provenance supplied by the author: V1 has no global revision ledger
  and does not prove predecessor existence or prevent forks/reused revisions.
- `source` records repository identity, a full Git commit ID and references to
  decisions/specifications. These references are recorded, not fetched. Confirm
  the checkout and any local changes before executing; compilation does not
  attest that the working tree matches `source.commit`.
- Requirements have stable unique IDs, descriptions and source links. Every
  requirement must map to work. Every graph node must map to exactly one work
  item with requirements and intended deliverables. Artifact names are labels;
  they are never treated as filesystem paths to open or write by the compiler.
- All ten phases must be represented. Each item needs an upstream prerequisite:
  discovery → inventory → architecture → dependencies → specification →
  implementation → verification → approval. Deployment and recovery each follow
  approval. Additional dependency branches are allowed. Recovery is an explicit
  gated task, not an automatic failure handler: the existing runner only advances
  after PASS. A recovery after a failed deployment needs a separately reviewed
  plan; the example deliberately remains blocked after failure.
- Verification nodes must be `check` nodes. `verify_with` references independent
  downstream verification nodes. Implementation items require at least one such
  check. Approval phase and approval node kind must agree.
- Deployment/recovery, recoverable/irreversible risk and Git-push authority need
  a **direct** human approval dependency. Deployment/recovery gates must follow
  every declared implementation verifier in the master plan. This conservative
  V1 release gate covers the whole plan; staged releases can use separate master
  plans. Intermediate risky work can have an earlier approval after preliminary
  verification; its own verification must still follow its execution.
- The existing compiler enforces duplicate/missing nodes, cycles, typed ports,
  capabilities, retry bounds, and network/Git-push policy. No authority field is
  widened or inferred during lowering.

## Identity and traceability

The plan digest is SHA-256 over JSON serialization of the compiler version and
the fully deserialized source. Defaults become explicit; object keys are sorted
by `serde_json`; array order remains significant. This is this compiler's
versioned encoding, not a claim of RFC 8785 canonicalization. The graph ID becomes
`<authored-id>@r<revision>-<full-plan-sha256>`. Thus a change to requirements,
artifacts, source commit, policy, authority or executors changes the runtime
identity even when the node topology is unchanged.

The output graph retains the original nodes, edges and authority. The existing
runtime also computes its FNV fingerprint. Both identity and fingerprint must
match when importing evidence. `master status` recompiles the bundle source and
compares the entire generated bundle, rejecting changed waves, digests, policy
or other inconsistent compiled content.

SHA-256 supplies content identity, **not an approval signature**. Reports and
source revision claims are operator-supplied, unsigned data. A malicious author
can fabricate a report; the projector is not an attestation or authorization
service. Preserve the generated bundle and original run directory together for
review. Status inspection never resumes work or grants runtime approval.

## Planned, implemented and verified

These are projections over one supplied local-runtime report, not persisted
scheduler states. With no report everything is planned. Import requires the
exact node set, consistent outcome/attempt records, matching node IDs and no
execution past a failed prerequisite. An observed shell PASS also needs matching
command, zero exit status and declared authority.

| State | Required evidence |
| --- | --- |
| `planned` | No successful observed execution for this item (including failures, blocked nodes and inferred results) |
| `implemented` | Final attempted execution has observed PASS |
| `verified` | Item has observed PASS and every explicitly linked independent verifier has observed PASS |

Items without `verify_with` can reach implemented, never verified. In particular,
a check passing does not independently verify itself. Intended deliverables do
not become evidence merely by being listed. Runtime `PASS/FAIL/UNKNOWN/ERROR`
remains a separate field so failures are visible even when state remains planned.

Each item includes JSON pointers into the supplied report's evidence arrays,
including its verifier evidence, and the report's `run_dir`. Those records contain
commands, output, timestamps, authority and provider artifact links. The status
command does not dereference arbitrary artifact locations, authenticate their
contents, combine evidence from multiple runs, or silently promote inferred
results. It lists unmet prerequisite/verifier IDs for follow-up.

## Authority and execution limits

The local runner executes shell nodes with caller permissions; its authority
declarations are not a shell sandbox. Review commands before running untrusted
plans. A compiler-valid plan is not permission to deploy. The durable path retains
its agent structured-tool allowlist, denied agent Bash access and per-node worktree
rules from PR #1. Non-agent shell nodes require the explicit unsandboxed opt-in
described in the durable mixed-graph documentation.

The compiler checks declared risk and topology; it does not infer the effects of
arbitrary shell commands. Providers must enforce their own authority contracts.
Human approval uses the local runtime prompt or durable digest-bound decision,
never a field in the plan or a result of compilation. Missing provider capability remains UNKNOWN and blocks
dependents. Typed edges remain compile-time contracts without runtime artifact
transport. Worktree integration, process termination/timeouts,
signed approvals, automated deployment and automatic recovery remain future work.

## Validation

See the [recorded local validation results](validation/druid-master-compiler-v1.md)
for the build matrix, test totals and actual example-run evidence.

`tests/master_plan.rs` checks determinism, identity invalidation, lifecycle and
traceability gaps, authority preservation, approval bypass attempts, cycles,
malformed reports, bundle tampering, schema drift, refusal to overwrite output,
and an actual CLI → canonical runner → evidence projection round trip. It runs
with both default and `--no-default-features --features hive` builds.

Run the repository's standard gates:

```sh
cargo fmt --all -- --check
cargo check --all-targets
cargo check --all-targets --no-default-features --features hive
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features --features hive -- -D warnings
cargo test --all-targets
cargo test --all-targets --no-default-features --features hive
```

The schema drift test compares checked-in schemas with schemas derived from the
Rust types, so schema changes must be deliberate and reviewed with the compiler.
