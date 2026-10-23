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

#[test]
fn shared_client_managed_policy_preserves_grants_and_network() -> Result<()> {
    use codex_freebsd_sandbox_client::policy as client;

    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let workspace = root.join("workspace");
    let dependencies = root.join("dependencies");
    let original = root.join("original");
    for path in [&workspace, &dependencies, &original] {
        std::fs::create_dir(path)?;
    }
    for network in [client::Network::Restricted, client::Network::Enabled] {
        let policy = client::Policy {
            grants: vec![
                client::Grant {
                    path: workspace.clone(),
                    access: client::Access::Write,
                },
                client::Grant {
                    path: dependencies.clone(),
                    access: client::Access::Read,
                },
                client::Grant {
                    path: original.clone(),
                    access: client::Access::Deny,
                },
            ],
            network,
        };
        let profile: PermissionProfile = serde_json::from_value(policy.permissions()?)?;
        let plan = compile(&profile, &workspace)?;
        let grants = [&workspace, &dependencies, &original]
            .into_iter()
            .map(|path| (path.clone(), plan.roots.get(path).copied()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            grants,
            BTreeMap::from([
                (workspace.clone(), Some(Access::Write)),
                (dependencies.clone(), Some(Access::Read)),
                (original.clone(), Some(Access::Deny)),
            ])
        );
        assert_eq!(
            plan.network,
            match network {
                client::Network::Restricted => NetworkSandboxPolicy::Restricted,
                client::Network::Enabled => NetworkSandboxPolicy::Enabled,
            }
        );
    }
    Ok(())
}
