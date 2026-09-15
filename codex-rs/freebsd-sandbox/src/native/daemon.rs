use super::filesystem;
use super::identity::Identity;
use super::job;
use super::sys;
use crate::protocol::Request;
use crate::protocol::Response;
use crate::protocol::VERSION;
use crate::protocol::receive;
use crate::protocol::send;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use clap::Parser;
use serde::Deserialize;
use std::fs;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub(super) static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stopping(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/usr/local/etc/codex-freebsd-sandbox.json")]
    config: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    allowed_uids: Vec<u32>,
}

pub(crate) fn run() -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "sandbox service must run as root"
    );
    let args = Args::parse();
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&args.config)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.uid() == 0 && metadata.mode() & 0o022 == 0 && metadata.is_file(),
        "service configuration must be root-owned and not writable by other users"
    );
    let config: Config = serde_json::from_reader(file)?;
    ensure!(
        !config.allowed_uids.is_empty() && !config.allowed_uids.contains(&0),
        "configure ordinary user UIDs only"
    );
    let base = Path::new("/var/run/codex-freebsd-sandbox");
    fs::create_dir_all(base)?;
    let metadata = base.symlink_metadata()?;
    ensure!(
        metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
        "unsafe sandbox service directory"
    );
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(base.join("lock"))?;
    sys::cvt(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) })
        .context("sandbox service already running")?;
    let state = base.join("jobs");
    fs::create_dir_all(&state)?;
    let metadata = state.symlink_metadata()?;
    ensure!(
        metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
        "unsafe sandbox job directory"
    );
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
    filesystem::prepare_device_rules()?;
    recover(&state)?;
    if Path::new(crate::SOCKET_PATH).exists() {
        fs::remove_file(crate::SOCKET_PATH)?;
    }
    let listener = UnixListener::bind(crate::SOCKET_PATH)?;
    fs::set_permissions(crate::SOCKET_PATH, fs::Permissions::from_mode(0o666))?;
    listener.set_nonblocking(true)?;
    unsafe {
        libc::signal(libc::SIGTERM, stopping as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stopping as *const () as libc::sighandler_t);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let mut children = Vec::new();
    let mut last_recovery = std::time::Instant::now();
    eprintln!("FreeBSD sandbox service ready (protocol {VERSION})");
    while !STOP.load(Ordering::Relaxed) {
        children
            .retain(|pid| unsafe { libc::waitpid(*pid, std::ptr::null_mut(), libc::WNOHANG) } == 0);
        if last_recovery.elapsed() > Duration::from_secs(5) {
            recover(&state)?;
            last_recovery = std::time::Instant::now();
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let identity = match Identity::peer(&stream) {
                    Ok(identity) if config.allowed_uids.contains(&identity.uid) => identity,
                    _ => {
                        let _ = send(
                            &mut stream,
                            &Response::Error {
                                message: "user is not authorized for the sandbox service".into(),
                            },
                        );
                        continue;
                    }
                };
                if children.len() >= 128 {
                    let _ = send(
                        &mut stream,
                        &Response::Error {
                            message: "too many sandbox jobs".into(),
                        },
                    );
                    continue;
                }
                let parent = unsafe { libc::getpid() };
                let pid = sys::cvt(unsafe { libc::fork() })?;
                if pid == 0 {
                    unsafe {
                        libc::close(listener.as_raw_fd());
                        libc::close(lock.as_raw_fd());
                    }
                    let mut signal = libc::SIGTERM;
                    let parent_death = sys::cvt(unsafe {
                        libc::procctl(
                            libc::P_PID,
                            0,
                            libc::PROC_PDEATHSIG_CTL,
                            (&mut signal as *mut i32).cast(),
                        )
                    });
                    if parent_death.is_err() || unsafe { libc::getppid() } != parent {
                        unsafe {
                            libc::_exit(125);
                        }
                    }
                    let result = (|| -> Result<()> {
                        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
                        let request = receive(&mut stream)?;
                        stream.set_read_timeout(None)?;
                        match request {
                            Request::Probe { version } => {
                                ensure!(version == VERSION, "incompatible sandbox protocol");
                                send(
                                    &mut stream,
                                    &Response::Status {
                                        version: VERSION,
                                        description: format!(
                                            "FreeBSD jail service: protocol {VERSION}; explicit filesystem roots; network blocked or unrestricted; managed proxies unsupported"
                                        ),
                                    },
                                )?;
                            }
                            Request::Launch(request) => {
                                job::run(stream.try_clone()?, *request, identity, &state)?
                            }
                            _ => anyhow::bail!("first sandbox request must be launch or probe"),
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                        let _ = send(
                            &mut stream,
                            &Response::Error {
                                message: format!("{error:#}"),
                            },
                        );
                    }
                    unsafe {
                        libc::_exit(0);
                    }
                }
                children.push(pid);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(error) => return Err(error.into()),
        }
    }
    for pid in &children {
        unsafe {
            libc::kill(*pid, libc::SIGTERM);
        }
    }
    for pid in children {
        unsafe {
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    }
    recover(&state)?;
    fs::remove_file(crate::SOCKET_PATH)?;
    Ok(())
}

fn recover(state: &Path) -> Result<()> {
    for entry in fs::read_dir(state)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| name.starts_with("job-")) else {
            continue;
        };
        let Ok(lock) = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path.join("lock"))
        else {
            continue;
        };
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            continue;
        }
        let result = (|| -> Result<()> {
            let (version, jail, uid): (u32, String, u32) =
                serde_json::from_slice(&fs::read(path.join("owner.json"))?)?;
            ensure!(
                version == 1 && uid != 0 && jail == format!("codex_{name}"),
                "invalid sandbox recovery journal"
            );
            job::remove_named_jail(&jail, &path.join("root"))?;
            filesystem::cleanup_mounts(&path)?;
            fs::remove_dir_all(&path)?;
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!(
                "sandbox recovery left {} quarantined: {error:#}",
                path.display()
            );
        }
    }
    Ok(())
}
