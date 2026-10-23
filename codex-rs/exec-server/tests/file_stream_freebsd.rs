#![cfg(target_os = "freebsd")]

mod common;
#[path = "file_system/support.rs"]
mod support;

use anyhow::Result;
use bytes::Bytes;
use codex_exec_server::FsCloseParams;
use codex_exec_server::FsOpenMode;
use codex_exec_server::FsOpenParams;
use codex_exec_server::FsWriteBlockParams;
use codex_utils_path_uri::PathUri;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use support::FileSystemImplementation;
use support::create_file_system_context;
use support::read_only_sandbox;
use support::workspace_write_sandbox;

#[tokio::test]
async fn jailed_streaming_replacement_flushes_each_block_and_truncates_empty_files() -> Result<()> {
    for implementation in [
        FileSystemImplementation::Local,
        FileSystemImplementation::Remote,
    ] {
        let context = create_file_system_context(implementation).await?;
        let root = tempfile::tempdir()?;
        let target = root.path().join("stream.bin");
        std::fs::write(&target, b"previous bytes")?;
        let uri = PathUri::from_host_native_path(&target)?;
        let sandbox = workspace_write_sandbox(root.path().to_path_buf());
        let chunk_size = 1024 * 1024;
        let contents = futures::stream::iter(0..3_u8).map(|block| {
            assert_eq!(
                std::fs::metadata(&target).unwrap().len(),
                u64::from(block) * chunk_size as u64
            );
            Ok(Bytes::from(vec![block; chunk_size]))
        });
        assert_eq!(
            context
                .file_system
                .write_file_stream(&uri, contents.boxed(), Some(&sandbox))
                .await?,
            3 * chunk_size as u64
        );
        assert_eq!(
            std::fs::read(&target)?,
            [
                vec![0; chunk_size],
                vec![1; chunk_size],
                vec![2; chunk_size]
            ]
            .concat()
        );
        assert_eq!(
            context
                .file_system
                .write_file_stream(&uri, futures::stream::empty().boxed(), Some(&sandbox))
                .await?,
            0
        );
        assert_eq!(std::fs::read(&target)?, Vec::<u8>::new());
    }
    Ok(())
}

#[tokio::test]
async fn jailed_streaming_denial_preserves_the_target_and_does_not_poll_contents() -> Result<()> {
    for implementation in [
        FileSystemImplementation::Local,
        FileSystemImplementation::Remote,
    ] {
        let context = create_file_system_context(implementation).await?;
        let root = tempfile::tempdir()?;
        let target = root.path().join("protected.bin");
        std::fs::write(&target, b"preserve me")?;
        let uri = PathUri::from_host_native_path(&target)?;
        let sandbox = read_only_sandbox(root.path().to_path_buf());
        let contents =
            futures::stream::poll_fn(|_| -> std::task::Poll<Option<std::io::Result<Bytes>>> {
                panic!("denied replacement must not poll its contents")
            });
        assert!(
            context
                .file_system
                .write_file_stream(&uri, contents.boxed(), Some(&sandbox))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&target)?, b"preserve me");
    }
    Ok(())
}

#[tokio::test]
async fn jailed_remote_write_blocks_preserve_positional_offsets() -> Result<()> {
    let server = common::exec_server::exec_server().await?;
    let client = codex_exec_server::ExecServerClient::connect_websocket(
        codex_exec_server::RemoteExecServerConnectArgs::new(
            server.websocket_url().to_string(),
            "jailed-stream-test".to_string(),
            codex_http_client::HttpClientFactory::new(
                codex_http_client::OutboundProxyPolicy::ReqwestDefault,
            ),
        ),
    )
    .await?;
    let root = tempfile::tempdir()?;
    let target = root.path().join("position.bin");
    let uri = PathUri::from_host_native_path(&target)?;
    let sandbox = workspace_write_sandbox(root.path().to_path_buf());
    client
        .fs_open(FsOpenParams {
            handle_id: "position".into(),
            path: uri,
            mode: FsOpenMode::Replace,
            sandbox: Some(sandbox),
        })
        .await?;
    for (offset, bytes) in [(4, b"world".to_vec()), (0, b"hey ".to_vec())] {
        client
            .fs_write_block(FsWriteBlockParams {
                handle_id: "position".into(),
                offset,
                chunk: bytes.into(),
            })
            .await?;
    }
    client
        .fs_close(FsCloseParams {
            handle_id: "position".into(),
        })
        .await?;
    assert_eq!(std::fs::read(&target)?, b"hey world");
    Ok(())
}

#[tokio::test]
async fn jailed_streaming_source_failure_preserves_only_acknowledged_bytes() -> Result<()> {
    for implementation in [
        FileSystemImplementation::Local,
        FileSystemImplementation::Remote,
    ] {
        let context = create_file_system_context(implementation).await?;
        let root = tempfile::tempdir()?;
        let target = root.path().join("partial.bin");
        std::fs::write(&target, b"previous")?;
        let uri = PathUri::from_host_native_path(&target)?;
        let sandbox = workspace_write_sandbox(root.path().to_path_buf());
        let contents = futures::stream::iter([
            Ok(Bytes::from_static(b"confirmed")),
            Err(std::io::Error::other("source failed")),
        ]);
        let error = context
            .file_system
            .write_file_stream(&uri, contents.boxed(), Some(&sandbox))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("source failed"));
        assert_eq!(std::fs::read(&target)?, b"confirmed");
    }
    Ok(())
}
