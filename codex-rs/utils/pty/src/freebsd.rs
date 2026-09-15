use std::io;
use std::os::fd::RawFd;

/// Keep explicitly inherited descriptors and mark the rest close-on-exec.
///
/// FreeBSD's default devfs only exposes descriptors 0, 1 and 2 in /dev/fd.
/// close_range works without an fdescfs mount and avoids allocating after fork.
/// CLOEXEC keeps Rust's exec-error pipe usable until exec succeeds.
pub(crate) fn set_cloexec_except(preserved_fds: &[RawFd]) -> io::Result<()> {
    let mut first = libc::STDERR_FILENO as libc::c_uint + 1;
    loop {
        // The caller need not sort or deduplicate its descriptor list.
        let next_preserved = preserved_fds
            .iter()
            .filter_map(|&fd| libc::c_uint::try_from(fd).ok())
            .filter(|&fd| fd >= first)
            .min();
        let last = next_preserved.map_or(libc::c_uint::MAX, |fd| fd - 1);
        if first <= last {
            // SAFETY: this only sets descriptor flags in the forked child.
            let result =
                unsafe { libc::close_range(first, last, libc::CLOSE_RANGE_CLOEXEC as libc::c_int) };
            if result == -1 {
                return Err(io::Error::last_os_error());
            }
        }
        match next_preserved {
            Some(fd) => first = fd + 1,
            None => return Ok(()),
        }
    }
}
