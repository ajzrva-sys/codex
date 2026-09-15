#!/usr/bin/env python3
"""Select the FreeBSD project-only permission profile for the current user."""

import os
import json
from pathlib import Path
import re
import shutil
import time
import tomllib


START = "# BEGIN CODEX FREEBSD SANDBOX"
END = "# END CODEX FREEBSD SANDBOX"
PROFILE = f"""{START}
[permissions.freebsd-workspace]
description = "FreeBSD jail: project and runtime tools; network disabled"

[permissions.freebsd-workspace.filesystem]
":minimal" = "read"
":project_roots" = "write"
":tmpdir" = "write"
":slash_tmp" = "write"

[permissions.freebsd-workspace.network]
enabled = false
{END}
"""


def main() -> None:
    if os.geteuid() == 0:
        raise SystemExit("Run this as your regular user, not root.")
    home = Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex")))
    home.mkdir(mode=0o700, parents=True, exist_ok=True)
    config = home / "config.toml"
    if config.is_symlink():
        raise SystemExit("Refusing to replace symlinked configuration.")
    text = config.read_text() if config.exists() else ""
    original = text
    if START in text:
        text = re.sub(
            re.escape(START) + r".*?" + re.escape(END) + r"\n?", "", text, flags=re.S
        )
    parsed = tomllib.loads(text)
    if "freebsd-workspace" in parsed.get("permissions", {}):
        raise SystemExit(
            "A user-defined freebsd-workspace profile already exists; refusing to overwrite it."
        )
    lines = text.splitlines(keepends=True)
    in_root = True
    kept = []
    for line in lines:
        if line.lstrip().startswith("["):
            in_root = False
        if in_root and re.match(r"\s*(default_permissions|sandbox_mode)\s*=", line):
            continue
        kept.append(line)
    # Keep authentication out of the default view even if Codex is started
    # directly in the user's home directory.
    hidden = {home.absolute(), Path.home() / ".codex", Path.home() / ".ssh"}
    hidden.update(path.resolve() for path in list(hidden))
    denies = "".join(f'{json.dumps(str(path))} = "deny"\n' for path in sorted(hidden))
    profile = PROFILE.replace(
        "[permissions.freebsd-workspace.network]",
        denies + "\n[permissions.freebsd-workspace.network]",
    )
    text = (
        'default_permissions = "freebsd-workspace"\n'
        + "".join(kept).rstrip()
        + "\n\n"
        + profile
    )
    tomllib.loads(text)
    if text != original:
        if config.exists():
            shutil.copy2(config, f"{config}.backup-{time.time_ns()}")
        temporary = home / f".freebsd-config-{os.getpid()}.toml"
        with temporary.open("x") as output:
            os.fchmod(output.fileno(), 0o600)
            output.write(text)
        os.replace(temporary, config)
    print(
        f"Selected freebsd-workspace in {config}. Existing credentials were not changed."
    )


if __name__ == "__main__":
    main()
