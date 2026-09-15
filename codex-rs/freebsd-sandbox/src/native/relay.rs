use super::daemon::STOP;
use super::filesystem::View;
use super::sys;
use crate::protocol::CHUNK;
use crate::protocol::MAX_FRAME;
use crate::protocol::Request;
use crate::protocol::Response;
use crate::protocol::send;
use anyhow::Result;
use anyhow::ensure;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::process::Child;
use std::sync::atomic::Ordering;

pub(super) fn run(
    stream: &mut UnixStream,
    child: &mut Child,
    master: Option<File>,
    view: &mut View,
) -> Result<i32> {
    let terminal = master.is_some();
    let mut outputs: Vec<(File, bool)> = Vec::new();
    let mut input = if let Some(master) = master {
        outputs.push((master.try_clone()?, false));
        Some(master)
    } else {
        if let Some(stdout) = child.stdout.take() {
            outputs.push((unsafe { File::from_raw_fd(stdout.into_raw_fd()) }, false));
        }
        if let Some(stderr) = child.stderr.take() {
            outputs.push((unsafe { File::from_raw_fd(stderr.into_raw_fd()) }, true));
        }
        child
            .stdin
            .take()
            .map(|stdin| unsafe { File::from_raw_fd(stdin.into_raw_fd()) })
    };
    stream.set_nonblocking(true)?;
    for (file, _) in &outputs {
        sys::set_nonblocking(file.as_raw_fd())?;
    }
    if let Some(input) = &input {
        sys::set_nonblocking(input.as_raw_fd())?;
    }
    let mut incoming = Vec::new();
    let mut outgoing = VecDeque::new();
    let mut pending_input = VecDeque::new();
    let mut eof = false;
    let mut exit = None;
    loop {
        ensure!(!STOP.load(Ordering::Relaxed), "sandbox service is stopping");
        if exit.is_none()
            && let Some(status) = child.try_wait()?
        {
            exit = Some(
                status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
            );
            view.stop()?; // Kill descendants before waiting for output EOF.
            input.take();
        }
        if let Some(code) = exit
            && outputs.is_empty()
            && outgoing.is_empty()
        {
            stream.set_nonblocking(false)?;
            return Ok(code);
        }
        let mut polls = vec![libc::pollfd {
            fd: stream.as_raw_fd(),
            // Backpressure the client while the command is not consuming
            // stdin, leaving room to decode the remainder of a partial frame.
            events: if pending_input.len() < MAX_FRAME / 2 {
                libc::POLLIN
            } else {
                0
            } | if outgoing.is_empty() {
                0
            } else {
                libc::POLLOUT
            },
            revents: 0,
        }];
        for (file, _) in &outputs {
            polls.push(libc::pollfd {
                fd: file.as_raw_fd(),
                events: if outgoing.len() < 4 * MAX_FRAME {
                    libc::POLLIN
                } else {
                    0
                },
                revents: 0,
            });
        }
        let result = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 25) };
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(std::io::Error::last_os_error().into());
        }
        ensure!(
            polls[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0,
            "sandbox client disconnected"
        );
        if polls[0].revents & libc::POLLIN != 0 {
            let mut bytes = [0; CHUNK];
            match stream.read(&mut bytes) {
                Ok(0) => anyhow::bail!("sandbox client disconnected"),
                Ok(count) => incoming.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
            while incoming.len() >= 4 {
                let length = u32::from_be_bytes(incoming[..4].try_into()?) as usize;
                ensure!(length <= MAX_FRAME, "sandbox frame too large");
                if incoming.len() < length + 4 {
                    break;
                }
                let request: Request = serde_json::from_slice(&incoming[4..length + 4])?;
                incoming.drain(..length + 4);
                match request {
                    Request::Input { data } => {
                        ensure!(
                            data.len() <= CHUNK && pending_input.len() + data.len() <= MAX_FRAME,
                            "sandbox input buffer exceeded"
                        );
                        ensure!(!eof, "input after EOF");
                        pending_input.extend(data);
                    }
                    Request::Eof => {
                        ensure!(!eof, "duplicate EOF");
                        eof = true;
                        if terminal {
                            ensure!(
                                pending_input.len() < MAX_FRAME,
                                "sandbox input buffer exceeded"
                            );
                            pending_input.push_back(4);
                        }
                    }
                    Request::Resize { size } => {
                        if terminal && let Some(input) = &input {
                            let size = libc::winsize {
                                ws_row: size.rows,
                                ws_col: size.cols,
                                ws_xpixel: 0,
                                ws_ypixel: 0,
                            };
                            sys::cvt(unsafe {
                                libc::ioctl(input.as_raw_fd(), libc::TIOCSWINSZ, &size)
                            })?;
                        }
                    }
                    Request::Signal { signal } => {
                        ensure!(
                            [libc::SIGINT, libc::SIGTERM, libc::SIGHUP].contains(&signal),
                            "unsupported signal"
                        );
                        if exit.is_none() {
                            unsafe {
                                libc::kill(-(child.id() as i32), signal);
                            }
                        }
                        if signal != libc::SIGINT {
                            view.stop()?;
                        }
                    }
                    Request::Launch(_) | Request::Probe { .. } => {
                        anyhow::bail!("unexpected sandbox request")
                    }
                }
            }
        }
        if !pending_input.is_empty()
            && let Some(writer) = input.as_mut()
        {
            match writer.write(pending_input.make_contiguous()) {
                Ok(count) => {
                    pending_input.drain(..count);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => {
                    input.take();
                    pending_input.clear();
                }
            }
        }
        if eof && !terminal && pending_input.is_empty() {
            input.take();
        }
        for index in (0..outputs.len()).rev() {
            if polls[index + 1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                continue;
            }
            let mut data = vec![0; CHUNK];
            match outputs[index].0.read(&mut data) {
                Ok(0) => {
                    outputs.remove(index);
                }
                Ok(count) => {
                    data.truncate(count);
                    let mut frame = Vec::new();
                    send(
                        &mut frame,
                        &Response::Output {
                            stderr: outputs[index].1,
                            data,
                        },
                    )?;
                    outgoing.extend(frame);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if terminal && error.raw_os_error() == Some(libc::EIO) => {
                    outputs.remove(index);
                }
                Err(error) => return Err(error.into()),
            }
        }
        if !outgoing.is_empty() {
            match stream.write(outgoing.make_contiguous()) {
                Ok(0) => anyhow::bail!("sandbox client stopped reading"),
                Ok(count) => {
                    outgoing.drain(..count);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}
