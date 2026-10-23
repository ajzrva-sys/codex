use super::*;
use codex_config::test_support::CloudConfigBundleFixture;
use codex_config::types::SandboxWorkspaceWrite;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSpecialPath;
use core_test_support::TempDirExt;
use pretty_assertions::assert_eq;
use std::path::Path;
use tempfile::TempDir;

#[tokio::test]
async fn freebsd_implicit_defaults_follow_project_trust_and_hide_credentials() -> anyhow::Result<()>
{
    for trusted in [false, true] {
        let workspace = TempDir::new()?;
        let home = workspace.path().join(".codex");
        std::fs::create_dir(&home)?;
        let config = Config::load_from_base_config_with_overrides(
            ConfigToml {
                projects: trusted.then(|| {
                    std::collections::HashMap::from([(
                        workspace.path().to_string_lossy().into_owned(),
                        ProjectConfig {
                            trust_level: Some(TrustLevel::Trusted),
                        },
                    )])
                }),
                ..Default::default()
            },
            ConfigOverrides {
                cwd: Some(workspace.path().to_path_buf()),
                ..Default::default()
            },
            AbsolutePathBuf::from_absolute_path(&home)?,
        )
        .await?;
        let policy = config.permissions.file_system_sandbox_policy();
        assert!(!policy.has_full_disk_read_access());
        assert!(policy.include_platform_defaults());
        assert!(
            policy.can_read_local_path_with_cwd(&workspace.path().join("source"), workspace.path())
        );
        assert_eq!(
            policy
                .can_write_local_path_with_cwd(&workspace.path().join("source"), workspace.path()),
            trusted
        );
        assert!(!policy.can_read_local_path_with_cwd(&home.join("auth.json"), workspace.path()));
        assert_eq!(
            config.permissions.network_sandbox_policy(),
            NetworkSandboxPolicy::Restricted
        );
        assert_eq!(config.permissions.active_permission_profile(), None);
        assert!(policy.entries.iter().all(|entry| !matches!(
            entry.path,
            FileSystemPath::GlobPattern { .. }
                | FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root
                }
        )));
    }
    Ok(())
}

#[test]
fn freebsd_implicit_policy_preserves_network_tmp_and_metadata_rules() -> anyhow::Result<()> {
    let cwd = Path::new("/home/codex-freebsd-fixture");
    let protected =
        [".codex", ".ssh"].map(|name| AbsolutePathBuf::from_absolute_path(cwd.join(name)).unwrap());
    for exclude_tmp in [false, true] {
        let original = PermissionProfile::workspace_write_with(
            &[],
            NetworkSandboxPolicy::Enabled,
            exclude_tmp,
            exclude_tmp,
        );
        let profile = permissions::freebsd_implicit_permission_profile(&original, &protected);
        let (policy, network) = profile.to_runtime_permissions();
        assert_eq!(network, NetworkSandboxPolicy::Enabled);
        assert!(policy.include_platform_defaults());
        assert!(!policy.has_full_disk_read_access());
        assert!(policy.can_write_local_path_with_cwd(&cwd.join("source"), cwd));
        assert!(!policy.can_write_local_path_with_cwd(&cwd.join(".git/config"), cwd));
        assert_eq!(
            policy.can_write_local_path_with_cwd(Path::new("/tmp/output"), cwd),
            !exclude_tmp
        );
        for path in &protected {
            assert!(!policy.can_read_local_path_with_cwd(&path.join("private"), cwd));
        }
    }
    Ok(())
}

#[tokio::test]
async fn freebsd_implicit_default_keeps_explicit_and_persisted_selections() -> anyhow::Result<()> {
    let cases = [
        (
            ConfigToml::default(),
            ConfigOverrides {
                permission_profile: Some(PermissionProfile::read_only()),
                ..Default::default()
            },
        ),
        (
            ConfigToml::default(),
            ConfigOverrides {
                sandbox_mode: Some(SandboxMode::ReadOnly),
                ..Default::default()
            },
        ),
        (
            ConfigToml {
                sandbox_mode: Some(SandboxMode::ReadOnly),
                ..Default::default()
            },
            ConfigOverrides::default(),
        ),
        (
            ConfigToml {
                sandbox_workspace_write: Some(SandboxWorkspaceWrite::default()),
                ..Default::default()
            },
            ConfigOverrides::default(),
        ),
        (
            ConfigToml {
                default_permissions: Some(BUILT_IN_READ_ONLY_PROFILE.into()),
                ..Default::default()
            },
            ConfigOverrides::default(),
        ),
        (
            ConfigToml::default(),
            ConfigOverrides {
                default_permissions: Some(BUILT_IN_READ_ONLY_PROFILE.into()),
                ..Default::default()
            },
        ),
        (
            ConfigToml::default(),
            ConfigOverrides {
                persisted_permission_profile: Some(PermissionProfile::read_only()),
                ..Default::default()
            },
        ),
        (
            ConfigToml::default(),
            ConfigOverrides {
                persisted_permission_profile_id: Some(BUILT_IN_READ_ONLY_PROFILE.into()),
                ..Default::default()
            },
        ),
    ];
    for (base, mut overrides) in cases {
        let home = TempDir::new()?;
        let cwd = TempDir::new()?;
        overrides.cwd = Some(cwd.path().to_path_buf());
        let config =
            Config::load_from_base_config_with_overrides(base, overrides, home.abs()).await?;
        assert!(
            config
                .permissions
                .file_system_sandbox_policy()
                .has_full_disk_read_access()
        );
    }
    Ok(())
}

#[tokio::test]
async fn freebsd_implicit_default_keeps_managed_selections() -> anyhow::Result<()> {
    for requirements in [
        "allowed_sandbox_modes = [\"read-only\"]",
        "allowed_permission_profiles = { \":read-only\" = true }\ndefault_permissions = \":read-only\"",
    ] {
        let home = TempDir::new()?;
        let cwd = TempDir::new()?;
        let config = ConfigBuilder::without_managed_config_for_tests()
            .codex_home(home.path().to_path_buf())
            .fallback_cwd(Some(cwd.path().to_path_buf()))
            .cloud_config_bundle(
                CloudConfigBundleFixture::loader_with_enterprise_requirement(requirements),
            )
            .build()
            .await?;
        assert!(
            config
                .permissions
                .file_system_sandbox_policy()
                .has_full_disk_read_access()
        );
    }
    Ok(())
}
