#!/usr/bin/env python3
"""Run native sandbox authorization, SUID, and service lifecycle tests as root.

This deliberately restarts the sandbox service. Use a dedicated test host.
Workloads always run as the specified ordinary user.
"""

import argparse
import json
import os
from pathlib import Path
import pwd
import shutil
import signal
import subprocess
import tempfile
import time

from sandbox_smoke import SOCKET, entry, profile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--user", required=True)
    parser.add_argument("--client", type=Path, required=True)
    args = parser.parse_args()
    if os.geteuid() != 0 or os.uname().sysname != "FreeBSD":
        parser.error("run as root on a dedicated FreeBSD test host")
    account = pwd.getpwnam(args.user)
    if account.pw_uid == 0:
        parser.error("workloads must run as an ordinary user")
    identity = dict(
        user=account.pw_uid,
        group=account.pw_gid,
        extra_groups=os.getgrouplist(args.user, account.pw_gid),
    )
    env = {
        "PATH": "/bin:/sbin:/usr/bin:/usr/sbin:/usr/local/bin:/usr/local/sbin",
        "HOME": account.pw_dir,
        "LANG": "C.UTF-8",
    }
    client = str(args.client.resolve(strict=True))
    prefix = (
        [client, "--internal-freebsd-sandbox"]
        if Path(client).name != "codex-freebsd-sandbox"
        else [client]
    )

    def run_user(argv, **kwargs):
        kwargs.setdefault("timeout", 45)
        return subprocess.run(
            argv,
            **identity,
            env=env,
            cwd="/",
            capture_output=True,
            text=True,
            **kwargs,
        )

    def service(action):
        subprocess.run(
            ["/usr/sbin/service", "codex_freebsd_sandbox", action],
            check=True,
            timeout=30,
        )

    def wait_for(predicate, message, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.1)
        raise AssertionError(message)

    with tempfile.TemporaryDirectory(
        prefix="codex-sandbox-test-", dir="/usr/local/libexec"
    ) as shared:
        shared = Path(shared)
        shared.chmod(0o755)
        fixture = shared / "suid"
        subprocess.run(
            ["cc", "-x", "c", "-", "-o", str(fixture)],
            check=True,
            text=True,
            input='#include <stdio.h>\n#include <unistd.h>\nint main(void){printf("%u\\n",(unsigned)geteuid());return 0;}\n',
        )
        fixture.chmod(0o4755)
        smoke = shared / "sandbox_smoke.py"
        shutil.copyfile(Path(__file__).with_name("sandbox_smoke.py"), smoke)
        result = run_user(
            [
                "/usr/local/bin/python3",
                str(smoke),
                client,
                "--suid-fixture",
                str(fixture),
            ],
            timeout=180,
        )
        print(result.stdout, end="", flush=True)
        assert result.returncode == 0, result.stderr
        races = shared / "sandbox_races.py"
        shutil.copyfile(Path(__file__).with_name("sandbox_races.py"), races)
        result = run_user(["/usr/local/bin/python3", str(races), client], timeout=180)
        print(result.stdout, end="", flush=True)
        assert result.returncode == 0, result.stderr

    nobody = pwd.getpwnam("nobody")
    probe = (
        """import socket,struct,json
s=socket.socket(socket.AF_UNIX);s.settimeout(5);s.connect(%r)
h=s.recv(4,socket.MSG_WAITALL);r=json.loads(s.recv(struct.unpack('!I',h)[0],socket.MSG_WAITALL))
assert r['type']=='Error' and 'not authorized' in r['message']
"""
        % SOCKET
    )
    result = subprocess.run(
        ["/usr/local/bin/python3", "-c", probe],
        user=nobody.pw_uid,
        group=nobody.pw_gid,
        extra_groups=[],
        env=env,
        cwd="/",
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 0, result.stderr
    print("PASS unauthorized peer rejected", flush=True)

    with tempfile.TemporaryDirectory(
        prefix="codex-jail-lifecycle-", dir="/var/tmp"
    ) as workspace:
        workspace = Path(workspace)
        os.chown(workspace, account.pw_uid, account.pw_gid)

        def command(script, permissions=None):
            return [
                *prefix,
                "--command-cwd",
                str(workspace),
                "--sandbox-policy-cwd",
                str(workspace),
                "--permission-profile",
                json.dumps(permissions or profile(workspace)),
                "--",
                "/bin/sh",
                "-c",
                script,
            ]

        def ready():
            result = run_user(command("true"))
            return result.returncode == 0

        for label in ["service shutdown", "daemon crash"]:
            marker = workspace / "heartbeat"
            marker.unlink(missing_ok=True)
            process = subprocess.Popen(
                command("(while :; do printf . >> heartbeat; sleep 0.05; done) & wait"),
                **identity,
                env=env,
                cwd="/",
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            try:
                wait_for(marker.exists, "lifecycle fixture did not start")
                if label == "service shutdown":
                    service("onestop")
                    failed = run_user(command("touch should-not-run"))
                    assert (
                        failed.returncode == 125
                        and not (workspace / "should-not-run").exists()
                    )
                    print("PASS unavailable service fails closed", flush=True)
                    service("onestart")
                else:
                    daemon_pid = int(
                        Path("/var/run/codex-freebsd-sandbox.pid").read_text()
                    )
                    os.kill(daemon_pid, signal.SIGKILL)
                process.wait(timeout=15)
                time.sleep(0.5)
                size = marker.stat().st_size
                time.sleep(0.5)
                assert marker.stat().st_size == size, "job descendants survived"
                wait_for(ready, "service did not recover", timeout=30)
                print(f"PASS {label} terminates descendants and recovers", flush=True)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()

        readonly = workspace / "readonly"
        readonly.mkdir()
        os.chown(readonly, account.pw_uid, account.pw_gid)
        bad = profile(
            workspace, [entry(readonly, "read"), entry(readonly / "missing", "deny")]
        )
        result = run_user(command("touch should-not-run", bad))
        assert (
            result.returncode == 125 and not (workspace / "should-not-run").exists()
        ), result.stderr
        print("PASS partial setup failure does not execute command", flush=True)
        wait_for(
            lambda: not list(Path("/var/run/codex-freebsd-sandbox/jobs").iterdir()),
            "quarantined resources did not recover",
            timeout=120,
        )
        print("PASS lifecycle cleanup leaves no job resources", flush=True)


if __name__ == "__main__":
    main()
