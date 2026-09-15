use codex_exec_server_protocol::JSONRPCErrorError;
use codex_sandboxing::SandboxExecRequest;
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
    let mut child = spawn_command(command, std::process::Stdio::piped())?;
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

#[cfg(test)]
#[path = "sandboxed_file_open_freebsd_tests.rs"]
mod tests;
