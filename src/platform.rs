#[cfg(target_os = "macos")]
use anyhow::bail;
use anyhow::Result;

/// Observe live members only. Failure is never interpreted as absence.
#[cfg(target_os = "macos")]
pub fn group_has_live_members(pgid: u32) -> Result<bool> {
    // proc_listpids(PROC_PGRP_ONLY) returns bytes. A retained zombie group leader
    // can make Darwin's killpg return EPERM even after every live member exited.
    let mut pids = vec![0i32; 256];
    loop {
        unsafe {
            *libc::__error() = 0;
        }
        let bytes = unsafe {
            libc::proc_listpids(
                2,
                pgid,
                pids.as_mut_ptr().cast(),
                std::mem::size_of_val(pids.as_slice()) as i32,
            )
        };
        if bytes < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if bytes == 0 {
            let error = std::io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(0 | libc::ESRCH)) {
                return Err(error.into());
            }
            return Ok(false);
        }
        if bytes as usize >= std::mem::size_of_val(pids.as_slice()) {
            if pids.len() >= 65_536 {
                bail!("process group exceeds inspection bound");
            }
            pids.resize(pids.len() * 2, 0);
            continue;
        }
        for &pid in pids
            .iter()
            .take(bytes as usize / std::mem::size_of::<i32>())
            .filter(|&&pid| pid > 0)
        {
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            unsafe {
                *libc::__error() = 0;
            }
            let size = std::mem::size_of_val(&info) as i32;
            let result = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    size,
                )
            };
            if result == size {
                if info.pbi_pgid == pgid && info.pbi_status != libc::SZOMB {
                    return Ok(true);
                }
            } else {
                let error = std::io::Error::last_os_error();
                if !matches!(error.raw_os_error(), Some(0 | libc::ESRCH)) {
                    return Err(error.into());
                }
            }
        }
        return Ok(false);
    }
}

use std::{
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    },
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "deadline reached")
}

/// Wait for readiness until an absolute deadline. Readiness that is an error
/// or hang-up is reported by the following I/O call.
fn wait_ready(fd: RawFd, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timed_out());
        }
        // Round up so a sub-millisecond remainder still waits, then re-check.
        let timeout = left.as_millis().saturating_add(1).min(i32::MAX as u128) as libc::c_int;
        let mut poll = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        match unsafe { libc::poll(&mut poll, 1, timeout) } {
            -1 => {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            0 => continue,
            _ => return Ok(()),
        }
    }
}

fn unix_socket() -> io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
    #[cfg(not(target_os = "linux"))]
    let kind = libc::SOCK_STREAM;
    let raw = unsafe { libc::socket(libc::AF_UNIX, kind, 0) };
    if raw == -1 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    #[cfg(not(target_os = "linux"))]
    unsafe {
        if libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) == -1
            || libc::fcntl(
                raw,
                libc::F_SETFL,
                libc::fcntl(raw, libc::F_GETFL) | libc::O_NONBLOCK,
            ) == -1
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(fd)
}

/// Connect to a Unix stream socket without blocking past `deadline`.
///
/// A blocking connect has no timeout: on Linux it waits while the listener's
/// backlog is full. This uses a non-blocking socket, polls an in-progress
/// connection, and retries a full backlog (`EAGAIN`) only until the deadline.
/// The returned stream is non-blocking.
pub fn connect_unix(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path is too long",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, &byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = byte as libc::c_char;
    }
    let length = std::mem::size_of::<libc::sockaddr_un>();
    #[cfg(target_os = "macos")]
    {
        address.sun_len = length as u8;
    }
    loop {
        let fd = unix_socket()?;
        let result = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                length as libc::socklen_t,
            )
        };
        if result == 0 {
            return Ok(UnixStream::from(fd));
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINPROGRESS | libc::EINTR) => {
                wait_ready(fd.as_raw_fd(), libc::POLLOUT, deadline)?;
                let mut pending: libc::c_int = 0;
                let mut size = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                if unsafe {
                    libc::getsockopt(
                        fd.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        (&mut pending as *mut libc::c_int).cast(),
                        &mut size,
                    )
                } == -1
                {
                    return Err(io::Error::last_os_error());
                }
                if pending != 0 {
                    return Err(io::Error::from_raw_os_error(pending));
                }
                return Ok(UnixStream::from(fd));
            }
            Some(libc::EAGAIN) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(timed_out());
                }
                thread::sleep(left.min(Duration::from_millis(5)));
            }
            _ => return Err(error),
        }
    }
}

/// Write one request and read one newline-terminated reply of at most `limit`
/// bytes, all before an absolute `deadline`. Trickled bytes cannot extend it.
pub fn exchange_line(
    stream: &mut UnixStream,
    request: &[u8],
    deadline: Instant,
    limit: usize,
) -> io::Result<Vec<u8>> {
    stream.set_nonblocking(true)?;
    let fd = stream.as_raw_fd();
    let mut written = 0;
    while written < request.len() {
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        match stream.write(&request[written..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(size) => written += size,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_ready(fd, libc::POLLOUT, deadline)?
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
            Err(error) => return Err(error),
        }
    }
    let mut reply = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        match stream.read(&mut chunk) {
            Ok(0) if reply.is_empty() => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(0) => return Ok(reply),
            Ok(size) => {
                let start = reply.len();
                reply.extend_from_slice(&chunk[..size]);
                if let Some(end) = reply[start..].iter().position(|&byte| byte == b'\n') {
                    reply.truncate(start + end);
                }
                if reply.len() > limit {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "reply exceeds size limit",
                    ));
                }
                if reply.len() < start + size {
                    return Ok(reply);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_ready(fd, libc::POLLIN, deadline)?
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
            Err(error) => return Err(error),
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn group_has_live_members(pgid: u32) -> Result<bool> {
    let result = unsafe { libc::kill(-(pgid as i32), 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(error.into())
    }
}
