use crate::policy::Access;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use std::ffi::CStr;
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

pub(super) fn cvt(value: libc::c_int) -> io::Result<libc::c_int> {
    if value == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

pub(super) fn cpath(path: &Path) -> Result<CString> {
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

/// Serialize service mount topology changes; command execution remains parallel.
/// In particular, do not race a nullfs unmount against an overlapping mount.
pub(super) fn mount_guard() -> Result<File> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open("/var/run/codex-freebsd-sandbox/mount.lock")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o077 == 0,
        "unsafe mount lock"
    );
    cvt(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) })?;
    Ok(file)
}

/// Resolve each component beneath a pinned directory; never follow a symlink.
pub(super) fn beneath(root: &File, path: &Path, create: Option<libc::mode_t>) -> Result<File> {
    let mut directory = root.try_clone()?;
    let components: Vec<_> = path
        .components()
        .filter(|p| *p != Component::RootDir)
        .collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            anyhow::bail!("invalid path component");
        };
        let name = CString::new(name.as_bytes())?;
        let last = index + 1 == components.len();
        if let Some(mode) = create
            && can_create_scaffold(&directory)?
        {
            // SAFETY: the parent descriptor and nul-terminated name are valid.
            let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), mode) };
            if result == -1 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                return Err(io::Error::last_os_error()).context("create mount target");
            }
        }
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if !last || create.is_some() {
                libc::O_DIRECTORY
            } else {
                0
            };
        // SAFETY: openat returns a newly owned descriptor.
        let fd = cvt(unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) })
            .with_context(|| format!("open without symlinks: {}", path.display()))?;
        directory = unsafe { File::from_raw_fd(fd) };
    }
    let metadata = directory.metadata()?;
    ensure!(
        metadata.is_dir() || metadata.is_file(),
        "only ordinary files and directories can be exposed"
    );
    Ok(directory)
}

pub(super) fn create_file(root: &File, path: &Path) -> Result<File> {
    let parent = beneath(
        root,
        path.parent().context("file has no parent")?,
        Some(0o755),
    )?;
    let name = CString::new(path.file_name().context("file has no name")?.as_bytes())?;
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK
        | if can_create_scaffold(&parent)? {
            libc::O_CREAT
        } else {
            0
        };
    let fd = cvt(unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, 0o644) })?;
    let file = unsafe { File::from_raw_fd(fd) };
    ensure!(
        file.metadata()?.is_file(),
        "mount target must be an ordinary file"
    );
    Ok(file)
}

fn can_create_scaffold(parent: &File) -> Result<bool> {
    if unsafe { libc::geteuid() } != 0 {
        return Ok(true);
    }
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    cvt(unsafe { libc::fstatfs(parent.as_raw_fd(), &mut stat) })?;
    // Creating a missing target through an exposed host view must happen with
    // the caller's credentials. Root may only build the private scaffolding.
    Ok(unsafe { CStr::from_ptr(stat.f_fstypename.as_ptr()) }.to_bytes() != b"nullfs")
}

pub(super) fn mount(
    fstype: &str,
    target: &Path,
    options: &[(&str, String)],
    flags: libc::c_int,
) -> Result<()> {
    let mut strings = vec![
        CString::new("fstype")?,
        CString::new(fstype)?,
        CString::new("fspath")?,
        cpath(target)?,
    ];
    for (name, value) in options {
        strings.push(CString::new(*name)?);
        strings.push(CString::new(value.as_str())?);
    }
    let mut iov: Vec<libc::iovec> = strings
        .iter()
        .map(|s| libc::iovec {
            iov_base: s.as_ptr().cast_mut().cast(),
            iov_len: s.as_bytes_with_nul().len(),
        })
        .collect();
    cvt(unsafe { libc::nmount(iov.as_mut_ptr(), iov.len() as u32, flags) })
        .with_context(|| format!("mount {fstype} on {}", target.display()))?;
    Ok(())
}

pub(super) fn bind(
    source: &File,
    target: &File,
    fd_root: &Path,
    access: Access,
) -> Result<[i32; 2]> {
    let source_path = fd_root.join(source.as_raw_fd().to_string());
    let target_path = fd_root.join(target.as_raw_fd().to_string());
    let job_root = fd_root.parent().context("descriptor filesystem parent")?;
    let before = mounts_beneath(job_root)?;
    mount(
        "nullfs",
        &target_path,
        &[
            ("target", source_path.to_string_lossy().into_owned()),
            ("nounixbypass", String::new()),
        ],
        libc::MNT_NOSUID
            | if access == Access::Read {
                libc::MNT_RDONLY
            } else {
                0
            },
    )?;
    // FreeBSD records the resolved mountpoint name, which may differ from
    // the fdescfs path passed to nmount. Identify the new mount by FSID.
    // The caller holds mount_guard across this entire operation.
    let mut added = mounts_beneath(job_root)?
        .into_iter()
        .filter(|mount| !before.iter().any(|previous| previous.id == mount.id));
    let id = added.next().context("new descriptor mount is missing")?.id;
    ensure!(
        added.next().is_none(),
        "mount topology changed during setup"
    );
    Ok(id)
}

pub(super) fn mount_id(file: &File) -> Result<[i32; 2]> {
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    cvt(unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) })?;
    Ok(unsafe { std::mem::transmute::<libc::fsid_t, [i32; 2]>(stat.f_fsid) })
}

pub(super) struct Mounted {
    pub target: PathBuf,
    pub id: [i32; 2],
}

pub(super) fn mounts_beneath(root: &Path) -> Result<Vec<Mounted>> {
    let mut entries = std::ptr::null_mut();
    let count = unsafe { libc::getmntinfo(&mut entries, libc::MNT_NOWAIT) };
    ensure!(count > 0, "cannot enumerate mounts");
    let mut mounts = Vec::new();
    for entry in unsafe { std::slice::from_raw_parts(entries, count as usize) } {
        let name = unsafe { CStr::from_ptr(entry.f_mntonname.as_ptr()) }.to_str()?;
        let target = PathBuf::from(name);
        if target.starts_with(root) {
            // FreeBSD fsid_t is two 32-bit integers (the libc fields are private).
            let id = unsafe { std::mem::transmute::<libc::fsid_t, [i32; 2]>(entry.f_fsid) };
            mounts.push(Mounted { target, id });
        }
    }
    // getmntinfo preserves mount order; reverse it to remove stacked views first.
    mounts.reverse();
    Ok(mounts)
}

pub(super) fn unmount(mount: &Mounted) -> Result<()> {
    let id = CString::new(format!("FSID:{}:{}", mount.id[0], mount.id[1]))?;
    cvt(unsafe { libc::unmount(id.as_ptr(), libc::MNT_BYFSID) })
        .with_context(|| format!("unmount {}", mount.target.display()))?;
    Ok(())
}

pub(super) fn close_on_exec() -> io::Result<()> {
    cvt(unsafe { libc::close_range(3, u32::MAX, libc::CLOSE_RANGE_CLOEXEC as i32) })?;
    Ok(())
}

pub(super) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = cvt(unsafe { libc::fcntl(fd, libc::F_GETFL) })?;
    cvt(unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) })?;
    Ok(())
}
