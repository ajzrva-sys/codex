use std::collections::HashMap;

use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_sandboxing::SandboxExecRequest;
use codex_sandboxing::SandboxType;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tokio::io::AsyncReadExt;

fn command(script: &str) -> SandboxExecRequest {
    let cwd = PathUri::from_host_native_path(std::env::current_dir().unwrap()).unwrap();
    SandboxExecRequest {
        command: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: cwd.clone(),
        sandbox_policy_cwd: cwd,
        env: HashMap::new(),
        network: None,
        network_environment_id: None,
        sandbox: SandboxType::None,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        permission_profile: PermissionProfile::Disabled,
        arg0: None,
    }
}

const HEADER: &str =
    "printf '%s\\n' '{\"status\":\"ok\",\"payload\":{\"operation\":\"fs/open\",\"response\":{}}}'";

#[tokio::test]
async fn streamed_snapshot_preserves_binary_contents_after_helper_exit() {
    let script = format!(
        "{HEADER}; printf '\\000\\377\\012'; /bin/dd if=/dev/zero bs=65536 count=4 2>/dev/null"
    );
    let mut file = super::open(command(&script), Vec::new()).await.unwrap();
    let mut actual = Vec::new();
    file.read_to_end(&mut actual).await.unwrap();
    let mut expected = vec![0, 255, 10];
    expected.resize(3 + 4 * 65536, 0);
    assert_eq!(actual, expected);
    assert_eq!(file.metadata().await.unwrap().len(), expected.len() as u64);
}

#[tokio::test]
async fn failed_stream_never_returns_a_partial_file() {
    let script = format!("{HEADER}; printf partial; printf failure >&2; exit 7");
    let error = super::open(command(&script), Vec::new()).await.unwrap_err();
    assert!(error.message.contains('7'));
    assert!(error.message.contains("failure"));
}

#[tokio::test]
async fn oversized_or_incomplete_stream_headers_are_rejected() {
    for script in [
        "printf '{'",
        "/bin/dd if=/dev/zero bs=65536 count=1 2>/dev/null",
    ] {
        let error = super::open(command(script), Vec::new()).await.unwrap_err();
        assert_eq!(error.message, "invalid fs sandbox stream header");
    }
}
