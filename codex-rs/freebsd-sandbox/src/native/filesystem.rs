use super::file_views;
use super::identity::Identity;
use super::sys;
use crate::policy::Access;
use crate::policy::Plan;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use std::collections::BTreeMap;
use std::fs;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

pub(super) const DEVICE_RULESET: u32 = 62001;

pub(super) struct View {
    pub path: PathBuf,
    pub root: PathBuf,
    pub name: String,
    pub jail: Option<i32>,
    identity: Identity,
    _lock: File,
}

impl View {
    pub fn new(state: &Path, identity: &Identity) -> Result<Self> {
        let path = tempfile::Builder::new()
            .prefix("job-")
            .tempdir_in(state)?
            .keep();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        let lock = File::create(path.join("lock"))?;
        sys::cvt(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) })?;
        let name = format!(
            "codex_{}",
            path.file_name()
                .context("job name")?
                .to_str()
                .context("job name encoding")?
        );
        fs::write(
            path.join("owner.json"),
            serde_json::to_vec(&(1u32, &name, identity.uid))?,
        )?;
        fs::create_dir(path.join("root"))?;
        fs::create_dir(path.join("fds"))?;
        Ok(Self {
            root: path.join("root"),
            path,
            name,
            jail: None,
            identity: identity.clone(),
            _lock: lock,
        })
    }

    pub fn populate(&mut self, plan: &Plan) -> Result<()> {
        let _mount_guard = sys::mount_guard()?;
        // Pin sources with the peer's credentials before creating any views.
        let mut sources: Vec<file_views::Source> = Vec::new();
        for (path, access) in &plan.roots {
            ensure!(
                !super::daemon::STOP.load(std::sync::atomic::Ordering::Relaxed),
                "sandbox service is stopping"
            );
            // Policy metadata carveouts apply to directories. Decide from the
            // pinned source, so substituting a directory for a file cannot
            // suppress its protections between policy translation and setup.
            if plan.protected_metadata.contains(path)
                && sources.iter().any(|(parent, _, source)| {
                    path != parent
                        && path.starts_with(parent)
                        && source
                            .as_ref()
                            .is_some_and(|file| file.metadata().is_ok_and(|m| m.is_file()))
                })
            {
                continue;
            }
            if path == Path::new("/tmp") || path == Path::new("/var/tmp") {
                match access {
                    Access::Write => continue,
                    Access::Read => {
                        let private = self.path.join(format!("private-tmp-{}", sources.len()));
                        fs::create_dir(&private)?;
                        sources.push((path.clone(), *access, Some(File::open(private)?)));
                        continue;
                    }
                    Access::Deny => {}
                }
            }
            let in_writable_root = plan
                .roots
                .iter()
                .filter(|(parent, _)| path != *parent && path.starts_with(parent))
                .max_by_key(|(parent, _)| parent.components().count())
                .is_some_and(|(_, mode)| *mode == Access::Write);
            if *access == Access::Deny && in_writable_root && !path.try_exists()? {
                self.identity
                    .as_user(|| sys::beneath(&File::open("/")?, path, Some(0o755)))?;
            }
            let source = if *access == Access::Deny {
                None
            } else {
                match self.identity.open(path) {
                    Ok(file) => Some(file),
                    Err(error)
                        if path
                            .symlink_metadata()
                            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                    {
                        if *access == Access::Read && in_writable_root {
                            let file = self
                                .identity
                                .as_user(|| sys::beneath(&File::open("/")?, path, Some(0o755)))?;
                            Some(file)
                        } else if *access == Access::Read {
                            // An absent readable root grants no access.
                            continue;
                        } else {
                            return Err(error);
                        }
                    }
                    Err(error) => return Err(error),
                }
            };
            sources.push((path.clone(), *access, source));
        }
        sources.sort_by_key(|(path, _, _)| path.components().count());
        for (path, access, source) in &sources {
            if *access == Access::Read
                && (path == Path::new("/tmp") || path == Path::new("/var/tmp"))
            {
                let private = source.as_ref().context("private temporary root")?;
                for (child, _, child_source) in &sources {
                    if child != path && child.starts_with(path) {
                        let relative = child.strip_prefix(path)?;
                        if child_source.as_ref().is_some_and(|file| {
                            file.metadata().is_ok_and(|metadata| metadata.is_file())
                        }) {
                            sys::create_file(private, relative)?;
                        } else {
                            sys::beneath(private, relative, Some(0o755))?;
                        }
                    }
                }
            }
        }
        sys::mount(
            "tmpfs",
            &self.root,
            &[("mode", "0755".into())],
            libc::MNT_NOSUID,
        )?;
        let root = File::open(&self.root)?;
        sys::mount(
            "fdescfs",
            &self.path.join("fds"),
            &[("nodup", String::new())],
            libc::MNT_NOSUID,
        )?;
        for directory in [
            Path::new("/tmp"),
            Path::new("/var/tmp"),
            self.identity.home.as_path(),
        ] {
            sys::beneath(&root, directory, Some(0o755))?;
            sys::mount(
                "tmpfs",
                &self.root.join(directory.strip_prefix("/")?),
                &[
                    ("uid", self.identity.uid.to_string()),
                    ("gid", self.identity.gid.to_string()),
                    ("mode", "0700".into()),
                ],
                libc::MNT_NOSUID,
            )?;
        }
        self.etc(&root, plan)?;
        fs::create_dir(self.path.join("sources"))?;
        let private = file_views::prepare(
            &mut sources,
            &self.identity,
            &self.path,
            &self.path.join("fds"),
        )?;
        sources.sort_by_key(|(path, _, _)| path.components().count());
        let modes: BTreeMap<_, _> = sources
            .iter()
            .map(|(path, access, _)| (path.clone(), *access))
            .collect();
        let granted: Vec<_> = sources
            .iter()
            .filter(|&(_path, access, _source)| *access != Access::Deny)
            .map(|(path, _access, source)| {
                (
                    path.clone(),
                    source
                        .as_ref()
                        .is_some_and(|file| file.metadata().is_ok_and(|m| m.is_dir())),
                )
            })
            .collect();
        for (index, (path, access, source)) in sources.into_iter().enumerate() {
            ensure!(
                !super::daemon::STOP.load(std::sync::atomic::Ordering::Relaxed),
                "sandbox service is stopping"
            );
            let existing = sys::beneath(&root, &path, /*create*/ None).ok();
            let directory = source
                .as_ref()
                .or(existing.as_ref())
                .is_none_or(|f| f.metadata().is_ok_and(|m| m.is_dir()));
            let target = if directory {
                sys::beneath(&root, &path, Some(0o755))?
            } else {
                sys::create_file(&root, &path)?
            };
            if let Some(source) = source {
                if !directory {
                    let ancestor = path
                        .ancestors()
                        .skip(1)
                        .find_map(|parent| modes.get(parent).map(|mode| (parent, *mode)));
                    if ancestor
                        .is_some_and(|(parent, mode)| mode == access && !private.contains(parent))
                    {
                        continue;
                    }
                    let source = if access == Access::Read
                        && ancestor.is_some_and(|(_, mode)| mode == Access::Write)
                    {
                        file_views::snapshot(
                            source,
                            &self.path.join(format!("file-{index}")),
                            &self.identity,
                        )?
                    } else {
                        source
                    };
                    sys::bind(&source, &target, &self.path.join("fds"), access)
                        .with_context(|| format!("expose file {} as {access:?}", path.display()))?;
                    continue;
                }
                // Interpose an empty, service-owned vnode. Mounting a source
                // back over an alias of itself creates recursive vnode locks
                // during unmount on FreeBSD 15.1 (FreeBSD PR 297174).
                // The intermediate view must have a DIFFERENT lower vnode.
                let bridge = self.path.join("sources").join(index.to_string());
                if directory {
                    fs::create_dir(&bridge)?;
                } else {
                    File::create(&bridge)?;
                }
                let stage_id = sys::bind(
                    &File::open(&bridge)?,
                    &target,
                    &self.path.join("fds"),
                    Access::Read,
                )
                .with_context(|| format!("stage {}", path.display()))?;
                let staged = sys::beneath(&root, &path, /*create*/ None)?;
                // A host-side rename must not redirect the second mount to a
                // different destination after the first descriptor-pinned mount.
                ensure!(
                    sys::mount_id(&staged)? == stage_id,
                    "mount destination changed during setup"
                );
                sys::bind(&source, &staged, &self.path.join("fds"), access)
                    .with_context(|| format!("expose {} as {access:?}", path.display()))?;
            } else {
                let mask = self.path.join(format!("mask-{index}"));
                let descendants: Vec<_> = granted
                    .iter()
                    .filter(|(grant, _)| grant != &path && grant.starts_with(&path))
                    .collect();
                if directory {
                    fs::create_dir(&mask)?;
                    let mask_root = File::open(&mask)?;
                    for (grant, is_directory) in &descendants {
                        let relative = grant.strip_prefix(&path)?;
                        if *is_directory {
                            sys::beneath(&mask_root, relative, Some(0o755))?;
                        } else {
                            sys::create_file(&mask_root, relative)?;
                        }
                    }
                } else {
                    ensure!(
                        descendants.is_empty(),
                        "a denied file cannot contain granted paths"
                    );
                    File::create(&mask)?;
                }
                fs::set_permissions(
                    &mask,
                    fs::Permissions::from_mode(if descendants.is_empty() { 0 } else { 0o555 }),
                )?;
                sys::bind(
                    &File::open(&mask)?,
                    &target,
                    &self.path.join("fds"),
                    Access::Read,
                )?;
            }
        }
        let dev = sys::beneath(&root, Path::new("/dev"), Some(0o755))?;
        sys::mount(
            "devfs",
            &self.path.join("fds").join(dev.as_raw_fd().to_string()),
            &[("ruleset", DEVICE_RULESET.to_string())],
            libc::MNT_NOSUID,
        )?;
        // Synthetic parents are root-owned and not writable by the workload.
        // Runtime and protected source views additionally use read-only mounts.
        Ok(())
    }

    fn etc(&self, root: &File, plan: &Plan) -> Result<()> {
        sys::beneath(root, Path::new("/etc"), Some(0o755))?;
        let etc = self.root.join("etc");
        let Identity {
            uid,
            gid,
            name,
            home,
            ..
        } = &self.identity;
        ensure!(!name.contains([':', '\n']), "invalid account name");
        fs::write(
            etc.join("master.passwd"),
            format!(
                "{name}:*:{uid}:{gid}::0:0:Sandbox:{}:/bin/sh\n",
                home.display()
            ),
        )?;
        let status = Command::new("/usr/sbin/pwd_mkdb")
            .env_clear()
            .args(["-p", "-d"])
            .arg(&etc)
            .arg(etc.join("master.passwd"))
            .status()?;
        ensure!(status.success(), "cannot create sandbox account database");
        fs::write(etc.join("group"), format!("{name}:*:{gid}:\n"))?;
        fs::write(
            etc.join("nsswitch.conf"),
            "passwd: files\ngroup: files\nhosts: files dns\n",
        )?;
        let mut files = vec![
            "/etc/hosts",
            "/etc/localtime",
            "/var/run/ld-elf.so.hints",
            "/var/run/ld-elf32.so.hints",
        ];
        if plan.network.is_enabled() {
            files.push("/etc/resolv.conf");
        }
        for path in files {
            if let Ok(bytes) = self.identity.as_user(|| Ok(fs::read(path)?)) {
                let destination = self.root.join(path.trim_start_matches('/'));
                fs::create_dir_all(destination.parent().context("runtime file parent")?)?;
                fs::write(destination, bytes)?;
            }
        }
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        if let Some(jid) = self.jail {
            let result = unsafe { libc::jail_remove(jid) };
            if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL) {
                anyhow::bail!(
                    "cannot remove sandbox jail: {}",
                    std::io::Error::last_os_error()
                );
            }
            self.jail = None;
        }
        Ok(())
    }

    pub fn cleanup(&mut self) -> Result<()> {
        self.stop()?;
        cleanup_mounts(&self.path)?;
        // Keep empty protected-path mount targets in the workspace. Another
        // live job may still protect the same inode through its own nullfs
        // view; removing its host directory here could reopen the logical path.
        // Only service-owned scaffolding remains once EVERY mount is gone.
        fs::remove_dir_all(&self.path)?;
        Ok(())
    }
}

impl Drop for View {
    fn drop(&mut self) {
        if self.path.exists()
            && let Err(error) = self.cleanup()
        {
            eprintln!("quarantined sandbox {}: {error:#}", self.path.display());
        }
    }
}

pub(super) fn cleanup_mounts(path: &Path) -> Result<()> {
    let _mount_guard = sys::mount_guard()?;
    for mount in sys::mounts_beneath(path)? {
        // A busy child mount stops cleanup; never remove directories through it.
        for attempt in 0..20 {
            match sys::unmount(&mount) {
                Ok(()) => break,
                Err(error)
                    if attempt < 19
                        && error
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|error| error.raw_os_error() == Some(libc::EBUSY)) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        }
    }
    ensure!(
        sys::mounts_beneath(path)?.is_empty(),
        "sandbox still has mounted filesystems"
    );
    Ok(())
}

pub(super) fn prepare_device_rules() -> Result<()> {
    let rules = [
        "hide",
        "path null unhide",
        "path zero unhide",
        "path random unhide",
        "path urandom unhide",
        "path fd unhide",
        "path fd/* unhide",
        "path stdin unhide",
        "path stdout unhide",
        "path stderr unhide",
    ];
    let number = DEVICE_RULESET.to_string();
    let output = Command::new("/sbin/devfs")
        .env_clear()
        .args(["rule", "-s", &number, "show"])
        .output()?;
    ensure!(output.status.success(), "cannot inspect devfs rules");
    let expected = rules
        .iter()
        .enumerate()
        .map(|(i, rule)| format!("{} {rule}\n", (i + 1) * 100))
        .collect::<String>();
    let actual = String::from_utf8(output.stdout)?;
    if !actual.trim().is_empty() {
        ensure!(
            actual == expected,
            "devfs ruleset {number} is already used by another service"
        );
        return Ok(());
    }
    for (index, rule) in rules.iter().enumerate() {
        let mut command = Command::new("/sbin/devfs");
        command.env_clear().args([
            "rule",
            "-s",
            &number,
            "add",
            &((index + 1) * 100).to_string(),
        ]);
        command.args(rule.split_whitespace());
        ensure!(
            command.status()?.success(),
            "cannot configure sandbox devfs"
        );
    }
    Ok(())
}
