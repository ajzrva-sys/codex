use anyhow::Result;
use clap::Parser;
use codex_protocol::models::PermissionProfile;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    command_cwd: PathBuf,
    #[arg(long)]
    sandbox_policy_cwd: PathBuf,
    #[arg(long)]
    permission_profile: String,
    #[arg(required = true, trailing_var_arg = true)]
    command: Vec<String>,
}

pub(crate) fn status() -> Result<String> {
    codex_freebsd_sandbox_client::status()
}

pub(crate) fn run() -> Result<i32> {
    let mut argv: Vec<_> = std::env::args_os().collect();
    if argv.get(1).is_some_and(|arg| arg == crate::CLIENT_ARG) {
        argv.remove(1);
    }
    let args = Args::parse_from(argv);
    let permissions: PermissionProfile = serde_json::from_str(&args.permission_profile)?;
    crate::policy::compile(&permissions, &args.sandbox_policy_cwd)?;
    codex_freebsd_sandbox_client::run(crate::protocol::Launch {
        version: crate::protocol::VERSION,
        argv: args.command,
        cwd: args.command_cwd,
        policy_cwd: args.sandbox_policy_cwd,
        permissions: serde_json::to_value(permissions)?,
        env: std::env::vars().collect(),
        terminal: None,
    })
}
