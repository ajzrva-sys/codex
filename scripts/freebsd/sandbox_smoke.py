#!/usr/bin/env python3
"""Exercise the real jail service as an ordinary user using disposable fixtures."""

import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import socket
import struct
import subprocess
import tempfile
import termios
import time


SOCKET = "/var/run/codex-freebsd-sandbox/control.sock"


def entry(path: Path, access: str) -> dict:
    return {"path": {"type": "path", "path": str(path)}, "access": access}


def profile(
    workspace: Path,
    extra: list | None = None,
    network: str = "restricted",
    access: str = "write",
) -> dict:
    return {
        "type": "managed",
        "file_system": {
            "type": "restricted",
            "entries": [
                {
                    "path": {"type": "special", "value": {"kind": "minimal"}},
                    "access": "read",
                },
                entry(workspace, access),
                *(extra or []),
            ],
        },
        "network": network,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("client", type=Path)
    parser.add_argument("--suid-fixture", type=Path)
    args = parser.parse_args()
    if os.geteuid() == 0 or os.uname().sysname != "FreeBSD":
        parser.error(
            "run as an ordinary user on FreeBSD with the jail service installed"
        )
    client = str(args.client.resolve(strict=True))
    prefix = (
        [client, "--internal-freebsd-sandbox"]
        if Path(client).name != "codex-freebsd-sandbox"
        else [client]
    )
    passed = []

    def check(name, condition):
        if not condition:
            raise AssertionError(name)
        passed.append(name)
        print(f"PASS {name}", flush=True)

    with tempfile.TemporaryDirectory(
        prefix="codex-jail-tests-", dir=Path.home()
    ) as temporary:
        base = Path(temporary)
        workspace = base / "project"
        workspace.mkdir()
        sibling = base / "private"
        sibling.mkdir()
        (sibling / "credential").write_text("SECRET_FIXTURE")
        readable = base / "readonly"
        readable.mkdir()
        (readable / "data").write_text("READ_ONLY_FIXTURE")
        (readable / "writable").mkdir()
        subprocess.run(["git", "init", "-q", str(workspace)], check=True)
        original_git = (workspace / ".git/config").read_bytes()

        def command(script, permissions=None, cwd=None):
            cwd = cwd or workspace
            return [
                *prefix,
                "--command-cwd",
                str(cwd),
                "--sandbox-policy-cwd",
                str(cwd),
                "--permission-profile",
                json.dumps(permissions or profile(workspace)),
                "--",
                "/bin/sh",
                "-c",
                script,
            ]

        def run(script, permissions=None):
            result = subprocess.run(
                command(script, permissions), text=True, capture_output=True, timeout=30
            )
            if result.returncode == 125:
                raise AssertionError(f"Sandbox setup failed: {result.stderr}")
            return result

        def python(code, permissions=None):
            import shlex

            return run("/usr/local/bin/python3 -c " + shlex.quote(code), permissions)

        result = run(
            'test "$(id -u)" = "'
            + str(os.getuid())
            + '" && test "$(sysctl -n security.jail.jailed)" = 1 && printf proof > proof.txt && cat proof.txt'
        )
        check(
            "jail identity and persistent workspace writes",
            result.returncode == 0
            and result.stdout == "proof"
            and (workspace / "proof.txt").stat().st_uid == os.getuid(),
        )
        check(
            "shell pipelines and Git",
            run("printf 'b\\na\\n' | sort; git status --porcelain").returncode == 0,
        )
        payload = b"x" * (4 * 1024 * 1024)
        result = subprocess.run(
            command(
                "/usr/local/bin/python3 -c 'import sys,time; time.sleep(0.25); "
                "print(len(sys.stdin.buffer.read()))'"
            ),
            input=payload,
            capture_output=True,
            timeout=60,
        )
        check(
            "large stdin stream survives backpressure",
            result.returncode == 0
            and result.stdout.strip() == str(len(payload)).encode(),
        )
        (workspace / "hello.c").write_text(
            '#include <stdio.h>\nint main(void) { puts("COMPILED"); return 0; }\n'
        )
        result = run("cc hello.c -o hello && ./hello")
        check(
            "native compiler and executable",
            result.returncode == 0 and result.stdout == "COMPILED\n",
        )
        check(
            "sibling credential hidden",
            python(
                f"from pathlib import Path; assert not Path({str(sibling / 'credential')!r}).exists()"
            ).returncode
            == 0,
        )
        check(
            "Codex credentials hidden",
            python(
                "from pathlib import Path; assert not (Path.home()/'.codex/auth.json').exists()"
            ).returncode
            == 0,
        )
        check(
            "SSH directory hidden",
            python(
                "from pathlib import Path; assert not (Path.home()/'.ssh').exists()"
            ).returncode
            == 0,
        )
        (workspace / "escape").symlink_to(sibling / "credential")
        check("absolute symlink escape blocked", run("cat escape").returncode != 0)
        ro = profile(workspace, [entry(readable, "read")])
        check(
            "explicit readable root",
            python(
                f"from pathlib import Path; assert Path({str(readable / 'data')!r}).read_text() == 'READ_ONLY_FIXTURE'",
                ro,
            ).returncode
            == 0,
        )
        check(
            "read-only root rejects writes",
            python(f"open({str(readable / 'data')!r}, 'w').write('bad')", ro).returncode
            != 0,
        )
        (workspace / "ro-link").symlink_to(readable / "data")
        check(
            "symlink cannot bypass read-only mount",
            run("printf bad > ro-link", ro).returncode != 0,
        )
        check(
            "cross-mount hard link blocked",
            python(
                f"import os; os.link({str(readable / 'data')!r}, 'hardlink')", ro
            ).returncode
            != 0,
        )
        check(
            "protected Git metadata",
            run("printf bad > .git/config").returncode != 0
            and (workspace / ".git/config").read_bytes() == original_git,
        )
        check(
            "protected missing Codex metadata",
            run("mkdir -p .codex; printf bad > .codex/config.toml").returncode != 0
            and not (workspace / ".codex/config.toml").exists(),
        )
        overlapping = base / "overlapping"
        overlapping.mkdir()
        processes = []
        try:
            for name in ["first", "second"]:
                script = (
                    f"printf ready > {name}-ready; "
                    f"while [ ! -e {name}-release ]; do sleep 0.05; done; "
                )
                if name == "second":
                    script += (
                        "if printf bad > .codex/race-result; then exit 42; fi; exit 0"
                    )
                else:
                    script += "exit 0"
                process = subprocess.Popen(
                    command(script, profile(overlapping), cwd=overlapping),
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                )
                processes.append(process)
                deadline = time.monotonic() + 30
                while not (overlapping / f"{name}-ready").exists():
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise AssertionError(f"overlapping {name} job did not start")
                    time.sleep(0.05)
            for name, process in zip(["first", "second"], processes):
                (overlapping / f"{name}-release").touch()
                process.communicate(timeout=30)
                check(
                    f"overlapping {name} job preserves metadata protection",
                    process.returncode == 0,
                )
            check(
                "cleanup cannot reopen another job's protected path",
                not (overlapping / ".codex/race-result").exists(),
            )
        finally:
            for process in processes:
                if process.poll() is None:
                    process.kill()
                process.communicate(timeout=10)
        check(
            "read-only workspace",
            run("printf bad > proof.txt", profile(workspace, access="read")).returncode
            != 0,
        )
        protected_file = workspace / "readonly-file"
        protected_file.write_text("FILE_FIXTURE")
        file_read = profile(workspace, [entry(protected_file, "read")])
        check(
            "read-only file exception inside writable directory",
            run("cat readonly-file", file_read).stdout == "FILE_FIXTURE"
            and run("printf bad > readonly-file", file_read).returncode != 0
            and protected_file.read_text() == "FILE_FIXTURE",
        )
        writable_file = readable / "single-write"
        writable_file.write_text("before")
        file_write = profile(
            workspace, [entry(readable, "read"), entry(writable_file, "write")]
        )
        check(
            "writable file exception persists through read-only parent",
            python(
                f"open({str(writable_file)!r}, 'w').write('after')", file_write
            ).returncode
            == 0
            and writable_file.read_text() == "after"
            and python(
                f"open({str(readable / 'data')!r}, 'w').write('bad')", file_write
            ).returncode
            != 0,
        )
        nested = profile(
            workspace, [entry(readable, "read"), entry(readable / "writable", "write")]
        )
        check(
            "writable exception beneath read-only root",
            python(
                f"open({str(readable / 'writable/proof')!r}, 'w').write('nested')",
                nested,
            ).returncode
            == 0,
        )
        denied = workspace / "denied"
        denied.mkdir()
        (denied / "secret").write_text("DENIED_FIXTURE")
        (denied / "public").write_text("PUBLIC_FIXTURE")
        restricted = profile(
            workspace, [entry(denied, "deny"), entry(denied / "public", "read")]
        )
        check(
            "denied ancestor with readable exception",
            python(
                f"from pathlib import Path; assert Path({str(denied / 'public')!r}).read_text() == 'PUBLIC_FIXTURE'; assert not Path({str(denied / 'secret')!r}).exists()",
                restricted,
            ).returncode
            == 0,
        )
        denied_file = profile(workspace, [entry(denied / "secret", "deny")])
        check(
            "denied file remains unreadable",
            python(f"open({str(denied / 'secret')!r}).read()", denied_file).returncode
            != 0,
        )
        check(
            "no inherited host descriptors",
            python(
                "import os; leaked=[]\nfor fd in range(3,256):\n try: os.fstat(fd); leaked.append(fd)\n except OSError: pass\nassert not leaked, leaked"
            ).returncode
            == 0,
        )
        (workspace / "source-link").symlink_to(sibling, target_is_directory=True)
        invalid = subprocess.run(
            command(
                "printf unsafe > race-result",
                profile(workspace, [entry(workspace / "source-link", "read")]),
            ),
            capture_output=True,
            timeout=30,
        )
        check(
            "symlink source rejected before execution",
            invalid.returncode == 125 and not (workspace / "race-result").exists(),
        )
        (workspace / "mountpoint").mkdir()
        check(
            "mount privileges denied",
            run("/sbin/mount_nullfs . mountpoint").returncode != 0,
        )
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            port = listener.getsockname()[1]
            connection = f"import socket; socket.create_connection(('127.0.0.1', {port}), 1).close()"
            check("IPv4 and host loopback blocked", python(connection).returncode != 0)
            check(
                "explicit unrestricted network",
                python(connection, profile(workspace, network="enabled")).returncode
                == 0,
            )
        check(
            "IPv6 sockets blocked",
            python(
                "import socket; socket.socket(socket.AF_INET6, socket.SOCK_STREAM)"
            ).returncode
            != 0,
        )
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(str(workspace / "host.sock"))
            listener.listen()
            check(
                "host Unix socket bypass blocked",
                python(
                    "import socket; s=socket.socket(socket.AF_UNIX); s.connect('host.sock')"
                ).returncode
                != 0,
            )
        victim = subprocess.Popen(["/bin/sleep", "60"])
        try:
            check(
                "host same-user process signals blocked",
                python(f"import os; os.kill({victim.pid}, 0)").returncode != 0,
            )
            check(
                "host same-user debugging blocked",
                python(
                    f"import ctypes; c=ctypes.CDLL(None,use_errno=True); assert c.ptrace(10,{victim.pid},None,0) == -1"
                ).returncode
                == 0,
            )
        finally:
            victim.kill()
            victim.wait()
        if args.suid_fixture:
            result = run(str(args.suid_fixture))
            check(
                "SUID cannot elevate",
                result.returncode == 0 and result.stdout.strip() == str(os.getuid()),
            )
        inaccessible = base / "inaccessible"
        inaccessible.mkdir(mode=0)
        try:
            invalid = subprocess.run(
                command("true", profile(workspace, [entry(inaccessible, "read")])),
                capture_output=True,
                text=True,
                timeout=30,
            )
            check(
                "service cannot expose inaccessible source", invalid.returncode == 125
            )
        finally:
            inaccessible.chmod(0o700)
        for name, message in [
            ("wrong protocol rejected", {"type": "Probe", "version": 999}),
            ("forged identity rejected", {"type": "Probe", "version": 1, "uid": 0}),
        ]:
            with socket.socket(socket.AF_UNIX) as peer:
                peer.settimeout(5)
                peer.connect(SOCKET)
                data = json.dumps(message).encode()
                peer.sendall(struct.pack("!I", len(data)) + data)
                header = peer.recv(4, socket.MSG_WAITALL)
                response = json.loads(
                    peer.recv(struct.unpack("!I", header)[0], socket.MSG_WAITALL)
                )
                check(name, response["type"] == "Error")
        with socket.socket(socket.AF_UNIX) as peer:
            peer.settimeout(5)
            peer.connect(SOCKET)
            peer.sendall(struct.pack("!I", 1024 * 1024 + 1))
            header = peer.recv(4, socket.MSG_WAITALL)
            response = json.loads(
                peer.recv(struct.unpack("!I", header)[0], socket.MSG_WAITALL)
            )
            check("oversized request rejected", response["type"] == "Error")
        master, slave = pty.openpty()
        process = subprocess.Popen(
            command(
                "test -t 0 && test -t 1 && printf PTY_READY; "
                "IFS= read -r line; printf 'READ:%s\\n' \"$line\"; stty size"
            ),
            stdin=slave,
            stdout=slave,
            stderr=slave,
        )
        os.close(slave)
        output = bytearray()
        input_sent = False
        deadline = time.monotonic() + 15
        try:
            while time.monotonic() < deadline:
                ready, _, _ = select.select([master], [], [], 0.2)
                if ready:
                    try:
                        part = os.read(master, 16384)
                    except OSError:
                        break
                    if not part:
                        break
                    output.extend(part)
                    if not input_sent and b"PTY_READY" in output:
                        fcntl.ioctl(
                            master,
                            termios.TIOCSWINSZ,
                            struct.pack("HHHH", 42, 91, 0, 0),
                        )
                        process.send_signal(signal.SIGWINCH)
                        time.sleep(0.1)
                        os.write(master, b"hello\n")
                        input_sent = True
                if process.poll() is not None and not ready:
                    break
            check(
                "dedicated PTY input and resizing",
                process.wait(timeout=5) == 0
                and b"READ:hello" in output
                and b"42 91" in output,
            )
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
        check(
            "read-only fixture unchanged",
            (readable / "data").read_text() == "READ_ONLY_FIXTURE",
        )
        for label, stop_signal in [
            ("cancellation", signal.SIGTERM),
            ("client disconnect", signal.SIGKILL),
        ]:
            heartbeat = workspace / "heartbeat"
            heartbeat.unlink(missing_ok=True)
            with tempfile.TemporaryFile() as blocked_input:
                blocked_input.write(payload)
                blocked_input.seek(0)
                process = subprocess.Popen(
                    command(
                        "(while :; do printf . >> heartbeat; sleep 0.05; done) & wait"
                    ),
                    stdin=blocked_input,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
            try:
                deadline = time.monotonic() + 10
                while not heartbeat.exists() and time.monotonic() < deadline:
                    time.sleep(0.05)
                check(f"{label} fixture started", heartbeat.exists())
                time.sleep(0.5)  # Fill the relay's stdin queue before cancellation.
                process.send_signal(stop_signal)
                process.wait(timeout=10)
                time.sleep(1)
                size = heartbeat.stat().st_size
                time.sleep(0.5)
                check(f"{label} kills descendants", heartbeat.stat().st_size == size)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
    print(f"{len(passed)} native jail checks passed.")


if __name__ == "__main__":
    main()
