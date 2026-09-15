#!/usr/bin/env python3
"""Race caller-owned paths against jail setup using disposable credential fixtures."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time

from sandbox_smoke import entry, profile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("client", type=Path)
    args = parser.parse_args()
    if os.geteuid() == 0 or os.uname().sysname != "FreeBSD":
        parser.error("run as an ordinary user on FreeBSD")
    client = args.client.resolve(strict=True)
    prefix = [str(client)]
    if client.name != "codex-freebsd-sandbox":
        prefix.append("--internal-freebsd-sandbox")
    for label in ["source", "destination"]:
        with tempfile.TemporaryDirectory(prefix="codex-race-", dir=Path.home()) as base:
            base = Path(base)
            workspace = base / "workspace"
            workspace.mkdir()
            path = (base if label == "source" else workspace) / "moving"
            path.mkdir()
            outside = base / "outside"
            outside.mkdir()
            if label == "destination":
                (path / "guard").mkdir()
                (outside / "guard").mkdir()
            subject = path if label == "source" else path / "guard"
            secret = outside if label == "source" else outside / "guard"
            (subject / "value").write_text("PUBLIC_FIXTURE")
            (secret / "value").write_text("PRIVATE_FIXTURE")
            parked = base / "parked"
            stopped = threading.Event()
            errors = []

            def substitute():
                try:
                    while not stopped.is_set():
                        path.rename(parked)
                        path.symlink_to(outside, target_is_directory=True)
                        time.sleep(0.002)
                        path.unlink()
                        parked.rename(path)
                        time.sleep(0.002)
                except Exception as error:
                    errors.append(error)
                finally:
                    if path.is_symlink():
                        path.unlink()
                    if parked.exists():
                        parked.rename(path)

            code = f"""from pathlib import Path
import sys
p = Path({str(subject / "value")!r})
try:
    content = p.read_text()
except OSError:
    sys.exit(43)
if content != 'PUBLIC_FIXTURE':
    sys.exit(44)
try:
    p.write_text('UNSAFE_WRITE')
except OSError:
    sys.exit(0)
sys.exit(45)
"""
            command = [
                *prefix,
                "--command-cwd",
                str(workspace),
                "--sandbox-policy-cwd",
                str(workspace),
                "--permission-profile",
                json.dumps(profile(workspace, [entry(subject, "read")])),
                "--",
                "/usr/local/bin/python3",
                "-c",
                code,
            ]
            baseline = subprocess.run(command, capture_output=True, timeout=30)
            assert baseline.returncode == 0, baseline.stderr
            worker = threading.Thread(target=substitute)
            worker.start()
            try:
                for _ in range(16):
                    result = subprocess.run(command, capture_output=True, timeout=30)
                    assert result.returncode in (0, 43, 125), (
                        label,
                        result.returncode,
                        result.stderr,
                    )
                    assert (secret / "value").read_text() == "PRIVATE_FIXTURE"
            finally:
                stopped.set()
                worker.join(timeout=10)
                assert not worker.is_alive(), "path substitution worker did not stop"
            assert not errors, errors
            assert (subject / "value").read_text() == "PUBLIC_FIXTURE"
            print(
                f"PASS {label} substitutions cannot expand access or write through read-only views",
                flush=True,
            )


if __name__ == "__main__":
    main()
