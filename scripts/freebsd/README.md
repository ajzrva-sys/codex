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

Build as your regular user:

```sh
git clone -b freebsd-support https://github.com/ajzrva-sys/codex.git
cd codex
python3 scripts/freebsd/package.py
```

Install the resulting package as root, using the absolute path to your tarball:

```sh
npm install -g /absolute/path/to/codex/dist/freebsd/openai-codex-0.0.0-freebsd.tgz
```

For a faster development build, pass `--profile dev-small`. To package an
existing native binary, pass `--codex-bin /absolute/path/to/codex`.
`--version` controls the npm package and runtime manifest versions. Source
builds also record the Git commit in the executable. Keep the tarball for installation on compatible
FreeBSD machines with the runtime libraries above.

The tarball includes the native CLI, code-mode helper, sandbox daemon, administrator scripts, ripgrep,
and the patched npm launcher.
It is self-contained with respect to npm dependencies. The official npm
registry package does not yet distribute a FreeBSD binary; reinstalling that
package will not install this branch's build.

## Run as your regular user

Return to your regular user account before signing in or starting Codex. The
global installation is available to ordinary users; running Codex does not
require root. Credentials and configuration belong to the account that runs it
and are stored in that account's `~/.codex` by default.

```sh
codex --version
codex login --device-auth
codex login status
codex -a on-request
```

## Native jail sandbox

FreeBSD 15.1 amd64 supports a separate jail for each tool invocation. Codex and
its authenticated model connection run outside the jail as your ordinary user.
Shells, PTYs, filesystem helpers, and patches execute inside it as that same UID.

Install the service explicitly as root after installing the npm package (replace
`aji` with the account that will run Codex):

```sh
python3 "$(npm root -g)/@openai/codex/freebsd/install_sandbox.py" \
  --user aji \
  --daemon "$(npm root -g)/@openai/codex/vendor/x86_64-unknown-freebsd/bin/codex-freebsd-sandboxd"
```

The installer copies the daemon to `/usr/local/libexec`, writes a root-owned UID
allowlist at `/usr/local/etc/codex-freebsd-sandbox.json`, and enables the
`codex_freebsd_sandbox` rc service. It loads nullfs, tmpfs, and fdescfs at startup.
Npm installation itself does not enable privileged services. No SUID executable
or changes to `vfs.usermount` or `security.bsd.unprivileged_chroot` are needed.

As your regular user, select the named permission profile, then verify it:

```sh
python3 "$(npm root -g)/@openai/codex/freebsd/configure_sandbox.py"
cd /path/to/project
codex doctor
codex sandbox -- /bin/sh -c 'id; sysctl security.jail.jailed'
codex -a on-request
```

The `freebsd-workspace` profile grants project writes and reads of installed
runtime tools, libraries, headers, and shared resources. It provides private
`/tmp`, `/var/tmp`, and a minimal home; host credentials and other projects are
absent. Host absolute paths are preserved. Existing protected `.git`, `.codex`,
and `.agents` rules remain enforced. Approved noninteractive workspace patches
can run with this profile. Missing protected directories are created empty as
the caller so they can serve as mount targets. They remain after cleanup to
preserve protection for other jobs running in the same workspace.

FreeBSD does not allow stacked file mounts. Read-only file exceptions inside a
writable directory therefore use a content snapshot for that command. Writable
file exceptions inside a read-only directory use a private snapshot of the
parent's directory entries; writes to the granted file still reach the host.
Each tool invocation constructs a fresh view.
Streamed file reads copy the jailed file into an anonymous temporary snapshot
outside the jail, using bounded buffers. No host socket is passed into the jail
to transfer descriptors.

To grant another directory, add its absolute path with `"read"`, `"write"`, or
`"deny"` under `[permissions.freebsd-workspace.filesystem]`. Concrete nested
exceptions are supported. Symlinked source paths are rejected; use their real
absolute paths. Filesystem glob policies, full-host-root views (including legacy
`-s workspace-write` / `-s read-only` profiles), and managed network proxies are
rejected. Service errors block execution; there is no automatic unsandboxed
fallback. Explicit danger-full-access/approval bypasses retain their existing
meaning and should only be selected deliberately.

Networking defaults to disabled for both IPv4 and IPv6, including host loopback.
Setting `enabled = true` under `[permissions.freebsd-workspace.network]` permits
inherited host IP networking while retaining the jail's filesystem and process
isolation. Nullfs views use `nosuid` and `nounixbypass`; only a small devfs device
allowlist is exposed. Commands receive dedicated pipes or PTYs and no service or
host directory descriptors. They cannot signal or trace processes outside their
jail. Commands use `PROC_NO_NEW_PRIVS`; ordinary Unix tools do not enter Capsicum.

The service authenticates kernel peer credentials against its UID allowlist and
accepts bounded, versioned messages. It pins mount sources with the caller's
credentials and uses private descriptor-based mount paths. The daemon must match
the packaged client protocol. Use matching builds for remote exec-server peers.

Cancellation, disconnection, and service shutdown remove the job jail, killing
its descendants. Cleanup unmounts views before deleting scaffolding. Busy views
remain in a root-only quarantine and are retried periodically and at startup;
TCP references can briefly delay cleanup after network-enabled commands.
Logs are written to `/var/log/codex-freebsd-sandbox.log`. Crash recovery verifies
the service journal and jail path before removal. Resource-exhaustion quotas,
full Bubblewrap compatibility, and managed proxies are outside this version.

### Rollback

The installer and configuration helper create timestamped `.backup-*` copies
when replacing existing files. Keep the previous npm tarball as well. Stop the
service before restoring its previous executable and matching package, then
restart it. Restore the user's `config.toml.backup-*` if needed. Stopping the
service makes sandboxed commands fail closed. Never recursively delete the
service's job directories while any filesystem remains mounted beneath them.

The default build includes the V8 code-mode helper required by current models.
It applies the [FreeBSD ports V8 patches](https://github.com/freebsd/freebsd-ports/tree/34637c4eac759130933a4c101cc25a9a28664bca/misc/codex/files)
in a separate build directory, preserving Cargo's registry cache and this
repository's dependency lockfile. The first build compiles V8 from source;
later builds reuse its archive and generated bindings. Use `--v8-build-dir`
to choose that directory or `--code-mode-host-bin` to provide a prebuilt helper.
`--cli-only` omits it and requires a model that supports direct tools; models
that require code mode cannot execute tools without the helper.

## Checks

Native jail validation on FreeBSD 15.1 amd64 covers shell pipelines, Git,
compilation, PTY input and resizing, filesystem and process containment, network
policies, malformed requests, path substitution races, cancellation, and crash
recovery. Direct and code-mode sessions exercise shell tools and approved
workspace patches, with protected edits rejected. A signed-in session as UID
1001 also completed a real edit and its Python assertion. Scoped sandbox,
protocol, and PTY Rust suites and the npm launcher tests complement these checks.
Descriptor cleanup uses `close_range` with `CLOSE_RANGE_CLOEXEC`; privileged
mount setup uses a private `fdescfs` view with `nodup`.

```sh
pkg install just nextest
node --test codex-cli/tests/freebsd-launcher.test.js
python3 scripts/freebsd/smoke.py /absolute/path/to/codex
python3 scripts/freebsd/smoke.py /absolute/path/to/codex --code-mode
# As the allowlisted ordinary user, with the service running:
python3 scripts/freebsd/sandbox_smoke.py /absolute/path/to/codex
python3 scripts/freebsd/sandbox_races.py /absolute/path/to/codex
python3 scripts/freebsd/smoke.py /absolute/path/to/codex --sandbox
python3 scripts/freebsd/smoke.py /absolute/path/to/codex --sandbox --code-mode
cd codex-rs
just test -p codex-freebsd-sandbox -p codex-sandboxing -p codex-exec-server-protocol
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

On a dedicated test host, the administrator can additionally run the SUID,
unauthorized-peer, stopped-service, daemon-crash, and cleanup checks. This
intentionally restarts the sandbox service; its workloads run as the named user:

```sh
python3 scripts/freebsd/sandbox_admin_smoke.py --user aji --client /absolute/path/to/codex
```
