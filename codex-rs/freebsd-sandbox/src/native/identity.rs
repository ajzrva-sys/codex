use super::sys;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use std::ffi::CStr;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone)]
pub(super) struct Identity {
    pub uid: u32,
    pub gid: u32,
    pub groups: Vec<u32>,
    pub name: String,
    pub home: PathBuf,
}

impl Identity {
    pub fn peer(stream: &UnixStream) -> Result<Self> {
        let mut credentials: libc::xucred = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
        sys::cvt(unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                0,
                libc::LOCAL_PEERCRED,
                (&mut credentials as *mut libc::xucred).cast(),
                &mut size,
            )
        })?;
        ensure!(
            credentials.cr_version == 0
                && credentials.cr_ngroups > 0
                && credentials.cr_ngroups as usize <= credentials.cr_groups.len(),
            "invalid peer credentials"
        );
        ensure!(
            credentials.cr_uid != 0,
            "sandbox workloads must run as an ordinary user"
        );
        let groups = credentials.cr_groups[..credentials.cr_ngroups as usize].to_vec();
        let record = unsafe { libc::getpwuid(credentials.cr_uid) };
        ensure!(!record.is_null(), "unknown sandbox user");
        let record = unsafe { &*record };
        let name = unsafe { CStr::from_ptr(record.pw_name) }
            .to_str()?
            .to_string();
        let home = PathBuf::from(unsafe { CStr::from_ptr(record.pw_dir) }.to_str()?);
        crate::policy::absolute(&home)?;
        ensure!(
            home != Path::new("/"),
            "sandbox user must have a private home"
        );
        Ok(Self {
            uid: credentials.cr_uid,
            gid: groups[0],
            groups,
            name,
            home,
        })
    }

    /// Run filesystem preparation with the peer's effective credentials. This
    /// is only called in a single-threaded, forked setup worker.
    pub fn as_user<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        sys::cvt(unsafe { libc::setgroups(self.groups.len() as i32, self.groups.as_ptr()) })?;
        sys::cvt(unsafe { libc::setegid(self.gid) })?;
        sys::cvt(unsafe { libc::seteuid(self.uid) })?;
        let result = operation();
        // Restoration failure is fatal: never continue with uncertain credentials.
        if unsafe { libc::seteuid(0) } != 0
            || unsafe { libc::setegid(0) } != 0
            || unsafe { libc::setgroups(0, std::ptr::null()) } != 0
        {
            unsafe { libc::_exit(125) };
        }
        result
    }

    pub fn open(&self, path: &Path) -> Result<File> {
        self.as_user(|| sys::beneath(&File::open("/")?, path, /*create*/ None))
            .with_context(|| format!("user cannot expose {}", path.display()))
    }

    pub fn drop_permanently(&self) -> std::io::Result<()> {
        sys::cvt(unsafe { libc::setgroups(self.groups.len() as i32, self.groups.as_ptr()) })?;
        sys::cvt(unsafe { libc::setgid(self.gid) })?;
        sys::cvt(unsafe { libc::setuid(self.uid) })?;
        Ok(())
    }
}
