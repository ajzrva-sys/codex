use super::filesystem::View;
use super::identity::Identity;
use super::relay;
use super::sys;
use crate::protocol::Launch;
use crate::protocol::Response;
use crate::protocol::VERSION;
use crate::protocol::send;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use codex_protocol::permissions::NetworkSandboxPolicy;
use std::ffi::CString;
use std::fs::File;
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

pub(super) fn run(
    mut stream: UnixStream,
    request: Launch,
    identity: Identity,
    state: &Path,
) -> Result<()> {
    ensure!(request.version == VERSION, "incompatible sandbox protocol");
    ensure!(
        !request.argv.is_empty() && request.argv.len() <= 4096,
        "invalid sandbox command"
    );
    crate::policy::absolute(&request.cwd)?;
    let permissions = serde_json::from_value(request.permissions.clone())?;
    let plan = identity.as_user(|| crate::policy::compile(&permissions, &request.policy_cwd))?;
    let mut view = View::new(state, &identity)?;
    view.populate(&plan)?;
    ensure!(
        !super::daemon::STOP.load(std::sync::atomic::Ordering::Relaxed),
        "sandbox service is stopping"
    );
    let jid = create_jail(&view, plan.network)?;
    view.jail = Some(jid);
    let mut command = Command::new(&request.argv[0]);
    command
        .args(&request.argv[1..])
        .env_clear()
        .envs(&request.env)
        .env("HOME", &identity.home)
        .env("TMPDIR", "/tmp")
        .env("USER", &identity.name)
        .env("LOGNAME", &identity.name);
    let mut master = None;
    if let Some(size) = request.terminal {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        let mut size = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        sys::cvt(unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        })?;
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        master = Some(unsafe { File::from_raw_fd(master_fd) });
        command
            .stdin(slave.try_clone()?)
            .stdout(slave.try_clone()?)
            .stderr(slave);
    } else {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }
    let cwd = sys::cpath(&request.cwd)?;
    let interactive = request.terminal.is_some();
    unsafe {
        command.pre_exec(move || {
            sys::cvt(libc::setsid())?;
            if interactive {
                sys::cvt(libc::ioctl(0, libc::TIOCSCTTY, 0))?;
            }
            sys::cvt(libc::jail_attach(jid))?;
            identity.drop_permanently()?;
            let mut enabled: libc::c_int = libc::PROC_NO_NEW_PRIVS_ENABLE;
            sys::cvt(libc::procctl(
                libc::P_PID,
                0,
                libc::PROC_NO_NEW_PRIVS_CTL,
                (&mut enabled as *mut libc::c_int).cast(),
            ))?;
            sys::cvt(libc::chdir(cwd.as_ptr()))?;
            sys::close_on_exec()?;
            Ok(())
        });
    }
    let mut child = command.spawn().context("execute jailed command")?;
    let result = send(&mut stream, &Response::Ready)
        .map_err(anyhow::Error::from)
        .and_then(|()| relay::run(&mut stream, &mut child, master, &mut view));
    view.stop()?;
    child.wait()?;
    let code = result?;
    if let Err(error) = view.cleanup() {
        // TCP references can keep a removed jail's root vnode busy. The entire
        // job is already dead and the root-owned quarantine is inaccessible to
        // clients. Recovery retries without forced unmounts or recursive deletion.
        if error
            .downcast_ref::<std::io::Error>()
            .is_none_or(|error| error.raw_os_error() != Some(libc::EBUSY))
        {
            return Err(error);
        }
        eprintln!(
            "deferred sandbox cleanup {}: {error:#}",
            view.path.display()
        );
    }
    send(&mut stream, &Response::Exit { code })?;
    Ok(())
}

fn create_jail(view: &View, network: NetworkSandboxPolicy) -> Result<i32> {
    let mut entries: Vec<(CString, Vec<u8>)> = Vec::new();
    for (name, value) in [
        ("name", view.name.as_str()),
        ("path", view.root.to_str().context("jail path encoding")?),
    ] {
        entries.push((
            CString::new(name)?,
            CString::new(value)?.into_bytes_with_nul(),
        ));
    }
    for (name, value) in [
        ("persist", 1i32),
        ("ip4", if network.is_enabled() { 2 } else { 0 }),
        ("ip6", if network.is_enabled() { 2 } else { 0 }),
        ("children.max", 0),
        ("enforce_statfs", 2),
        ("allow.mount", 0),
        ("allow.raw_sockets", 0),
        ("allow.sysvipc", 0),
        ("allow.socket_af", 0),
        ("allow.set_hostname", 0),
    ] {
        entries.push((CString::new(name)?, value.to_ne_bytes().to_vec()));
    }
    let mut iov = Vec::new();
    for (name, value) in &mut entries {
        iov.push(libc::iovec {
            iov_base: name.as_ptr().cast_mut().cast(),
            iov_len: name.as_bytes_with_nul().len(),
        });
        iov.push(libc::iovec {
            iov_base: value.as_mut_ptr().cast(),
            iov_len: value.len(),
        });
    }
    let jid =
        sys::cvt(unsafe { libc::jail_set(iov.as_mut_ptr(), iov.len() as u32, libc::JAIL_CREATE) })
            .context("create isolated jail")?;
    Ok(jid)
}

pub(super) fn remove_named_jail(name: &str, root: &Path) -> Result<()> {
    // Name is taken only from a root-owned journal and verified by the caller.
    let name = CString::new(name)?;
    let key = CString::new("name")?;
    let path_key = CString::new("path")?;
    let mut path = vec![0u8; libc::PATH_MAX as usize];
    let mut iov = [
        libc::iovec {
            iov_base: key.as_ptr().cast_mut().cast(),
            iov_len: key.as_bytes_with_nul().len(),
        },
        libc::iovec {
            iov_base: name.as_ptr().cast_mut().cast(),
            iov_len: name.as_bytes_with_nul().len(),
        },
        libc::iovec {
            iov_base: path_key.as_ptr().cast_mut().cast(),
            iov_len: path_key.as_bytes_with_nul().len(),
        },
        libc::iovec {
            iov_base: path.as_mut_ptr().cast(),
            iov_len: path.len(),
        },
    ];
    let jid = unsafe { libc::jail_get(iov.as_mut_ptr(), iov.len() as u32, 0) };
    if jid >= 0 {
        ensure!(
            std::ffi::CStr::from_bytes_until_nul(&path)? == sys::cpath(root)?.as_c_str(),
            "stale jail root does not match service journal"
        );
        sys::cvt(unsafe { libc::jail_remove(jid) })?;
    } else if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
        return Err(std::io::Error::last_os_error()).context("find stale jail");
    }
    Ok(())
}
