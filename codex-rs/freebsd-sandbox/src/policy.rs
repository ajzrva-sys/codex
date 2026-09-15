use anyhow::Result;
use anyhow::ensure;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

pub(crate) const RUNTIME_ROOTS: &[&str] = &[
    "/bin",
    "/sbin",
    "/lib",
    "/libexec",
    "/usr/bin",
    "/usr/sbin",
    "/usr/lib",
    "/usr/libexec",
    "/usr/include",
    "/usr/share",
    "/usr/local/bin",
    "/usr/local/sbin",
    "/usr/local/lib",
    "/usr/local/libexec",
    "/usr/local/include",
    "/usr/local/share",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Access {
    Read,
    Write,
    Deny,
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "freebsd"), allow(dead_code))]
pub(crate) struct Plan {
    pub roots: BTreeMap<PathBuf, Access>,
    pub network: NetworkSandboxPolicy,
    pub protected_metadata: BTreeSet<PathBuf>,
}

pub(crate) fn absolute(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute(),
        "sandbox path must be absolute: {}",
        path.display()
    );
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        "sandbox path must not contain parent components: {}",
        path.display()
    );
    ensure!(path.to_str().is_some(), "sandbox paths must be UTF-8");
    Ok(())
}

pub(crate) fn compile(permissions: &PermissionProfile, cwd: &Path) -> Result<Plan> {
    absolute(cwd)?;
    let (mut fs, network) = permissions.to_runtime_permissions();
    ensure!(
        !fs.has_full_disk_read_access() && !fs.has_full_disk_write_access(),
        "FreeBSD sandbox requires explicit filesystem roots; select the freebsd-workspace permission profile"
    );
    ensure!(
        !fs.entries
            .iter()
            .any(|entry| matches!(entry.path, FileSystemPath::GlobPattern { .. })),
        "FreeBSD sandbox does not support filesystem glob policies"
    );
    for entry in &mut fs.entries {
        if let FileSystemPath::Special { value } = &mut entry.path {
            ensure!(
                !matches!(value, FileSystemSpecialPath::Unknown { .. }),
                "FreeBSD sandbox does not support unknown filesystem tokens"
            );
            if matches!(value, FileSystemSpecialPath::Tmpdir) {
                // Commands always receive a private /tmp and TMPDIR=/tmp. Do
                // not resolve this token using the privileged daemon's env.
                *value = FileSystemSpecialPath::SlashTmp;
            }
        }
    }
    // Keep logical paths intact. Canonicalizing a grant before the daemon
    // pins it would let a substituted symlink expand its authority.
    let mut explicit: BTreeMap<PathBuf, Access> = BTreeMap::new();
    for entry in &fs.entries {
        let path = match &entry.path {
            FileSystemPath::Path { path } => path.to_abs_path()?.to_path_buf(),
            FileSystemPath::GlobPattern { .. } => anyhow::bail!("filesystem globs are unsupported"),
            FileSystemPath::Special { value } => match value {
                FileSystemSpecialPath::ProjectRoots { subpath } => subpath
                    .as_ref()
                    .map_or_else(|| cwd.to_path_buf(), |subpath| cwd.join(subpath)),
                FileSystemSpecialPath::Tmpdir | FileSystemSpecialPath::SlashTmp => {
                    PathBuf::from("/tmp")
                }
                FileSystemSpecialPath::Minimal => continue,
                FileSystemSpecialPath::Root => {
                    anyhow::bail!("FreeBSD sandbox cannot expose the host root")
                }
                FileSystemSpecialPath::Unknown { .. } => anyhow::bail!("unknown filesystem token"),
            },
        };
        absolute(&path)?;
        let access = match entry.access {
            FileSystemAccessMode::Read => Access::Read,
            FileSystemAccessMode::Write => Access::Write,
            FileSystemAccessMode::Deny => Access::Deny,
        };
        // Existing policy precedence for equally specific rules: deny > write > read.
        explicit
            .entry(path)
            .and_modify(|previous| *previous = (*previous).max(access))
            .or_insert(access);
    }
    let mut roots = BTreeMap::new();
    if fs.include_platform_defaults() {
        for root in RUNTIME_ROOTS {
            let root = PathBuf::from(root);
            // Platform defaults never reopen an explicitly denied subtree or
            // narrow an explicitly writable one.
            if !explicit.keys().any(|parent| root.starts_with(parent)) {
                roots.insert(root, Access::Read);
            }
        }
    }
    let mut protected_metadata = BTreeSet::new();
    for root in fs.get_writable_roots_with_cwd_preserving_mutable_paths(cwd) {
        if matches!(root.root.as_path().to_str(), Some("/tmp" | "/var/tmp")) {
            continue; // Private temporary roots contain no host metadata.
        }
        for protected in root.read_only_subpaths {
            // Linked Git metadata outside all writable roots needs no write
            // carveout, and must not become an implicit additional read grant.
            if explicit.iter().any(|(parent, access)| {
                *access == Access::Write && protected.as_path().starts_with(parent)
            }) {
                roots.insert(protected.to_path_buf(), Access::Read);
                protected_metadata.insert(protected.to_path_buf());
            }
        }
        for name in root.protected_metadata_names {
            let path = root.root.join(name).to_path_buf();
            roots.insert(path.clone(), Access::Read);
            protected_metadata.insert(path);
        }
    }
    for path in explicit.keys() {
        protected_metadata.remove(path);
    }
    roots.extend(explicit);
    for root in roots.keys() {
        absolute(root)?;
        ensure!(
            root != Path::new("/"),
            "FreeBSD sandbox cannot expose the host root"
        );
        let service = Path::new("/var/run/codex-freebsd-sandbox");
        ensure!(
            !root.starts_with(service) && !service.starts_with(root),
            "sandbox service paths cannot be exposed"
        );
        ensure!(
            !root.starts_with("/dev") && !root.starts_with("/proc"),
            "host device/process filesystems cannot be exposed"
        );
    }
    Ok(Plan {
        roots,
        network,
        protected_metadata,
    })
}

#[cfg(all(test, unix))]
#[path = "policy_tests.rs"]
mod tests;
