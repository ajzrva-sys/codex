//! Dedicated relay for callers that provide an explicitly selected environment.
//! The privileged service authenticates the peer and validates every grant.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use client::protocol::Launch;
use client::protocol::VERSION;
use codex_freebsd_sandbox_client as client;

fn main() -> Result<()> {
    let args = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| anyhow::anyhow!("sandbox arguments must be UTF-8"))
        })
        .collect::<Result<Vec<_>>>()?;
    match args.as_slice() {
        [command] if command == "capabilities" => {
            let status = client::capabilities()?;
            client::validate_capabilities(&status)?;
            println!("{}", serde_json::to_string(&status)?);
        }
        [command, flag, permissions, separator, program, rest @ ..]
            if command == "run" && flag == "--permissions-json" && separator == "--" =>
        {
            ensure!(
                Path::new(program).is_absolute(),
                "sandbox executable must be absolute"
            );
            let permissions =
                serde_json::from_str(permissions).context("invalid sandbox permissions JSON")?;
            let status = client::capabilities()?;
            client::validate_capabilities(&status)?;
            let cwd = std::env::current_dir()?;
            let env = std::env::vars_os()
                .map(|(key, value)| {
                    Ok((
                        key.into_string()
                            .map_err(|_| anyhow::anyhow!("sandbox environment must be UTF-8"))?,
                        value
                            .into_string()
                            .map_err(|_| anyhow::anyhow!("sandbox environment must be UTF-8"))?,
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            let argv = std::iter::once(program.clone())
                .chain(rest.iter().cloned())
                .collect();
            let code = client::run(Launch {
                version: VERSION,
                argv,
                policy_cwd: cwd.clone(),
                cwd,
                permissions,
                env,
                terminal: None,
            })?;
            std::process::exit(code);
        }
        _ => anyhow::bail!(
            "usage: codex-freebsd-sandbox-client capabilities | run --permissions-json JSON -- /absolute/program [args...]"
        ),
    }
    Ok(())
}
