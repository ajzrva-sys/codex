//! Unprivileged transport shared by Codex and CA. The service independently
//! validates every policy and authenticates the caller using kernel credentials.

pub mod policy;
pub mod protocol;
#[cfg(target_os = "freebsd")]
mod transport;

pub const SOCKET_PATH: &str = "/var/run/codex-freebsd-sandbox/control.sock";
pub const REQUIRED_CAPABILITIES: &[&str] = &[
    "explicit-filesystem-v1",
    "peer-identity-v1",
    "jail-lifecycle-v1",
    "stdio-pty-v1",
];

/// Reject incompatible services before attempting to execute a workload.
pub fn validate_capabilities(status: &protocol::Capabilities) -> anyhow::Result<()> {
    anyhow::ensure!(
        status.version == protocol::VERSION,
        "incompatible sandbox protocol"
    );
    anyhow::ensure!(
        status.service == "codex-freebsd-sandboxd",
        "unexpected sandbox service"
    );
    for required in REQUIRED_CAPABILITIES {
        anyhow::ensure!(
            status.features.iter().any(|feature| feature == required),
            "sandbox service lacks required capability: {required}"
        );
    }
    Ok(())
}

/// Probe the authenticated root service without launching a command.
pub fn capabilities() -> anyhow::Result<protocol::Capabilities> {
    #[cfg(target_os = "freebsd")]
    return transport::capabilities();
    #[cfg(not(target_os = "freebsd"))]
    anyhow::bail!("FreeBSD jails require FreeBSD");
}

/// Legacy-compatible human-readable status for existing Codex clients.
pub fn status() -> anyhow::Result<String> {
    #[cfg(target_os = "freebsd")]
    return transport::status();
    #[cfg(not(target_os = "freebsd"))]
    anyhow::bail!("FreeBSD jails require FreeBSD");
}

/// Relay the invoking process's standard streams and signals to a jail job.
/// Invoke in a dedicated client process, which must exit after this returns:
/// its stdin/signal threads intentionally live for that process's lifetime.
pub fn run(request: protocol::Launch) -> anyhow::Result<i32> {
    #[cfg(target_os = "freebsd")]
    return transport::run(request);
    #[cfg(not(target_os = "freebsd"))]
    {
        let _ = request;
        anyhow::bail!("FreeBSD jails require FreeBSD");
    }
}

#[cfg(test)]
mod tests;
