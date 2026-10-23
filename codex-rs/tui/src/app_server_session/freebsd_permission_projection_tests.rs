use super::*;
use crate::legacy_core::config::ConfigBuilder;
use codex_config::LoaderOverrides;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn freebsd_lifecycle_embedded_defaults_do_not_send_legacy_root_overrides() {
    for trusted in [false, true] {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("workspace");
        if trusted {
            std::fs::write(
                home.path().join("config.toml"),
                format!(
                    "[projects.{}]\ntrust_level = \"trusted\"\n",
                    serde_json::to_string(&cwd.path().to_string_lossy()).unwrap()
                ),
            )
            .unwrap();
        }
        let config = ConfigBuilder::default()
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .codex_home(home.path().to_path_buf())
            .fallback_cwd(Some(cwd.path().to_path_buf()))
            .build()
            .await
            .expect("implicit config");
        let profile = config.permissions.effective_permission_profile();
        assert!(
            !profile
                .file_system_sandbox_policy()
                .has_full_disk_read_access()
        );
        let start =
            thread_start_params_from_config(&config, ThreadParamsMode::Embedded, None, None);
        let resume = thread_resume_params_from_config(
            config.clone(),
            ThreadId::new(),
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            ResumeModelSettings::OverrideFromCurrentConfig,
            crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
        );
        let fork = thread_fork_params_from_config(
            config.clone(),
            ThreadId::new(),
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
        );
        assert_eq!((start.sandbox, start.permissions), (None, None));
        assert_eq!((resume.sandbox, resume.permissions), (None, None));
        assert_eq!((fork.sandbox, fork.permissions), (None, None));
        assert_eq!(
            sandbox_mode_from_config(&config, ThreadParamsMode::Remote),
            sandbox_mode_from_permission_profile(&profile, config.cwd.as_path())
        );
    }
}

#[tokio::test]
async fn freebsd_lifecycle_root_read_carveouts_keep_explicit_embedded_overrides() {
    use crate::legacy_core::config::ConfigOverrides;
    use codex_protocol::permissions::{
        FileSystemAccessMode, FileSystemPath, FileSystemSandboxEntry, FileSystemSandboxPolicy,
        FileSystemSpecialPath, NetworkSandboxPolicy,
    };
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    for root in [
        FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        FileSystemPath::Path {
            path: codex_utils_path_uri::PathUri::from_host_native_path("/").unwrap(),
        },
    ] {
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry::new(root, FileSystemAccessMode::Read),
            FileSystemSandboxEntry::new(
                FileSystemPath::Path {
                    path: codex_utils_path_uri::PathUri::from_host_native_path(
                        cwd.path().join("private"),
                    )
                    .unwrap(),
                },
                FileSystemAccessMode::Deny,
            ),
        ]);
        assert!(!policy.has_full_disk_read_access());
        let profile =
            PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted);
        let config = ConfigBuilder::default()
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .codex_home(home.path().to_path_buf())
            .fallback_cwd(Some(cwd.path().to_path_buf()))
            .harness_overrides(ConfigOverrides {
                permission_profile: Some(profile),
                ..Default::default()
            })
            .build()
            .await
            .expect("explicit root-read policy");
        assert_eq!(
            sandbox_mode_from_config(&config, ThreadParamsMode::Embedded),
            Some(codex_app_server_protocol::SandboxMode::ReadOnly)
        );
    }
}
