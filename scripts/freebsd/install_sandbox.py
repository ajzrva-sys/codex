#!/usr/bin/env python3
"""Install the separately administered FreeBSD jail service (run as root)."""

import argparse
import json
import os
from pathlib import Path
import pwd
import shutil
import subprocess
import time


RC = """#!/bin/sh
# PROVIDE: codex_freebsd_sandbox
# REQUIRE: FILESYSTEMS
# KEYWORD: shutdown
. /etc/rc.subr
name="codex_freebsd_sandbox"
rcvar="codex_freebsd_sandbox_enable"
load_rc_config "$name"
: ${codex_freebsd_sandbox_enable:="NO"}
pidfile="/var/run/codex-freebsd-sandbox.supervisor.pid"
command="/usr/sbin/daemon"
command_args="-f -r -R 5 -P ${pidfile} -p /var/run/codex-freebsd-sandbox.pid -o /var/log/codex-freebsd-sandbox.log /usr/local/libexec/codex-freebsd-sandboxd"
start_precmd="codex_sandbox_modules"
codex_sandbox_modules() {
    /sbin/kldload -n nullfs && /sbin/kldload -n tmpfs && /sbin/kldload -n fdescfs
}
run_rc_command "$1"
"""


def replace_file(destination: Path, content: bytes, mode: int) -> None:
    if destination.is_symlink():
        raise SystemExit(f"Refusing symlink: {destination}")
    if destination.exists() and destination.read_bytes() != content:
        shutil.copy2(destination, f"{destination}.backup-{time.time_ns()}")
    temporary = destination.with_name(f".{destination.name}.{os.getpid()}")
    with temporary.open("xb") as output:
        output.write(content)
        os.fchmod(output.fileno(), mode)
        os.fchown(output.fileno(), 0, 0)
    os.replace(temporary, destination)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--user", required=True)
    parser.add_argument("--daemon", type=Path, required=True)
    args = parser.parse_args()
    if os.geteuid() != 0 or os.uname().sysname != "FreeBSD":
        parser.error("run this installer as root on FreeBSD")
    uid = pwd.getpwnam(args.user).pw_uid
    if uid == 0:
        parser.error("the sandbox user must not be root")
    for directory in ["/usr/local/libexec", "/usr/local/etc/rc.d", "/etc/rc.conf.d"]:
        target = Path(directory)
        target.mkdir(parents=True, exist_ok=True)
        for parent in [target, *target.parents]:
            metadata = parent.stat()
            if parent.is_symlink() or metadata.st_uid != 0 or metadata.st_mode & 0o022:
                raise SystemExit(f"Unsafe privileged installation directory: {parent}")
    config = Path("/usr/local/etc/codex-freebsd-sandbox.json")
    if config.is_symlink():
        raise SystemExit("Refusing symlinked service configuration")
    if config.exists():
        metadata = config.stat()
        if metadata.st_uid != 0 or metadata.st_mode & 0o022 or not config.is_file():
            raise SystemExit(
                "Existing service configuration is not protected by root ownership"
            )
    settings = (
        json.loads(config.read_text()) if config.exists() else {"allowed_uids": []}
    )
    settings["allowed_uids"] = sorted(set(settings["allowed_uids"]) | {uid})
    replace_file(config, (json.dumps(settings, indent=2) + "\n").encode(), 0o600)
    replace_file(
        Path("/usr/local/libexec/codex-freebsd-sandboxd"),
        args.daemon.read_bytes(),
        0o755,
    )
    replace_file(Path("/usr/local/etc/rc.d/codex_freebsd_sandbox"), RC.encode(), 0o555)
    for module in ["nullfs", "tmpfs", "fdescfs"]:
        subprocess.run(["/sbin/kldload", "-n", module], check=True)
    subprocess.run(
        [
            "/usr/sbin/sysrc",
            "-f",
            "/etc/rc.conf.d/codex_freebsd_sandbox",
            "codex_freebsd_sandbox_enable=YES",
        ],
        check=True,
    )
    subprocess.run(
        ["/usr/sbin/service", "codex_freebsd_sandbox", "onestop"], check=False
    )
    subprocess.run(
        ["/usr/sbin/service", "codex_freebsd_sandbox", "onestart"], check=True
    )
    print(f"Installed jail service for {args.user} (UID {uid}).")
    print("As that user, run configure_sandbox.py to select the project-only profile.")


if __name__ == "__main__":
    main()
