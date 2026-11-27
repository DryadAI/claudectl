# Proposed protected coordinator read contract

Status: **source-only protocol; no socket, service, CLI client or running
endpoint**. This is the first narrow slice of the whole-ledger cutover
outlined in `druid-protected-coordinator-cutover.md`, not an activation step.

`coord::protected_read` accepts one length-prefixed JSON request from a Linux
Unix peer. The two-byte big-endian length is limited to 256 bytes. The parser
checks `SO_PEERCRED` against a caller-supplied UID **before reading**; a real
service must pin that UID in protected host configuration, not graph JSON,
an environment variable, or operator-controlled files. It sets a two-second
read timeout and rejects unknown fields, versions, operations and malformed
IDs. Example request (illustrative only; no socket exists):

```json
{"version":1,"operation":"task_state","id":"task_123_456"}
```

The only staged operations are `task_state` and `graph_state`. There is no
submission, approval, SQL, file-path, command, claim or launch operation.
The response type allows only state and graph identity/digest fields, not
prompts, policies, worktree paths, evidence payloads or credentials. The
current dispatcher **always rejects**, so the response type is a design
constraint rather than a claim that status can already be queried.

`druid-protected-coordinator-store.md` describes a later source-only guarded
opener and projection implementation. It does not make this wire dispatcher
or a service active.

Before a service can use this protocol, it needs a host-proven protected ledger
opener and projection (the source-only versions are not host proof),
exact per-operation peer authorization,
bounded response framing, one-request-per-connection closure, resource limits
and tests against a disposable protected identity/storage layout. Writes and
the existing supervisor scheduler require separate design and review. The
root broker must not use this read contract to obtain a permit or open the
operator-profile DB. No host deployment is authorized by these source tests.
