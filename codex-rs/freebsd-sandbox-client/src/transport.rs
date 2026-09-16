use crate::protocol::CHUNK;
use crate::protocol::Launch;
use crate::protocol::Request;
use crate::protocol::Response;
use crate::protocol::TerminalSize;
use crate::protocol::VERSION;
use crate::protocol::receive;
use crate::protocol::send;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::Mutex;

fn connect() -> Result<UnixStream> {
    let stream = UnixStream::connect(crate::SOCKET_PATH).context(
        "sandbox service unavailable; install/start codex_freebsd_sandbox (execution was blocked)",
    )?;
    let mut uid = 0;
    let mut gid = 0;
    ensure!(
        unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0 && uid == 0,
        "sandbox service is not owned by root"
    );
    Ok(stream)
}

pub(crate) fn status() -> Result<String> {
    let mut stream = connect()?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    send(&mut stream, &Request::Probe { version: VERSION })?;
    match receive(&mut stream)? {
        Response::Status {
            version: VERSION,
            description,
        } => Ok(description),
        Response::Error { message } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("incompatible sandbox service protocol"),
    }
}

fn terminal_size() -> TerminalSize {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    unsafe {
        libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut size);
    }
    TerminalSize {
        rows: size.ws_row.max(1),
        cols: size.ws_col.max(1),
    }
}

struct Terminal(Option<libc::termios>);

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(saved) = self.0 {
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved);
            }
        }
    }
}

pub(crate) fn run(mut request: Launch) -> Result<i32> {
    ensure!(request.version == VERSION, "incompatible sandbox protocol");
    let interactive =
        request.terminal.is_some() || unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    if interactive && request.terminal.is_none() {
        request.terminal = Some(terminal_size());
    }
    let mut stream = connect()?;
    send(&mut stream, &Request::Launch(Box::new(request)))?;
    match receive(&mut stream)? {
        Response::Ready => {}
        Response::Error { message } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("invalid sandbox launch response"),
    }
    let mut terminal = Terminal(None);
    if interactive {
        let mut saved = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut saved) } == 0 {
            let mut raw = saved;
            unsafe {
                libc::cfmakeraw(&mut raw);
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw);
            }
            terminal.0 = Some(saved);
        }
    }
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let input_writer = Arc::clone(&writer);
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        let mut data = vec![0; CHUNK];
        loop {
            let request = match input.read(&mut data) {
                Ok(0) | Err(_) => Request::Eof,
                Ok(size) => Request::Input {
                    data: data[..size].to_vec(),
                },
            };
            let eof = matches!(request, Request::Eof);
            let Ok(mut writer) = input_writer.lock() else {
                break;
            };
            if send(&mut *writer, &request).is_err() || eof {
                break;
            }
        }
    });
    let mut signals = signal_hook::iterator::Signals::new([
        libc::SIGINT,
        libc::SIGTERM,
        libc::SIGHUP,
        libc::SIGWINCH,
    ])?;
    let cancellation = stream.try_clone()?;
    std::thread::spawn(move || {
        for signal in signals.forever() {
            if signal == libc::SIGTERM || signal == libc::SIGHUP {
                // A full stdin stream may hold the writer mutex. Disconnect
                // directly so cancellation cannot wait behind blocked input.
                let _ = cancellation.shutdown(std::net::Shutdown::Both);
                break;
            }
            let request = if signal == libc::SIGWINCH {
                Request::Resize {
                    size: terminal_size(),
                }
            } else {
                Request::Signal { signal }
            };
            let Ok(mut writer) = writer.lock() else {
                break;
            };
            if send(&mut *writer, &request).is_err() {
                break;
            }
        }
    });
    loop {
        match receive(&mut stream)? {
            Response::Output { stderr, data } => {
                if stderr {
                    std::io::stderr().write_all(&data)?;
                    std::io::stderr().flush()?;
                } else {
                    std::io::stdout().write_all(&data)?;
                    std::io::stdout().flush()?;
                }
            }
            Response::Exit { code } => return Ok(code),
            Response::Error { message } => anyhow::bail!("{message}"),
            _ => anyhow::bail!("invalid sandbox response"),
        }
    }
}

pub(crate) fn capabilities() -> Result<crate::protocol::Capabilities> {
    let mut stream = connect()?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
    send(&mut stream, &Request::Capabilities { version: VERSION })?;
    match receive(&mut stream)? {
        Response::Capabilities(status) => {
            crate::validate_capabilities(&status)?;
            Ok(status)
        }
        Response::Error { message } => anyhow::bail!("{message}"),
        _ => anyhow::bail!(
            "sandbox service lacks capability discovery; update the administrator-installed service"
        ),
    }
}
