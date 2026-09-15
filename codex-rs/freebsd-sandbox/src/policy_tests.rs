use super::*;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use pretty_assertions::assert_eq;

fn permissions(entries: Vec<FileSystemSandboxEntry>) -> PermissionProfile {
    PermissionProfile::from_runtime_permissions(
        &FileSystemSandboxPolicy::restricted(entries),
        NetworkSandboxPolicy::Restricted,
    )
}

#[test]
fn compiles_workspace_with_runtime_and_protected_metadata() -> Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join(".git"))?;
    let profile = permissions(vec![
        FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::Minimal,
            },
            FileSystemAccessMode::Read,
        ),
        FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::ProjectRoots { subpath: None },
            },
            FileSystemAccessMode::Write,
        ),
    ]);
    let canonical = root.path().canonicalize()?;
    let plan = compile(&profile, &canonical)?;
    assert_eq!(plan.roots.get(&canonical), Some(&Access::Write));
    assert_eq!(plan.roots.get(&canonical.join(".git")), Some(&Access::Read));
    assert_eq!(
        plan.roots.get(&canonical.join(".codex")),
        Some(&Access::Read)
    );
    assert_eq!(plan.roots.get(Path::new("/usr/bin")), Some(&Access::Read));
    assert_eq!(plan.network, NetworkSandboxPolicy::Restricted);
    Ok(())
}

#[test]
fn refuses_unenforceable_globs_and_host_root() {
    let glob = permissions(vec![FileSystemSandboxEntry::new(
        FileSystemPath::GlobPattern {
            pattern: "/home/*/.ssh".into(),
        },
        FileSystemAccessMode::Deny,
    )]);
    assert!(
        compile(&glob, Path::new("/workspace"))
            .unwrap_err()
            .to_string()
            .contains("glob")
    );
    let broad = PermissionProfile::from_runtime_permissions(
        &FileSystemSandboxPolicy::read_only(),
        NetworkSandboxPolicy::Restricted,
    );
    assert!(compile(&broad, Path::new("/workspace")).is_err());
}

#[test]
fn refuses_relative_and_parent_paths() {
    assert!(absolute(Path::new("relative")).is_err());
    assert!(absolute(Path::new("/workspace/../secret")).is_err());
}

#[test]
fn explicit_parent_rules_override_runtime_defaults() -> Result<()> {
    for access in [FileSystemAccessMode::Deny, FileSystemAccessMode::Write] {
        let profile = permissions(vec![
            FileSystemSandboxEntry::new(
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::Minimal,
                },
                FileSystemAccessMode::Read,
            ),
            FileSystemSandboxEntry::new(
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::ProjectRoots { subpath: None },
                },
                access,
            ),
        ]);
        let plan = compile(&profile, Path::new("/usr/local"))?;
        assert!(!plan.roots.contains_key(Path::new("/usr/local/bin")));
        assert_eq!(
            plan.roots.get(Path::new("/usr/local")),
            Some(&if access == FileSystemAccessMode::Deny {
                Access::Deny
            } else {
                Access::Write
            })
        );
    }
    Ok(())
}

#[test]
fn readable_symlink_grants_keep_the_logical_source_for_descriptor_validation() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    std::fs::create_dir(root.join("private"))?;
    std::os::unix::fs::symlink(root.join("private"), root.join("alias"))?;
    let profile = permissions(vec![FileSystemSandboxEntry::new(
        FileSystemPath::Special {
            value: FileSystemSpecialPath::ProjectRoots {
                subpath: Some("alias".into()),
            },
        },
        FileSystemAccessMode::Read,
    )]);
    assert_eq!(
        compile(&profile, &root)?.roots,
        BTreeMap::from([(root.join("alias"), Access::Read)])
    );
    Ok(())
}
