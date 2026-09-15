# Native FreeBSD build

This branch builds the Codex CLI natively on FreeBSD and packages it with the
npm launcher. It does not require Linux compatibility. FreeBSD 15.1 amd64 is
tested with Rust 1.96.1; arm64 launcher selection has tests but needs native testing.

## Build and install

Install the build tools (as root):

```sh
pkg install git rust cmake gmake pkgconf protobuf python3 node24 npm \
  ripgrep bash alsa-lib dbus oniguruma gn ninja llvm21 glib
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

The tarball includes the native CLI, code-mode helper, ripgrep, and the patched npm launcher.
It is self-contained with respect to npm dependencies. The official npm
registry package does not yet distribute a FreeBSD binary; reinstalling that
package will not install this branch's build.

## Scope

The CLI supports the terminal UI, `exec`, app-server, and ordinary shell tools.
FreeBSD currently has no Codex OS sandbox backend. Run in a dedicated account
or jail appropriate to the work; sandbox restrictions are not enforced by this
port. This branch does not change sandbox policy defaults.

The default build includes the V8 code-mode helper required by current models.
It applies the [FreeBSD ports V8 patches](https://github.com/freebsd/freebsd-ports/tree/34637c4eac759130933a4c101cc25a9a28664bca/misc/codex/files)
in a separate build directory, preserving Cargo's registry cache and this
repository's dependency lockfile. The first build compiles V8 from source;
later builds reuse its archive and generated bindings. Use `--v8-build-dir`
to choose that directory or `--code-mode-host-bin` to provide a prebuilt helper.
`--cli-only` omits it and requires a model that supports direct tools; models
that require code mode cannot execute tools without the helper.

## Checks

Native validation covers the terminal UI, shell and patch tool execution in
both direct and code mode, a live authenticated default-model session, 234
Rust tests, and six npm launcher tests. Descriptor cleanup uses FreeBSD's
`close_range` with `CLOSE_RANGE_CLOEXEC` rather than requiring an fdescfs mount.

```sh
pkg install just nextest
node --test codex-cli/tests/freebsd-launcher.test.js
python3 scripts/freebsd/smoke.py /absolute/path/to/codex
python3 scripts/freebsd/smoke.py /absolute/path/to/codex --code-mode
cd codex-rs
just test -p codex-code-mode-protocol -p codex-file-watcher -p codex-utils-pty
```

The 147 host/runtime tests additionally use
`just test -p codex-code-mode-host -p codex-code-mode-runtime` with the
`RUSTY_V8_ARCHIVE` and `RUSTY_V8_SRC_BINDING_PATH` environment variables saved in
the V8 build directory's `artifacts.json`.

The smoke test uses a local mock Responses API and a temporary Codex home. It
checks a real shell command, a file write, `apply_patch`, and the tool-result
round trip without needing credentials or making model API calls.

The manually triggered **FreeBSD native CLI** GitHub Actions workflow runs these
checks in a FreeBSD VM and uploads the npm tarball as a build artifact.
