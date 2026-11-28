#!/usr/bin/env bash
# Disposable Linux namespace proof only. Never points at an operator ledger.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
for command_name in cargo jq rg unshare mount chroot sqlite3; do
  command -v "$command_name" >/dev/null || {
    printf 'BLOCKED: missing %s\n' "$command_name" >&2
    exit 1
  }
done

outer_uid=$(id -u)
outer_gid=$(id -g)
if (( outer_uid == 0 || outer_gid == 0 )); then
  printf 'BLOCKED: run as an unprivileged user with subordinate IDs\n' >&2
  exit 1
fi
account=$(id -un)
read -r subuid_start subuid_count < <(awk -F: -v user="$account" '$1 == user { print $2, $3; exit }' /etc/subuid)
read -r subgid_start subgid_count < <(awk -F: -v user="$account" '$1 == user { print $2, $3; exit }' /etc/subgid)
for number in "$subuid_start" "$subuid_count" "$subgid_start" "$subgid_count"; do
  [[ "$number" =~ ^[0-9]+$ ]] || {
    printf 'BLOCKED: subordinate UID/GID mapping unavailable\n' >&2
    exit 1
  }
done
if (( subuid_count < 1003 || subgid_count < 1003 )); then
  printf 'BLOCKED: subordinate UID/GID range too small\n' >&2
  exit 1
fi

test_binary=$(cargo test --lib --no-run --message-format=json |
  jq -r 'select(.reason == "compiler-artifact" and .profile.test == true and .executable != null) | .executable' |
  tail -n 1)
if [[ ! -x "$test_binary" ]]; then
  printf 'BLOCKED: compiled Rust test binary unavailable\n' >&2
  exit 1
fi
if ! "$test_binary" --list | rg -q '^coord::protected_store::tests::disposable_namespace_probe: test$'; then
  printf 'BLOCKED: disposable Rust proof test was not compiled\n' >&2
  exit 1
fi
proof_dir=$(mktemp -d /tmp/druid-protected-proof.XXXXXXXX)
case "$proof_dir" in
  /tmp/druid-protected-proof.*) ;;
  *) printf 'BLOCKED: unsafe temporary path\n' >&2; exit 1 ;;
esac
trap 'rmdir -- "$proof_dir"' EXIT

unshare --user --mount --net --propagation private \
  --map-users "0:$outer_uid:1" --map-users "1:$subuid_start:$subuid_count" \
  --map-groups "0:$outer_gid:1" --map-groups "1:$subgid_start:$subgid_count" \
  --setuid 0 --setgid 0 \
  bash -s -- "$proof_dir" "$(dirname "$test_binary")" "$(basename "$test_binary")" <<'PROOF'
set -euo pipefail
root=$1
binary_dir=$2
binary_name=$3

# Every fixture is on a tmpfs visible only in this mount namespace. The
# namespace has no network and no host path to the operator-profile ledger.
mount -t tmpfs -o size=128m,mode=0755 tmpfs "$root"
trap 'umount "$root/probe" 2>/dev/null || true; umount "$root/usr" 2>/dev/null || true; umount "$root" 2>/dev/null || true' EXIT
mkdir -p "$root/usr" "$root/probe" "$root/tmp/seed-profile" \
  "$root/var/lib/druid-coordinator/coord"
chmod 1777 "$root/tmp"
chmod 0700 "$root/tmp/seed-profile" "$root/var/lib/druid-coordinator/coord"
chown 1001:1001 "$root/tmp/seed-profile" "$root/var/lib/druid-coordinator/coord"
ln -s usr/lib "$root/lib"
ln -s usr/lib "$root/lib64"
mount --bind /usr "$root/usr"
mount --bind "$binary_dir" "$root/probe"
mount -o remount,bind,ro "$root/usr"
mount -o remount,bind,ro "$root/probe"

run_case() {
  local mode=$1 uid=$2 label=${3:-$1}
  printf 'CASE %s (namespace UID %s)\n' "$label" "$uid"
  DRUID_PROTECTED_PROOF_MODE="$mode" CLAUDECTL_PROFILE=/tmp/seed-profile \
    chroot --groups= --userspec="$uid:$uid" "$root" "/probe/$binary_name" \
      --exact coord::protected_store::tests::disposable_namespace_probe --nocapture
}

run_case seed 1001
ledger="$root/var/lib/druid-coordinator/coord/coord.db"
seed="$root/tmp/seed-profile/.claudectl/coord/coord.db"
test -f "$seed"
cp -- "$seed" "$ledger"
chown 1001:1001 "$ledger"
chmod 0600 "$ledger"
check=$(chroot --groups= --userspec=1001:1001 "$root" /usr/bin/sqlite3 \
  -batch -noheader /var/lib/druid-coordinator/coord/coord.db \
  'PRAGMA integrity_check; PRAGMA user_version;')
if [[ "$check" != $'ok\n9' ]]; then
  printf 'BLOCKED: disposable ledger integrity/schema check failed\n' >&2
  exit 1
fi
printf 'PASS: disposable SQLite integrity and exact schema version 9\n'
run_case coordinator 1001

# Present private sidecars so the negative tests cannot pass on ENOENT.
for suffix in -wal -shm; do
  touch "$ledger$suffix"
  chown 1001:1001 "$ledger$suffix"
  chmod 0600 "$ledger$suffix"
  test -f "$ledger$suffix"
done
run_case operator 1000
run_case worker 1002
run_case root 0

chmod 0644 "$ledger-wal"
run_case reject 1001 "reject world-readable WAL"
chmod 0600 "$ledger-wal"

mv -- "$ledger" "$ledger.real"
ln -s coord.db.real "$ledger"
run_case reject 1001 "reject database symlink"
rm -- "$ledger"
mv -- "$ledger.real" "$ledger"

ln -- "$ledger" "$ledger.hardlink"
run_case reject 1001 "reject database hardlink"
rm -- "$ledger.hardlink"

chmod 0777 "$root/var/lib/druid-coordinator"
run_case reject 1001 "reject writable root-owned ancestor"
chmod 0755 "$root/var/lib/druid-coordinator"

rm -- "$ledger-shm"
ln -s coord.db "$ledger-shm"
run_case reject 1001 "reject SHM symlink"
rm -- "$ledger-shm" "$ledger-wal"

chroot --groups= --userspec=1001:1001 "$root" /usr/bin/sqlite3 \
  -batch /var/lib/druid-coordinator/coord/coord.db \
  'PRAGMA journal_mode=DELETE; PRAGMA user_version=8;' >/dev/null
run_case reject_schema 1001 "reject schema version 8"
chroot --groups= --userspec=1001:1001 "$root" /usr/bin/sqlite3 \
  -batch /var/lib/druid-coordinator/coord/coord.db \
  'PRAGMA user_version=9;'
run_case coordinator 1001
printf 'PASS: disposable namespace identity, private storage and guarded opener probes\n'
printf 'NOT PROVEN: actual Precision service, same-UID path-swap race, reboot or live cutover\n'
PROOF
