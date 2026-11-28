# Protected coordinator host decision packet

Status: **owner approved the proposed design in conversation on 2026-11-28;
not installed or activated**. This packet narrows the
identity, path and peer choice required by the source-only protected read code.
It does not authorize a live ledger migration, service, broker or worker.

## Read-only Precision observations

At 2026-11-28T13:01Z on `Precision`, `getent passwd druid-coordinator`
and `getent group druid-coordinator` found no entry. `/`, `/var` and
`/var/lib` were root-owned with modes 0555, 0755 and 0755 respectively.
`/var/lib/druid-coordinator` was absent. No unit file matching
`druid-coordinator*` was listed and neither `/run/druid-coordinator` nor
`/run/druid-coordinator/read.sock` was present. These checks cover only the
proposed names and paths; they are
**not** an inventory of every existing coordinator process or socket.
No coordinator DB, snapshot, secret or running service was opened.

## Owner-approved design, subject to host proof

| Boundary | Proposed value | Why |
| --- | --- | --- |
| Service identity | static system user/group `druid-coordinator`, no login shell, no supplementary groups | dedicated UID; never share it with a worker, operator CLI or unrelated process |
| Protected parent | `/var/lib/druid-coordinator`, root:root 0755 | root-owned, non-writable ancestors satisfy the guarded opener |
| Ledger directory | `/var/lib/druid-coordinator/coord`, coordinator-owned 0700 | operator and worker cannot traverse it |
| Ledger and WAL/SHM | `coord.db` and any sidecars, coordinator-owned 0600, regular single-link files | matches the no-follow private-file checks |
| Read peer: task state | existing `katalyst` UID, resolved at service start from protected host configuration | one operator status reader; no write or approval authority |
| Read peer: graph state | same `katalyst` UID initially | avoids another account until a distinct reader is justified |
| Denied read peers | root, `druid-worker`, `druid-coordinator`, all other UIDs | source policy rejects root/coordinator; host config must exclude worker |
| Local socket | root-managed `/run/druid-coordinator/read.sock`, exact mode/ownership to be designed with the service | do not give operator direct DB access; no listener exists yet |

The static UID number must be allocated and verified **at installation time**;
do not hardcode an unverified number in source or accept one from a graph,
request, operator profile or environment variable. Pin protected host
configuration under a root-owned non-user-writable path. The operator may
request only the two named status projections through the future service.
This proposal does not grant submission, approval, cancellation, claim,
launch, SQL, path, shell or generic graph mutation authority.

## Decision and proof gates

1. Owner approved the proposed account, root-owned parent and operator-only
   read-peer direction on 2026-11-28. The exact socket mode/group and service
   hardening are still open. Account creation, permissions and systemd changes
   are **not** covered by this source PR or that design approval.
2. In a disposable host environment, create the selected layout and prove
   `open_read_only` accepts only the dedicated coordinator identity and the
   exact schema-v9 DB. Prove operator and worker cannot directly open
   DB/WAL/SHM, and that the guarded opener rejects root. Root OS authority
   cannot be removed by file permissions. Prove symlink, hardlink, writable
   ancestor and path-swap denials. Synthetic unit fixtures do not satisfy
   this gate.
3. Build a single protected coordinator service around the existing
   supervisor scheduler. The read socket must authenticate kernel peers,
   apply the operation-specific policy and close after one request. Test
   timeouts, resource limits, crash/restart and denial side effects.
4. Inventory and migrate **every** direct-ledger caller in the cutover
   contract, keeping the current operator ledger non-broker until complete.
   Capture and audit a private consistent backup and prove rollback before
   any live cutover. Preserve existing data; no in-place destructive move.
5. Seek separate owner authorization for production cutover and any
   headless broker/worker/model activation. A merged PR or passing unit test
   does not lift these gates.

Open: exact Unix socket mode/group, host-managed service hardening, backup
destination and rollback procedure. These require a service design and
disposable-host proof before selection. Do not substitute a root process
opening the operator-profile ledger.

## Disposable namespace evidence (source-only)

`bash scripts/verify_druid_protected_namespace.sh` creates a fresh tmpfs
chroot inside new Linux user, mount and network namespaces. Namespace UID 0
maps to the invoking unprivileged host user; UIDs 1000/1001/1002 map into
that user's subordinate range. These are **synthetic roles**, not selected
Precision account numbers. No host `/var/lib` path or operator ledger is
mounted into the chroot. The Rust test binary and `/usr` are bind-mounted
read-only; the tmpfs disappears when the namespace exits.

On 2026-11-28 the script passed: the real schema migration created a fresh
v9 ledger; SQLite integrity and version checks passed; only the synthetic
coordinator UID could use the guarded opener and query missing task/graph
state. Synthetic operator and worker UIDs got permission-denied on the DB,
both sidecars and a rename attempt. Namespace root could read through DAC
but the guarded opener rejected root. The opener rejected world-readable
WAL, DB symlink/hardlink, writable root-owned ancestor, SHM symlink and
schema v8; restored v9 reopened successfully. No fixture directory remained
under the proof prefix. The full isolated-profile Rust suite passed
947/963/6/78/16/8; format, diff and Clippy with the existing Hive allowance
passed. This is independent process/UID evidence beyond the synthetic unit
fixtures, but **not** a complete actual-host or race proof.

Still unproved: a hostile process sharing the coordinator UID could swap a
path between validation and SQLite open; the dedicated account/service
must exclude such processes or the opener must be strengthened. Actual
Precision account, path, socket, unit, reboot behavior, open-handle inventory,
backup/rollback and every caller migration remain unverified. Production
cutover, broker, worker and model activation still require separate review.
