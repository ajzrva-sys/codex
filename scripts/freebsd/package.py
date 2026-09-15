#!/usr/bin/env python3
"""Build a native FreeBSD CLI and an installable, self-contained npm tarball."""

import argparse
import json
import os
import platform
import shutil
import subprocess
import tempfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", default="release")
    parser.add_argument("--version", default="0.0.0-freebsd")
    parser.add_argument("--codex-bin", type=Path)
    parser.add_argument("--code-mode-host-bin", type=Path)
    parser.add_argument("--output-dir", type=Path, default=REPO_ROOT / "dist/freebsd")
    args = parser.parse_args()
    if platform.system() != "FreeBSD":
        parser.error("run this script on the FreeBSD build host")
    architectures = {
        "amd64": ("x86_64-unknown-freebsd", "x64"),
        "x86_64": ("x86_64-unknown-freebsd", "x64"),
        "arm64": ("aarch64-unknown-freebsd", "arm64"),
        "aarch64": ("aarch64-unknown-freebsd", "arm64"),
    }
    if platform.machine() not in architectures:
        parser.error(f"unsupported FreeBSD architecture: {platform.machine()}")
    target, cpu = architectures[platform.machine()]
    codex = args.codex_bin
    if codex is None:
        commit = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, text=True
        ).strip()
        subprocess.run(
            [
                "cargo",
                "build",
                "--locked",
                "--profile",
                args.profile,
                "-p",
                "codex-cli",
                "--bin",
                "codex",
            ],
            cwd=REPO_ROOT / "codex-rs",
            env={**os.environ, "STABLE_GIT_COMMIT": commit},
            check=True,
        )
        target_dir = Path(os.environ.get("CARGO_TARGET_DIR", "target"))
        if not target_dir.is_absolute():
            target_dir = REPO_ROOT / "codex-rs" / target_dir
        profile_dir = "debug" if args.profile == "dev" else args.profile
        codex = target_dir / profile_dir / "codex"
    codex = codex.resolve(strict=True)
    subprocess.run([str(codex), "--version"], check=True)
    rg = shutil.which("rg")
    if rg is None:
        parser.error("install ripgrep with pkg install ripgrep")
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="codex-freebsd-") as temp:
        package = Path(temp)
        (package / "bin").mkdir()
        shutil.copy2(REPO_ROOT / "codex-cli/bin/codex.js", package / "bin/codex.js")
        native = package / "vendor" / target
        (native / "bin").mkdir(parents=True)
        (native / "codex-path").mkdir()
        (native / "codex-resources").mkdir()
        shutil.copy2(codex, native / "bin/codex")
        shutil.copy2(rg, native / "codex-path/rg")
        if args.code_mode_host_bin:
            shutil.copy2(args.code_mode_host_bin, native / "bin/codex-code-mode-host")
        for executable in (native / "bin").iterdir():
            subprocess.run(["strip", str(executable)], check=True)
        (native / "codex-package.json").write_text(
            json.dumps(
                {
                    "layoutVersion": 1,
                    "target": target,
                    "variant": "codex",
                    "version": args.version,
                    "entrypoint": "bin/codex",
                    "resourcesDir": "codex-resources",
                    "pathDir": "codex-path",
                },
                indent=2,
            )
            + "\n"
        )
        metadata = json.loads((REPO_ROOT / "codex-cli/package.json").read_text())
        metadata.update(
            {
                "version": args.version,
                "os": ["freebsd"],
                "cpu": [cpu],
                "files": ["bin/codex.js", "vendor", "README.md", "LICENSE"],
            }
        )
        (package / "package.json").write_text(json.dumps(metadata, indent=2) + "\n")
        shutil.copy2(REPO_ROOT / "scripts/freebsd/README.md", package / "README.md")
        shutil.copy2(REPO_ROOT / "LICENSE", package / "LICENSE")
        subprocess.run(
            ["npm", "pack", "--pack-destination", str(output)],
            cwd=package,
            check=True,
        )


if __name__ == "__main__":
    main()
