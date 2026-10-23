use std::error::Error;

use tokio::io;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;

use crate::fs_helper::FsHelperOpenResponse;
use crate::fs_helper::FsHelperPayload;
use crate::fs_helper::FsHelperRequest;
use crate::fs_helper::FsHelperResponse;
use crate::fs_helper::map_fs_error;
use crate::fs_helper::run_direct_request;
use crate::regular_file;

pub fn main() -> ! {
    let exit_code = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => match runtime.block_on(run_main()) {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("fs sandbox helper failed: {err}");
                1
            }
        },
        Err(err) => {
            eprintln!("failed to start fs sandbox helper runtime: {err}");
            1
        }
    };
    std::process::exit(exit_code);
}

async fn run_main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut stdin = BufReader::new(io::stdin());
    let mut input = String::new();
    stdin.read_line(&mut input).await?;
    let request: FsHelperRequest = serde_json::from_str(&input)?;
    let mut opened_file = None;
    #[cfg(target_os = "freebsd")]
    let mut opened_mode = None;
    let result = match request {
        FsHelperRequest::Open(params) => {
            let result: io::Result<_> = async {
                #[cfg(target_os = "freebsd")]
                {
                    opened_mode = Some(params.mode);
                }
                let path = params.path.to_abs_path()?;
                let file = regular_file::open(path.as_path(), params.mode).await?;
                // Unix can hand the opened fd directly to the parent.
                #[cfg(all(unix, not(target_os = "freebsd")))]
                crate::sandboxed_file_open::transfer_file(&file)?;
                let response = FsHelperOpenResponse {
                    // Windows duplicates from the helper process instead.
                    #[cfg(windows)]
                    process_id: std::process::id(),
                    // The parent needs the raw handle to duplicate it.
                    #[cfg(windows)]
                    file_handle: {
                        use std::os::windows::io::AsRawHandle;

                        file.as_raw_handle() as usize as u64
                    },
                };
                opened_file = Some(file);
                Ok(FsHelperPayload::Open(response))
            }
            .await;
            result.map_err(map_fs_error)
        }
        request => run_direct_request(request).await,
    };
    let response = match result {
        Ok(payload) => FsHelperResponse::Ok(payload),
        Err(error) => FsHelperResponse::Error(error),
    };
    let mut stdout = io::stdout();
    stdout
        .write_all(serde_json::to_string(&response)?.as_bytes())
        .await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;

    // The FreeBSD jail relay carries bytes, never host sockets or descriptors.
    // Stream the opened file after its status header into the parent's snapshot.
    #[cfg(target_os = "freebsd")]
    if let Some(file) = &mut opened_file {
        if matches!(opened_mode, Some(crate::protocol::FsOpenMode::Replace)) {
            receive_write_blocks(file, &mut stdin, &mut stdout).await?;
        } else {
            tokio::io::copy(file, &mut stdout).await?;
            stdout.flush().await?;
        }
    }

    // Keep the Windows handle alive until the parent duplicates it.
    #[cfg(windows)]
    if opened_file.is_some() {
        use tokio::io::AsyncReadExt;

        let mut acknowledgement = Vec::new();
        stdin.read_to_end(&mut acknowledgement).await?;
    }
    drop(opened_file);
    Ok(())
}

// The relay accepts one bounded positional block at a time. No host descriptor
// crosses the jail boundary, and acknowledgements follow completed file writes.
#[cfg(target_os = "freebsd")]
async fn receive_write_blocks(
    file: &mut tokio::fs::File,
    stdin: &mut (impl tokio::io::AsyncRead + Unpin),
    stdout: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> io::Result<()> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncSeekExt;
    let mut bytes = Vec::new();
    loop {
        let len = stdin.read_u32().await? as usize;
        if len == 0 {
            file.flush().await?;
            stdout.write_u32(0).await?;
            return stdout.flush().await;
        }
        if len > codex_file_system::FILE_WRITE_CHUNK_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fs sandbox write block is too large",
            ));
        }
        let offset = stdin.read_u64().await?;
        if offset
            .checked_add(len as u64)
            .and_then(|end| i64::try_from(end).ok())
            .is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid fs sandbox write range",
            ));
        }
        bytes.resize(len, 0);
        stdin.read_exact(&mut bytes).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        file.write_all(&bytes).await?;
        file.flush().await?;
        stdout.write_u32(len as u32).await?;
        stdout.flush().await?;
    }
}
