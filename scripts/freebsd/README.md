# Native FreeBSD build

This branch builds the Codex CLI natively on FreeBSD and packages it with the
npm launcher. It does not require Linux compatibility. FreeBSD 15.1 amd64 is
tested with Rust 1.96.1; arm64 launcher selection has tests but needs native testing.

## Build and install

Install the build tools (as root):

```sh
pkg install git rust cmake gmake pkgconf protobuf python3 node24 npm \
  ripgrep bash alsa-lib dbus oniguruma
```

Use Rust at least as recent as `codex-rs/rust-toolchain.toml`. The FreeBSD Rust
package includes Cargo, rustfmt, and Clippy. The build uses the system `protoc`
on FreeBSD, or the executable specified by `PROTOC`.

```sh
git clone -b freebsd-support https://github.com/ajzrva-sys/codex.git
cd codex
python3 scripts/freebsd/package.py
npm install -g ./dist/freebsd/openai-codex-0.0.0-freebsd.tgz
codex --version
codex login --device-auth
codex
```

For a faster development build, pass `--profile dev-small`. To package an
existing native binary, pass `--codex-bin /absolute/path/to/codex`.
`--version` controls the npm package and runtime manifest versions. Source
builds also record the Git commit in the executable. Keep the tarball for installation on compatible
FreeBSD machines with the runtime libraries above.

The tarball includes the native CLI, ripgrep, and the patched npm launcher.
It is self-contained with respect to npm dependencies. The official npm
registry package does not yet distribute a FreeBSD binary; reinstalling that
package will not install this branch's build.

## Scope

The CLI supports the terminal UI, `exec`, app-server, and ordinary shell tools.
FreeBSD currently has no Codex OS sandbox backend. Run in a dedicated account
or jail appropriate to the work; sandbox restrictions are not enforced by this
port. This branch does not change sandbox policy defaults.

The experimental V8 code-mode helper is a separate binary and is not built by
this script. If you build it using the FreeBSD ports V8 patches, include it with
`--code-mode-host-bin /absolute/path/to/codex-code-mode-host`. Ordinary CLI and
shell-tool operation does not require that experimental feature.

## Checks

Native validation covers the terminal UI, shell and patch tool execution, 87
Rust tests, and six npm launcher tests. Descriptor cleanup uses FreeBSD's
`close_range` with `CLOSE_RANGE_CLOEXEC` rather than requiring an fdescfs mount.

```sh
pkg install just nextest
node --test codex-cli/tests/freebsd-launcher.test.js
python3 scripts/freebsd/smoke.py /absolute/path/to/codex
cd codex-rs
just test -p codex-code-mode-protocol -p codex-file-watcher -p codex-utils-pty
```

The smoke test uses a local mock Responses API and a temporary Codex home. It
checks a real shell command, a file write, `apply_patch`, and the tool-result
round trip without needing credentials or making model API calls.

The manually triggered **FreeBSD native CLI** GitHub Actions workflow runs these
checks in a FreeBSD VM and uploads the npm tarball as a build artifact.
