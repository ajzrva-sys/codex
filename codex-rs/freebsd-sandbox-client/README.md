# Shared FreeBSD jail client

Unprivileged transport for Codex and CA, with no Codex application/protocol
dependency. The privileged engine remains in `codex-freebsd-sandbox`.

Consumers pin this crate to an immutable revision of the Codex fork. Build and
test it with `just test -p codex-freebsd-sandbox-client` from `codex-rs`.

`policy::Policy` encodes concrete read/write/deny paths and restricted or enabled
networking into the existing managed permission format. The service recompiles
and validates that policy under the authenticated user's credentials before
pinning mount sources. No execution UID or jail parameters are caller inputs.

`capabilities()` checks root socket-peer identity, protocol compatibility, and
required enforcement capabilities. An older service still accepts legacy Codex
probes and launches, but callers requiring capability discovery must explicitly
upgrade the administrator-installed daemon. There is no unsandboxed fallback.

Run `run(Launch)` only in a dedicated relay process and exit after it returns;
stdin and signal threads belong to that process. Launch descriptions must never
be logged: environment values and command arguments can contain secrets.
Callers must provide a clean, explicitly authorized environment. The jail does
not remove secrets that the caller intentionally places in the launch request.

The protocol retains its existing 1 MiB frame limit and 16 KiB stream chunks.
The service owns dedicated pipes/PTYs, identity changes, mounts, cancellation,
and cleanup. Callers do not pass host filesystem descriptors into workloads.
