use super::identity::Identity;
use super::sys;
use crate::policy::Access;
use anyhow::Result;
use anyhow::ensure;
use std::collections::BTreeSet;
use std::fs;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

pub(super) type Source = (PathBuf, Access, Option<File>);

/// FreeBSD cannot stack regular-file mounts. A writable file exception inside
/// a read-only directory therefore needs a private directory of mount targets.
/// Its other children retain read-only views of their pinned host sources.
pub(super) fn prepare(
    sources: &mut Vec<Source>,
    identity: &Identity,
    job: &Path,
    descriptors: &Path,
) -> Result<BTreeSet<PathBuf>> {
    let mut private: BTreeSet<_> = sources
        .iter()
        .filter(|(path, access, _)| {
            *access == Access::Read && matches!(path.to_str(), Some("/tmp" | "/var/tmp"))
        })
        .map(|(path, _, _)| path.clone())
        .collect();
    let mut parents = BTreeSet::new();
    for (path, access, source) in sources.iter() {
        if *access != Access::Write
            || !source
                .as_ref()
                .is_some_and(|file| file.metadata().is_ok_and(|m| m.is_file()))
        {
            continue;
        }
        let ancestor = sources
            .iter()
            .filter(|(parent, _, _)| path != parent && path.starts_with(parent))
            .max_by_key(|(parent, _, _)| parent.components().count());
        if let Some((ancestor, Access::Read, _)) = ancestor
            && !private.contains(ancestor)
            && let Some(parent) = path.parent()
        {
            parents.insert(parent.to_path_buf());
        }
    }
    for (index, parent) in parents.into_iter().enumerate() {
        let host = identity.open(&parent)?;
        let directory = job.join(format!("file-parent-{index}"));
        fs::create_dir(&directory)?;
        let target = File::open(&directory)?;
        // The descriptor was opened as the caller. Enumerate that pinned
        // directory, then open each child with the same caller credentials.
        for entry in fs::read_dir(descriptors.join(host.as_raw_fd().to_string()))? {
            let entry = entry?;
            let name = entry.file_name();
            let relative = Path::new(&name);
            let path = parent.join(&name);
            let destination = directory.join(&name);
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                let link = identity.as_user(|| {
                    let name = sys::cpath(relative)?;
                    let mut bytes = vec![0u8; libc::PATH_MAX as usize];
                    let count = unsafe {
                        libc::readlinkat(
                            host.as_raw_fd(),
                            name.as_ptr(),
                            bytes.as_mut_ptr().cast(),
                            bytes.len(),
                        )
                    };
                    ensure!(
                        count >= 0 && (count as usize) < bytes.len(),
                        "cannot pin directory symlink"
                    );
                    bytes.truncate(count as usize);
                    Ok(std::ffi::OsString::from_vec(bytes))
                })?;
                std::os::unix::fs::symlink(link, destination)?;
                continue;
            }
            if kind.is_dir() {
                sys::beneath(&target, relative, Some(0o755))?;
            } else {
                sys::create_file(&target, relative)?;
            }
            if sources.iter().any(|(existing, _, _)| existing == &path) {
                continue;
            }
            let source = identity.as_user(|| sys::beneath(&host, relative, /*create*/ None));
            match source {
                Ok(source) => sources.push((path, Access::Read, Some(source))),
                Err(_) => {
                    // Preserve inaccessible/special entries as opaque masks.
                    fs::set_permissions(destination, fs::Permissions::from_mode(0o0))?;
                }
            }
        }
        for (path, _, source) in sources.iter() {
            if path != &parent && path.starts_with(&parent) {
                let relative = path.strip_prefix(&parent)?;
                if sys::beneath(&target, relative, /*create*/ None).is_ok() {
                    continue;
                }
                if source
                    .as_ref()
                    .is_some_and(|file| file.metadata().is_ok_and(|m| m.is_file()))
                {
                    sys::create_file(&target, relative)?;
                } else {
                    sys::beneath(&target, relative, Some(0o755))?;
                }
            }
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555))?;
        sources.retain(|(path, _, _)| path != &parent);
        sources.push((parent.clone(), Access::Read, Some(target)));
        private.insert(parent);
    }
    Ok(private)
}

/// A read-only file inside a writable directory needs a different lower vnode
/// to avoid recursive nullfs locks. Pin its contents for this invocation.
pub(super) fn snapshot(source: File, path: &Path, identity: &Identity) -> Result<File> {
    let executable = source.metadata()?.mode() & 0o111 != 0;
    let mut output = File::create(path)?;
    std::io::copy(&mut &source, &mut output)?;
    sys::cvt(unsafe { libc::fchown(output.as_raw_fd(), identity.uid, identity.gid) })?;
    output.set_permissions(fs::Permissions::from_mode(if executable {
        0o500
    } else {
        0o400
    }))?;
    drop(output);
    Ok(File::open(path)?)
}
