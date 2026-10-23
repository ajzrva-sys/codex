use codex_exec_server_protocol::JSONRPCErrorError;
use codex_sandboxing::SandboxExecRequest;
use codex_utils_pty::ChildStdin;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncSeekExt;
use tokio::io::AsyncWriteExt;

use super::open_response;
use crate::fs_sandbox::drain_helper_stderr;
use crate::fs_sandbox::io_error;
use crate::fs_sandbox::spawn_command;
use crate::rpc::internal_error;

/// Materialize a jailed file into an anonymous snapshot without transferring a
/// host socket into the jail. Both the status header and copy buffers are bounded.
pub(super) async fn open(
    command: SandboxExecRequest,
    request: Vec<u8>,
) -> Result<tokio::fs::File, JSONRPCErrorError> {
    let mut snapshot = tokio::fs::File::from_std(tempfile::tempfile().map_err(io_error)?);
    let mut child = spawn_command(command, ChildStdin::Piped)?;
    let stderr = drain_helper_stderr(&mut child);
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| internal_error("missing fs sandbox helper stdin".into()))?;
    stdin.write_all(&request).await.map_err(io_error)?;
    stdin.shutdown().await.map_err(io_error)?;
    drop(stdin);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| internal_error("missing fs sandbox helper stdout".into()))?;
    let mut stdout = tokio::io::BufReader::new(stdout);
    let mut header = Vec::new();
    (&mut stdout)
        .take(16 * 1024)
        .read_until(b'\n', &mut header)
        .await
        .map_err(io_error)?;
    if header.last() != Some(&b'\n') {
        return Err(internal_error("invalid fs sandbox stream header".into()));
    }
    open_response(&header)?;
    tokio::io::copy(&mut stdout, &mut snapshot)
        .await
        .map_err(io_error)?;
    let status = child.wait().await.map_err(io_error)?;
    let stderr = stderr
        .await
        .map_err(|error| internal_error(error.to_string()))?
        .map_err(io_error)?;
    if !status.success() {
        return Err(internal_error(format!(
            "fs sandbox file stream failed with status {status}: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    snapshot.flush().await.map_err(io_error)?;
    snapshot.rewind().await.map_err(io_error)?;
    Ok(snapshot)
}

/// A bounded positional writer whose file remains inside the jail. Each block
/// is flushed and acknowledged before the caller can poll another input block.
#[cfg(target_os = "freebsd")]
pub(crate) struct Writer {
    child: codex_utils_pty::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
    stderr: Option<tokio::task::JoinHandle<Result<Vec<u8>, std::io::Error>>>,
}

#[cfg(target_os = "freebsd")]
impl Writer {
    pub(super) async fn open(
        command: SandboxExecRequest,
        request: Vec<u8>,
    ) -> Result<Self, JSONRPCErrorError> {
        let mut child = spawn_command(command, ChildStdin::Piped)?;
        let stderr = drain_helper_stderr(&mut child);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| internal_error("missing fs sandbox helper stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| internal_error("missing fs sandbox helper stdout".into()))?;
        let mut writer = Self {
            child,
            stdin,
            stdout: tokio::io::BufReader::new(stdout),
            stderr: Some(stderr),
        };
        writer.stdin.write_all(&request).await.map_err(io_error)?;
        writer.stdin.flush().await.map_err(io_error)?;
        let mut header = Vec::new();
        (&mut writer.stdout)
            .take(16 * 1024)
            .read_until(b'\n', &mut header)
            .await
            .map_err(io_error)?;
        if header.last() != Some(&b'\n') {
            return Err(internal_error("invalid fs sandbox stream header".into()));
        }
        open_response(&header)?;
        Ok(writer)
    }

    pub(crate) async fn write_block(&mut self, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
        if !(1..=codex_file_system::FILE_WRITE_CHUNK_SIZE).contains(&bytes.len())
            || offset
                .checked_add(bytes.len() as u64)
                .and_then(|end| i64::try_from(end).ok())
                .is_none()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid fs sandbox write block",
            ));
        }
        let len = bytes.len() as u32;
        self.stdin.write_u32(len).await?;
        self.stdin.write_u64(offset).await?;
        self.stdin.write_all(bytes).await?;
        self.stdin.flush().await?;
        if self.stdout.read_u32().await? != len {
            return Err(std::io::Error::other(
                "invalid fs sandbox write acknowledgement",
            ));
        }
        Ok(())
    }

    pub(crate) async fn finish(&mut self) -> std::io::Result<()> {
        if self.stderr.is_none() {
            return Ok(());
        }
        self.stdin.write_u32(0).await?;
        self.stdin.flush().await?;
        if self.stdout.read_u32().await? != 0 {
            return Err(std::io::Error::other(
                "invalid fs sandbox close acknowledgement",
            ));
        }
        self.stdin.shutdown().await?;
        let stderr = self
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("missing fs sandbox writer stderr task"))?;
        let (status, stderr) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::try_join!(self.child.wait(), async {
                stderr.await.map_err(std::io::Error::other)?
            })
        })
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "fs sandbox writer did not exit",
            )
        })??;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "fs sandbox writer failed with status {status}: {}",
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "sandboxed_file_open_freebsd_tests.rs"]
mod tests;
