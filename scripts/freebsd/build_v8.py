"""Build V8 using pinned FreeBSD ports patches without changing Cargo's cache."""

import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path

PORTS_COMMIT = "34637c4eac759130933a4c101cc25a9a28664bca"
V8_VERSION = "152.2.0"
PORTS_PATCH_VERSION = "150.4.0"


def build_v8(repo: Path, work: Path) -> dict[str, str]:
    work = work.resolve()
    work.mkdir(parents=True, exist_ok=True)
    triple = (
        subprocess.check_output(["rustc", "-vV"], text=True)
        .split("host: ")[1]
        .splitlines()[0]
    )
    metadata = json.loads(
        subprocess.check_output(
            [
                "cargo",
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--filter-platform",
                triple,
            ],
            cwd=repo / "codex-rs",
            text=True,
        )
    )
    packages = {p["name"]: p for p in metadata["packages"]}
    if packages["v8"]["version"] != V8_VERSION:
        raise RuntimeError(
            "update the pinned FreeBSD ports patches for this V8 version"
        )
    ports = work / "ports"
    if not ports.exists():
        subprocess.run(
            [
                "git",
                "clone",
                "--depth=1",
                "--filter=blob:none",
                "--sparse",
                "https://github.com/freebsd/freebsd-ports.git",
                str(ports),
            ],
            check=True,
        )
        subprocess.run(
            ["git", "fetch", "--depth=1", "--filter=blob:none", "origin", PORTS_COMMIT],
            cwd=ports,
            check=True,
        )
        subprocess.run(
            ["git", "sparse-checkout", "set", "misc/codex"], cwd=ports, check=True
        )
        subprocess.run(
            ["git", "checkout", "--detach", "FETCH_HEAD"], cwd=ports, check=True
        )
    commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ports, text=True
    ).strip()
    if commit != PORTS_COMMIT:
        raise RuntimeError(
            f"expected FreeBSD ports commit {PORTS_COMMIT}, got {commit}"
        )

    patches = sorted(
        (ports / "misc/codex/files").glob(
            f"patch-cargo-crates_v8-{PORTS_PATCH_VERSION}_*"
        )
    )
    if not patches:
        raise RuntimeError("FreeBSD ports V8 patches are missing")
    # Keep the pinned ports fixes, adapting only files changed by V8 152.
    overrides = repo / "scripts/freebsd/v8-patches"
    for index, patch in enumerate(patches):
        override = overrides / patch.name.replace(
            f"v8-{PORTS_PATCH_VERSION}_", f"v8-{V8_VERSION}_", 1
        )
        if override.is_file():
            patches[index] = override
    selected = {patch.name for patch in patches}
    patches.extend(
        patch
        for patch in sorted(overrides.glob(f"patch-cargo-crates_v8-{V8_VERSION}_*"))
        if patch.name not in selected
    )
    patch_digest = hashlib.sha256(
        b"".join(hashlib.sha256(patch.read_bytes()).digest() for patch in patches)
    ).hexdigest()
    patch_identity = f"{PORTS_COMMIT}:{V8_VERSION}:{patch_digest}\n"
    source = work / f"v8-{V8_VERSION}"
    marker = source / ".freebsd-patches-applied"
    if not marker.exists():
        v8_cache = Path(packages["v8"]["manifest_path"]).parent
        icu_cache = Path(packages["deno_core_icudata"]["manifest_path"]).parent
        if source.exists():
            shutil.rmtree(source)
        shutil.copytree(v8_cache, source)
        for patch in patches:
            subprocess.run(
                ["patch", "--batch", "-p2", "-i", str(patch)], cwd=source, check=True
            )
        icu = source / "third_party/icu/common"
        icu.mkdir(parents=True, exist_ok=True)
        shutil.copy2(icu_cache / "src/icudtl.dat", icu / "icudtl.dat")
        marker.write_text(patch_identity)
    if marker.read_text() != patch_identity:
        raise RuntimeError("V8 build directory uses a different FreeBSD patch set")

    (work / "Cargo.toml").write_text(
        '[package]\nname="codex-freebsd-v8-build"\nversion="0.1.0"\nedition="2024"\n[workspace]\n'
        f'[dependencies]\nv8={{path="v8-{V8_VERSION}",features=["v8_enable_sandbox"]}}\n'
    )
    (work / "src").mkdir(exist_ok=True)
    (work / "src/lib.rs").write_text("// Build the native V8 archive and bindings.\n")
    if not (work / "Cargo.lock").exists():
        shutil.copy2(repo / "codex-rs/Cargo.lock", work / "Cargo.lock")
    llvm = Path("/usr/local/llvm21")
    env = {
        **os.environ,
        "CARGO_TARGET_DIR": str(work / "target"),
        "V8_FROM_SOURCE": "1",
        "GN": os.environ.get("GN", "/usr/local/bin/gn"),
        "NINJA": "/usr/local/bin/ninja",
        "CLANG_BASE_PATH": str(llvm),
        "LIBCLANG_PATH": str(llvm / "lib"),
        "CC": str(llvm / "bin/clang"),
        "CXX": str(llvm / "bin/clang++"),
        "AR": str(llvm / "bin/llvm-ar"),
    }
    subprocess.run(["cargo", "build", "--release"], cwd=work, env=env, check=True)
    archive = next((work / "target/release").rglob("librusty_v8.a"))
    bindings = next((work / "target/release").rglob("src_binding.rs"))
    result = {
        "RUSTY_V8_ARCHIVE": str(archive),
        "RUSTY_V8_SRC_BINDING_PATH": str(bindings),
    }
    (work / "artifacts.json").write_text(json.dumps(result, indent=2) + "\n")
    return result
