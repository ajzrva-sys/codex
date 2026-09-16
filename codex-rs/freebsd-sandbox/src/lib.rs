#[cfg(target_os = "freebsd")]
mod client;
#[cfg(target_os = "freebsd")]
mod native;
mod policy;
#[cfg(any(target_os = "freebsd", test))]
mod protocol;

use codex_protocol::models::PermissionProfile;
use std::path::Path;

pub const CLIENT_ARG: &str = "--internal-freebsd-sandbox";
pub const SOCKET_PATH: &str = "/var/run/codex-freebsd-sandbox/control.sock";

pub fn command_args(
    command: Vec<String>,
    cwd: &Path,
    policy_cwd: &Path,
    permissions: &PermissionProfile,
) -> anyhow::Result<Vec<String>> {
    // Validate before spawning; the privileged service repeats this validation.
    policy::compile(permissions, policy_cwd)?;
    let mut args = vec![
        CLIENT_ARG.to_string(),
        "--command-cwd".to_string(),
        cwd.to_string_lossy().into_owned(),
        "--sandbox-policy-cwd".to_string(),
        policy_cwd.to_string_lossy().into_owned(),
        "--permission-profile".to_string(),
        serde_json::to_string(permissions)?,
        "--".to_string(),
    ];
    args.extend(command);
    Ok(args)
}

pub fn run_client() -> ! {
    #[cfg(target_os = "freebsd")]
    let result = client::run();
    #[cfg(not(target_os = "freebsd"))]
    let result: anyhow::Result<i32> = Err(anyhow::anyhow!("FreeBSD jails require FreeBSD"));
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("FreeBSD sandbox: {error:#}");
            std::process::exit(125);
        }
    }
}

pub fn run_daemon() -> anyhow::Result<()> {
    #[cfg(target_os = "freebsd")]
    return native::daemon::run();
    #[cfg(not(target_os = "freebsd"))]
    anyhow::bail!("FreeBSD jails require FreeBSD");
}

pub fn status() -> anyhow::Result<String> {
    #[cfg(target_os = "freebsd")]
    return client::status();
    #[cfg(not(target_os = "freebsd"))]
    anyhow::bail!("FreeBSD jails require FreeBSD");
}
