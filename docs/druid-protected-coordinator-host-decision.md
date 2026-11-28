# Protected coordinator host decision packet

Status: **owner approved the proposed identity/path direction and then the
socket/service proof direction in conversation on 2026-11-28; not installed
or activated**. This packet narrows the
identity, path and peer choice required by the source-only protected read code.
It does not authorize live ledger migration, production cutover, broker or
worker activation. Exact service implementation remains unreviewed.

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
| Local socket | root-managed `/run/druid-coordinator/read.sock`; root:`katalyst` 0660 under root:root 0755 directory, approved for non-production proof | operator can connect without direct DB access; peer UID policy still rejects root, worker and coordinator |

The static UID number must be allocated and verified **at installation time**;
do not hardcode an unverified number in source or accept one from a graph,
request, operator profile or environment variable. Pin protected host
configuration under a root-owned non-user-writable path. The operator may
request only the two named status projections through the future service.
This proposal does not grant submission, approval, cancellation, claim,
launch, SQL, path, shell or generic graph mutation authority.

## Decision and proof gates

1. Owner approved the proposed account, root-owned parent and operator-only
   read-peer direction on 2026-11-28, then approved the proposed socket ACL
   and protected service direction for continued proof. On the checked Precision
   snapshot, `katalyst` was the sole passwd user with primary GID 1000 and
   `getent group katalyst` listed no supplementary members; recheck both at
   installation. Root-owned parent and `SO_PEERCRED` are both required: socket
   DAC alone cannot distinguish an allowed UID from a different user sharing
   that group. Any Precision installation must be a separate scoped,
   non-production proof with a reviewed binary and rollback; this source PR
   installs nothing.
2. In a disposable host environment, create the selected layout and prove
   `open_read_only` accepts only the dedicated coordinator identity and the
   exact schema-v9 DB. Prove operator and worker cannot directly open
   DB/WAL/SHM, and that the guarded opener rejects root. Root OS authority
   cannot be removed by file permissions. Prove symlink, hardlink, writable
   ancestor and path-swap denials. Synthetic unit fixtures do not satisfy
   this gate.
3. Build a single protected coordinator service around the existing
   supervisor scheduler. Candidate socket-activation shape: systemd `.socket`
   creates the root-owned parent with `DirectoryMode=0755` and the socket with
   `SocketUser=root`, `SocketGroup=katalyst`, `SocketMode=0660`; the
   `druid-coordinator` service receives the listening FD rather than opening
   the path itself. Do not grant the coordinator service that operator group.
   The source-only one-request helper authenticates kernel peers, applies
   operation-specific policy, sets two-second read/write timeouts and closes
   an accepted connection after one request. A real accept loop still needs
   bounded concurrency, crash/restart and denial-side-effect tests. Candidate
   service controls are `User=druid-coordinator`, `Group=druid-coordinator`,
   no supplementary groups, `NoNewPrivileges=yes`, `ProtectSystem=strict`,
   `ReadWritePaths=/var/lib/druid-coordinator/coord`, `ProtectHome=yes`,
   `PrivateTmp=yes`, `RestrictAddressFamilies=AF_UNIX`,
   `IPAddressDeny=any` and an empty capability bounding set. Verify each
   control against the actual binary on a disposable host before selecting
   a unit; none is installed here.
4. Inventory and migrate **every** direct-ledger caller in the cutover
   contract, keeping the current operator ledger non-broker until complete.
   Capture and audit a private consistent backup and prove rollback before
   any live cutover. Preserve existing data; no in-place destructive move.
5. Seek separate owner authorization for production cutover and any
   headless broker/worker/model activation. A merged PR or passing unit test
   does not lift these gates.

Open: host-managed service implementation/hardening, a non-production host
proof and rollback, and the live-ledger backup destination/cutover procedure.
The approved socket ACL has only passed a disposable namespace DAC probe;
it has not been installed on Precision. Do not substitute a root process
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
schema v8; restored v9 reopened successfully. An additional actual-process
socket probe in the same disposable namespace created a root:operator 0660
socket under a root-owned 0755 directory. The synthetic operator connected;
synthetic worker and coordinator UIDs received DAC denial; namespace root
could connect but `SO_PEERCRED` identified UID 0. A second pass inherited
that socket into the Rust test handler running as the synthetic coordinator
UID: the operator received a bounded `not_found` response, worker and
coordinator still could not connect, and the handler rejected namespace root
by kernel peer identity without returning a status payload. This
does not prove a running DRUID service or actual Precision group membership.
No fixture directory remained
under the proof prefix. The full isolated-profile Rust suite passed
949/965/6/78/16/8; format, diff and Clippy with the existing Hive allowance
passed at source revision `6d14f17` before the socket probe addition. This
is independent process/UID evidence beyond the synthetic unit
fixtures, but **not** a complete actual-host or race proof.

Still unproved: a hostile process sharing the coordinator UID could swap a
path between validation and SQLite open; the dedicated account/service
must exclude such processes or the opener must be strengthened. The service
identity itself is trusted code with ledger write authority under Unix DAC;
the read-only SQLite flag does not make a compromised same-UID process
read-only. Actual
Precision account, path, socket, unit, reboot behavior, open-handle inventory,
backup/rollback and every caller migration remain unverified. Production
cutover, broker, worker and model activation still require separate review.
