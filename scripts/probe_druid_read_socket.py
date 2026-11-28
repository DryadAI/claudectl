#!/usr/bin/env python3
"""Disposable namespace-only proof of the proposed local read socket DAC.

Run only from verify_druid_protected_namespace.sh inside its tmpfs chroot.
The numeric UIDs/GIDs are synthetic namespace roles, not Precision choices.
"""

import os
import json
import socket
import stat
import struct
import subprocess
import sys


SOCKET_PATH = "/run/druid-coordinator/read.sock"
OPERATOR_UID = 1000
COORDINATOR_UID = 1001
WORKER_UID = 1002


def try_peer(listener: socket.socket, uid: int, should_connect: bool) -> None:
    read_fd, write_fd = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(read_fd)
        listener.close()
        os.setgroups([])
        os.setgid(uid)
        os.setuid(uid)
        peer = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            peer.connect(SOCKET_PATH)
            os.write(write_fd, b"connected")
        except PermissionError:
            os.write(write_fd, b"denied")
        finally:
            peer.close()
            os.close(write_fd)
        os._exit(0)

    os.close(write_fd)
    try:
        if should_connect:
            accepted, _ = listener.accept()
            with accepted:
                credentials = accepted.getsockopt(
                    socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")
                )
                _pid, peer_uid, _gid = struct.unpack("3i", credentials)
                assert peer_uid == uid, (peer_uid, uid)
                # DAC permits namespace root, but the application policy must
                # still reject it. Only the operator is a staged read peer.
                assert (peer_uid == OPERATOR_UID) == (uid == OPERATOR_UID)
        result = os.read(read_fd, 32)
        assert result == (b"connected" if should_connect else b"denied"), (
            uid,
            result,
        )
        _waited, status = os.waitpid(pid, 0)
        assert os.waitstatus_to_exitcode(status) == 0, (uid, status)
    finally:
        os.close(read_fd)


def request_as(uid: int, expected: bytes) -> None:
    read_fd, write_fd = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(read_fd)
        os.setgroups([])
        os.setgid(uid)
        os.setuid(uid)
        peer = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        peer.settimeout(3)
        try:
            try:
                peer.connect(SOCKET_PATH)
            except PermissionError:
                result = b"dac_denied"
            else:
                request = b'{"version":1,"operation":"task_state","id":"task_missing"}'
                peer.sendall(len(request).to_bytes(2, "big") + request)
                try:
                    header = peer.recv(2)
                    if header:
                        length = int.from_bytes(header, "big")
                        body = bytearray()
                        while len(body) < length:
                            chunk = peer.recv(length - len(body))
                            assert chunk, "truncated response"
                            body.extend(chunk)
                        assert json.loads(body)["kind"] == "not_found"
                        result = b"not_found"
                    else:
                        result = b"policy_denied"
                except ConnectionResetError:
                    # Rejection happens before request bytes are consumed.
                    result = b"policy_denied"
            os.write(write_fd, result)
        except BaseException:
            os._exit(1)
        finally:
            peer.close()
            os.close(write_fd)
        os._exit(0)

    os.close(write_fd)
    try:
        result = os.read(read_fd, 32)
        _waited, status = os.waitpid(pid, 0)
        assert os.waitstatus_to_exitcode(status) == 0, (uid, status)
        assert result == expected, (uid, result, expected)
    finally:
        os.close(read_fd)


def drop_to_coordinator() -> None:
    os.setgroups([])
    os.setgid(COORDINATOR_UID)
    os.setuid(COORDINATOR_UID)


def integrated_read_probe(binary: str) -> None:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        listener.bind(SOCKET_PATH)
        os.chown(SOCKET_PATH, 0, OPERATOR_UID)
        os.chmod(SOCKET_PATH, 0o660)
        listener.listen(4)
        env = {
            "DRUID_PROTECTED_PROOF_MODE": "socket_server",
            "DRUID_PROOF_LISTEN_FD": str(listener.fileno()),
            "RUST_TEST_THREADS": "1",
        }
        server = subprocess.Popen(
            [binary, "--exact", "coord::protected_store::tests::disposable_namespace_probe", "--nocapture"],
            env=env,
            pass_fds=(listener.fileno(),),
            preexec_fn=drop_to_coordinator,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            request_as(OPERATOR_UID, b"not_found")
            request_as(WORKER_UID, b"dac_denied")
            request_as(COORDINATOR_UID, b"dac_denied")
            request_as(0, b"policy_denied")
            output, errors = server.communicate(timeout=10)
            assert server.returncode == 0, (output.decode(), errors.decode())
        finally:
            if server.poll() is None:
                server.kill()
                server.communicate()
    os.unlink(SOCKET_PATH)
    print("PASS: Rust read handler served operator status; root denied by peer policy")


def main() -> None:
    assert os.geteuid() == 0, "namespace root required"
    parent = os.path.dirname(SOCKET_PATH)
    os.makedirs(parent, mode=0o755, exist_ok=False)
    os.chmod(parent, 0o755)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        listener.bind(SOCKET_PATH)
        os.chown(SOCKET_PATH, 0, OPERATOR_UID)
        os.chmod(SOCKET_PATH, 0o660)
        listener.listen(4)
        parent_stat = os.stat(parent)
        socket_stat = os.stat(SOCKET_PATH)
        assert (parent_stat.st_uid, stat.S_IMODE(parent_stat.st_mode)) == (0, 0o755)
        assert (socket_stat.st_uid, socket_stat.st_gid, stat.S_IMODE(socket_stat.st_mode)) == (
            0,
            OPERATOR_UID,
            0o660,
        )
        try_peer(listener, OPERATOR_UID, True)
        try_peer(listener, WORKER_UID, False)
        try_peer(listener, COORDINATOR_UID, False)
        try_peer(listener, 0, True)
    os.unlink(SOCKET_PATH)
    os.rmdir(parent)
    print("PASS: operator connects; worker/coordinator denied by DAC; root peer identified")
    # Run the same ACL with the actual Rust parser/projection/response handler.
    os.makedirs(parent, mode=0o755)
    integrated_read_probe(sys.argv[1])
    os.rmdir(parent)
    print("NOT PROVEN: actual host service, operator group membership at install, or cutover")


if __name__ == "__main__":
    main()
