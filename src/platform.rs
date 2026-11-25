#[cfg(target_os = "macos")]
use anyhow::bail;
use anyhow::Result;

/// Observe live members only. Failure is never interpreted as absence.
#[cfg(target_os = "macos")]
pub fn group_has_live_members(pgid: u32) -> Result<bool> {
    // proc_listpids(PROC_PGRP_ONLY) returns bytes. A retained zombie group leader
    // can make Darwin's killpg return EPERM even after every live member exited.
    Ok(group_members(pgid)?.iter().any(|&(_, live)| live))
}

/// Members of a process group as `(pid, live)`, where zombies are not live.
#[cfg(target_os = "macos")]
fn group_members(pgid: u32) -> Result<Vec<(u32, bool)>> {
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
            return Ok(Vec::new());
        }
        if bytes as usize >= std::mem::size_of_val(pids.as_slice()) {
            if pids.len() >= 65_536 {
                bail!("process group exceeds inspection bound");
            }
            pids.resize(pids.len() * 2, 0);
            continue;
        }
        let mut members = Vec::new();
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
                if info.pbi_pgid == pgid {
                    members.push((pid as u32, info.pbi_status != libc::SZOMB));
                }
            } else {
                let error = std::io::Error::last_os_error();
                if !matches!(error.raw_os_error(), Some(0 | libc::ESRCH)) {
                    return Err(error.into());
                }
            }
        }
        return Ok(members);
    }
}

/// Members of a process group as `(pid, live)`, read from procfs, where
/// zombies are not live. Requires procfs from this process's own PID
/// namespace, so its numbers mean what `getpgrp` means here. A process that
/// exits between listing and reading is not a member.
#[cfg(target_os = "linux")]
fn group_members(pgid: u32) -> Result<Vec<(u32, bool)>> {
    if !procfs_reports_own_namespace() {
        anyhow::bail!("procfs does not report this PID namespace");
    }
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_str().and_then(positive_decimal) else {
            continue;
        };
        let Some(stat) = read_bounded(&entry.path().join("stat"), SMALL_LIMIT) else {
            continue;
        };
        let Some((state, group)) = stat_state_and_group(&stat) else {
            continue;
        };
        if group == pgid {
            members.push((pid, !matches!(state, "Z" | "X")));
        }
    }
    Ok(members)
}

/// State (field 3) and process group (field 5) of a `/proc/PID/stat` line,
/// counted after the command name's final closing parenthesis.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn stat_state_and_group(stat: &str) -> Option<(&str, u32)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?;
    fields.next()?;
    Some((state, positive_decimal(fields.next()?)?))
}

/// Whether the process group `pgid` has a live member other than `own`,
/// which must itself be observed in the group as a sanity check of the view.
/// Zombies are not live. Any error means members may be present: callers
/// must then keep waiting rather than shorten a shutdown grace on a guess.
pub fn group_has_other_live_members(pgid: u32, own: u32) -> Result<bool> {
    let members = group_members(pgid)?;
    if !members.iter().any(|&(pid, _)| pid == own) {
        anyhow::bail!("this process was not observed in its process group");
    }
    Ok(members.iter().any(|&(pid, live)| live && pid != own))
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

fn socket_address(path: &Path) -> io::Result<(libc::sockaddr_un, usize)> {
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
    Ok((address, length))
}

/// Which step of a Unix control-socket capability check failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketStep {
    Create,
    Bind,
}

/// Create and bind a temporary Unix socket at `path`, then remove it. The
/// socket never listens or accepts. Only a socket this call bound is removed,
/// on every return path, so an existing file at `path` is never deleted.
pub fn probe_unix_bind(path: &Path) -> Result<(), (SocketStep, io::Error)> {
    struct Remove<'a>(&'a Path);
    impl Drop for Remove<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
        }
    }
    let (address, length) = socket_address(path).map_err(|e| (SocketStep::Bind, e))?;
    let fd = unix_socket().map_err(|e| (SocketStep::Create, e))?;
    let result = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length as libc::socklen_t,
        )
    };
    if result == -1 {
        return Err((SocketStep::Bind, io::Error::last_os_error()));
    }
    let _remove = Remove(path);
    drop(fd);
    Ok(())
}

/// Whether an error is an access-control denial rather than a missing or
/// failed endpoint.
pub fn is_denial(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EPERM | libc::EACCES))
}

/// Opaque identity of the PID number space this process observes, or `None`
/// when it cannot be established completely. Linux only.
///
/// A PID namespace inode alone is not a safe identity: hosts can recycle it
/// between short-lived per-call namespaces. The token therefore combines the
/// boot ID, this process's PID namespace link and the start time (clock
/// ticks since boot) of that namespace's PID 1 as seen through `/proc`. Only
/// a digest is kept. The procfs `stat` and `status` files read here contain
/// process names, but no name is retained; environment is never read.
///
/// `/proc/1` is that namespace's init only when the procfs mount belongs to
/// this process's own PID namespace. `NSpid` in `/proc/self/status` lists this
/// process's PID in each namespace from the procfs mount's namespace down to
/// its own, so it must hold exactly one PID equal to `std::process::id()`, and
/// `/proc/self/stat` must report the same PID. Otherwise procfs was mounted
/// from an ancestor namespace, is missing or is unreadable, and there is no
/// context. Root-owned namespace links of PID 1 are not required.
#[cfg(target_os = "linux")]
pub fn pid_context() -> Option<&'static str> {
    static CONTEXT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CONTEXT
        .get_or_init(|| {
            if !procfs_reports_own_namespace() {
                return None;
            }
            let boot = read_bounded(Path::new("/proc/sys/kernel/random/boot_id"), SMALL_LIMIT)?;
            let namespace = std::fs::read_link("/proc/self/ns/pid").ok()?;
            let namespace = namespace.to_str()?;
            let init = init_start_ticks(&read_bounded(Path::new("/proc/1/stat"), SMALL_LIMIT)?)?;
            pid_context_token(boot.trim(), namespace, init)
        })
        .as_deref()
}

#[cfg(not(target_os = "linux"))]
pub fn pid_context() -> Option<&'static str> {
    None
}

/// Whether procfs reports this process from its own PID namespace; see
/// `pid_context`. Cached for the process lifetime.
#[cfg(target_os = "linux")]
fn procfs_reports_own_namespace() -> bool {
    static OWN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OWN.get_or_init(|| {
        match (
            read_bounded(Path::new("/proc/self/status"), STATUS_LIMIT),
            read_bounded(Path::new("/proc/self/stat"), SMALL_LIMIT),
        ) {
            (Some(status), Some(stat)) => {
                procfs_is_own_namespace(&status, &stat, std::process::id())
            }
            _ => false,
        }
    })
}

/// Opaque digests of this machine and of its current boot.
pub struct BootContext {
    pub machine: String,
    pub boot: String,
}

/// This machine's identity and current boot, or `None` when either cannot be
/// established completely. Only digests are kept.
///
/// Linux uses the machine ID together with the host name, so two hosts
/// sharing a state directory are told apart even when one was cloned from the
/// other's image, and the kernel's per-boot random `boot_id`. macOS uses the
/// hardware UUID and the per-boot `kern.bootsessionuuid`. A changed host
/// name or an unreadable source only withholds the inference built on this.
pub fn boot_context() -> Option<&'static BootContext> {
    static CONTEXT: std::sync::OnceLock<Option<BootContext>> = std::sync::OnceLock::new();
    CONTEXT
        .get_or_init(|| {
            let (machine, boot) = boot_sources()?;
            let machine: Vec<&[u8]> = machine.iter().map(Vec::as_slice).collect();
            Some(BootContext {
                machine: context_digest(b"proclease machine context v1\0", &machine),
                boot: context_digest(b"proclease boot context v1\0", &[&boot]),
            })
        })
        .as_ref()
}

/// Whether a record's saved machine and boot prove that it was written by an
/// earlier boot of this machine, so none of its processes can still run.
/// Requires both saved values and a complete current context, an identical
/// machine and a different boot. Anything else, including records written
/// before these fields existed and records from another machine sharing the
/// state directory, proves nothing.
pub fn from_earlier_boot(machine: Option<&str>, boot: Option<&str>) -> bool {
    earlier_boot(machine, boot, boot_context())
}

fn earlier_boot(machine: Option<&str>, boot: Option<&str>, current: Option<&BootContext>) -> bool {
    matches!(
        (machine, boot, current),
        (Some(machine), Some(boot), Some(current))
            if machine == current.machine && boot != current.boot
    )
}

/// Domain-separated digest of length-prefixed fields.
fn context_digest(domain: &[u8], fields: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(domain);
    for field in fields {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    format!("v1:{:x}", digest.finalize())
}

/// A trimmed identifier of 1 to 64 printable ASCII characters.
fn identifier(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    (!text.is_empty() && text.len() <= 64 && text.bytes().all(|c| c.is_ascii_graphic()))
        .then(|| text.as_bytes().to_vec())
}

#[cfg(target_os = "linux")]
fn boot_sources() -> Option<(Vec<Vec<u8>>, Vec<u8>)> {
    let machine_id = ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .into_iter()
        .find_map(|path| read_bounded(Path::new(path), SMALL_LIMIT))?;
    let machine_id = machine_id.trim();
    // An uninitialized or malformed machine ID is not an identity.
    if machine_id.len() != 32
        || !machine_id
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        || machine_id.bytes().all(|c| c == b'0')
    {
        return None;
    }
    let mut host = [0u8; 256];
    if unsafe { libc::gethostname(host.as_mut_ptr().cast(), host.len()) } != 0 {
        return None;
    }
    let host = &host[..host.iter().position(|&c| c == 0)?];
    if host.is_empty() {
        return None;
    }
    let boot = identifier(&read_bounded(
        Path::new("/proc/sys/kernel/random/boot_id"),
        SMALL_LIMIT,
    )?)?;
    Some((vec![machine_id.as_bytes().to_vec(), host.to_vec()], boot))
}

#[cfg(target_os = "macos")]
fn boot_sources() -> Option<(Vec<Vec<u8>>, Vec<u8>)> {
    let mut uuid = [0u8; 16];
    let wait = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    if unsafe { libc::gethostuuid(uuid.as_mut_ptr(), &wait) } != 0 || uuid.iter().all(|&b| b == 0) {
        return None;
    }
    let mut value = [0u8; 128];
    let mut size = value.len();
    let name = c"kern.bootsessionuuid";
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    let value = &value[..size.min(value.len())];
    let text =
        std::str::from_utf8(&value[..value.iter().position(|&c| c == 0).unwrap_or(value.len())])
            .ok()?;
    Some((vec![uuid.to_vec()], identifier(text)?))
}

/// Bound for small single-line procfs files.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const SMALL_LIMIT: u64 = 4096;
/// Bound for `/proc/self/status`. Its `Groups:` line precedes `NSpid:` and
/// can list up to 65,536 supplementary groups (about 720 KiB), so a small
/// bound could cut `NSpid` off.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const STATUS_LIMIT: u64 = 1_048_576;

/// The whole file as text, or `None` if it is unreadable, not UTF-8 or longer
/// than `limit` bytes. Oversized input is rejected rather than truncated.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn read_bounded(path: &Path, limit: u64) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(limit.saturating_add(1))
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= limit).then_some(text)
}

/// Whether procfs reports this process from its own PID namespace: `status`
/// has exactly one `NSpid:` line holding exactly one positive decimal PID,
/// and both it and the leading PID of `stat` equal `pid`. Multiple levels,
/// a missing or repeated line and malformed values are all rejected.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn procfs_is_own_namespace(status: &str, stat: &str, pid: u32) -> bool {
    let positive = positive_decimal;
    let mut lines = status
        .lines()
        .filter_map(|line| line.strip_prefix("NSpid:"));
    let (Some(nspid), None) = (lines.next(), lines.next()) else {
        return false;
    };
    let mut levels = nspid.split_whitespace();
    let single = match (levels.next(), levels.next()) {
        (Some(value), None) => positive(value),
        _ => None,
    };
    let stat_pid = stat.split_once(' ').and_then(|(value, _)| positive(value));
    single == Some(pid) && stat_pid == Some(pid)
}

/// A positive decimal number without sign or other characters.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn positive_decimal(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|&value| value > 0)
}

/// Start time (field 22) from a `/proc/PID/stat` line, in clock ticks since
/// boot. Zero is a valid value (PID 1 can start within the first tick). The
/// command name in field 2 may contain spaces and parentheses, so fields are
/// counted only after its final closing parenthesis, where field 3 begins.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn init_start_ticks(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Domain-separated digest of a complete PID context; incomplete input is
/// rejected rather than producing a weaker token. A parsed start time is
/// complete, including zero.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn pid_context_token(boot: &str, namespace: &str, init_start: u64) -> Option<String> {
    use sha2::{Digest, Sha256};
    let valid_namespace = namespace
        .strip_prefix("pid:[")
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|inode| !inode.is_empty() && inode.bytes().all(|c| c.is_ascii_digit()));
    if boot.is_empty() || boot.len() > 64 || !valid_namespace {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"proclease pid context v1\0");
    for field in [
        boot.as_bytes(),
        namespace.as_bytes(),
        &init_start.to_be_bytes(),
    ] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    Some(format!("v1:{:x}", digest.finalize()))
}

/// Whether PIDs saved with `recorded` may be interpreted by this process.
///
/// Linux requires a complete, identical context; records without one,
/// including those written by earlier versions, fail closed. macOS has one
/// PID space per boot and keeps its existing behavior for records without a
/// context; a context there can only come from another system.
pub fn pid_numbers_comparable(recorded: Option<&str>) -> bool {
    if cfg!(target_os = "linux") {
        contexts_match(recorded, pid_context())
    } else {
        recorded.is_none()
    }
}

fn contexts_match(recorded: Option<&str>, current: Option<&str>) -> bool {
    matches!((recorded, current), (Some(recorded), Some(current)) if recorded == current)
}

/// Connect to a Unix stream socket without blocking past `deadline`.
///
/// A blocking connect has no timeout: on Linux it waits while the listener's
/// backlog is full. This uses a non-blocking socket, polls an in-progress
/// connection, and retries a full backlog (`EAGAIN`) only until the deadline.
/// The returned stream is non-blocking.
pub fn connect_unix(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    let (address, length) = socket_address(path)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_context_requires_complete_identical_identity() {
        let token = pid_context_token("boot-a", "pid:[4026532001]", 42).unwrap();
        assert!(token.starts_with("v1:"));
        // Any component changes the token, including a recycled namespace
        // inode whose namespace init started at a different time.
        for other in [
            pid_context_token("boot-b", "pid:[4026532001]", 42),
            pid_context_token("boot-a", "pid:[4026532002]", 42),
            pid_context_token("boot-a", "pid:[4026532001]", 43),
        ] {
            assert_ne!(other.unwrap(), token);
        }
        for incomplete in [
            pid_context_token("", "pid:[1]", 42),
            pid_context_token("boot-a", "net:[1]", 42),
            pid_context_token("boot-a", "pid:[]", 42),
            pid_context_token("boot-a", "pid:[1x]", 42),
        ] {
            assert!(incomplete.is_none());
        }
        // Zero clock ticks since boot is a parsed, complete start time.
        let zero = pid_context_token("boot-a", "pid:[4026532001]", 0).unwrap();
        assert_ne!(zero, token);
        assert!(contexts_match(Some(&token), Some(&token)));
        assert!(!contexts_match(Some(&token), Some("v1:other")));
        assert!(!contexts_match(None, Some(&token)));
        assert!(!contexts_match(Some(&token), None));
        assert!(!contexts_match(None, None));
    }

    #[test]
    fn init_start_time_is_counted_after_the_command_name() {
        let tail = "S 0 1 1 0 -1 4194560 1 2 3 4 5 6 7 8 20 0 1 0 1234 5 6";
        assert_eq!(init_start_ticks(&format!("1 (init) {tail}")), Some(1234));
        assert_eq!(
            init_start_ticks(&format!("1 (a) b (c d) {tail}")),
            Some(1234)
        );
        assert_eq!(init_start_ticks("1 (init) S 0"), None);
        assert_eq!(init_start_ticks("no parenthesis"), None);
        // Zero is valid; malformed values are not.
        let zero = tail.replace(" 1234 ", " 0 ");
        assert_eq!(init_start_ticks(&format!("1 (init) {zero}")), Some(0));
        for bad in [" -1 ", " x ", " 12x "] {
            let tail = tail.replace(" 1234 ", bad);
            assert_eq!(init_start_ticks(&format!("1 (init) {tail}")), None, "{bad}");
        }
    }

    #[test]
    fn long_supplementary_groups_do_not_hide_nspid() {
        let mut builder = tempfile::Builder::new();
        builder.prefix("g");
        let dir = match std::env::var_os("PROCLEASE_TEST_ROOT") {
            Some(root) => {
                std::fs::create_dir_all(&root).unwrap();
                builder.tempdir_in(root).unwrap()
            }
            None => builder.tempdir().unwrap(),
        };
        // The kernel maximum of 65,536 groups with large IDs, before NSpid.
        let groups: String = (0..65_536u32)
            .map(|index| format!(" {}", 4_000_000_000u32 + index))
            .collect();
        let status = format!("Name:\tproclease\nPid:\t42\nGroups:{groups}\nNSpid:\t42\n");
        assert!(status.len() > 700_000 && status.len() as u64 <= STATUS_LIMIT);
        let stat = "42 (proclease) S 1 42 42 0 -1";
        let path = dir.path().join("status");
        std::fs::write(&path, &status).unwrap();
        let read = read_bounded(&path, STATUS_LIMIT).unwrap();
        assert!(procfs_is_own_namespace(&read, stat, 42));
        // The former 4 KiB bound would have dropped NSpid.
        assert!(read_bounded(&path, SMALL_LIMIT).is_none());
        let truncated: String = status.chars().take(4096).collect();
        assert!(!procfs_is_own_namespace(&truncated, stat, 42));
        // Incomplete status: NSpid line cut short or absent.
        let cut = format!("Name:\tproclease\nGroups:{groups}\nNSpid:");
        assert!(!procfs_is_own_namespace(&cut, stat, 42));
        // Exactly at the limit is accepted; one byte over is rejected.
        std::fs::write(&path, vec![b'a'; SMALL_LIMIT as usize]).unwrap();
        assert!(read_bounded(&path, SMALL_LIMIT).is_some());
        std::fs::write(&path, vec![b'a'; SMALL_LIMIT as usize + 1]).unwrap();
        assert!(read_bounded(&path, SMALL_LIMIT).is_none());
        assert!(read_bounded(&dir.path().join("missing"), SMALL_LIMIT).is_none());
    }

    #[test]
    fn procfs_must_belong_to_this_pid_namespace() {
        let status =
            |nspid: &str| format!("Name:\tproclease\nPid:\t42\nNSpid:{nspid}\nNSpgid:\t42\n");
        let stat = "42 (proclease) S 1 42 42 0 -1";
        // Matched: one level, equal to this process's PID in both files.
        assert!(procfs_is_own_namespace(&status("\t42"), stat, 42));
        // procfs from an ancestor namespace lists every level.
        assert!(!procfs_is_own_namespace(&status("\t9042\t42"), stat, 42));
        assert!(!procfs_is_own_namespace(&status("\t42\t3"), stat, 42));
        // Malformed or mismatched values.
        for nspid in ["", "\t", "\t0", "\t-42", "\t+42", "\t42x", "\tx", "\t43"] {
            assert!(
                !procfs_is_own_namespace(&status(nspid), stat, 42),
                "{nspid:?}"
            );
        }
        // Missing or repeated line (kernels without NSpid fail closed).
        assert!(!procfs_is_own_namespace(
            "Name:\tproclease\nPid:\t42\n",
            stat,
            42
        ));
        assert!(!procfs_is_own_namespace(
            &format!("{}NSpid:\t42\n", status("\t42")),
            stat,
            42
        ));
        // `/proc/self/stat` must report the same PID.
        for other in ["43 (proclease) S", "x (proclease) S", "", "42"] {
            assert!(
                !procfs_is_own_namespace(&status("\t42"), other, 42),
                "{other:?}"
            );
        }
    }

    #[test]
    fn records_without_context_keep_platform_behavior() {
        if cfg!(target_os = "linux") {
            // Earlier Linux records cannot prove their PID context.
            assert!(!pid_numbers_comparable(None));
            if let Some(current) = pid_context() {
                assert!(pid_numbers_comparable(Some(current)));
            }
        } else {
            assert!(pid_context().is_none());
            assert!(pid_numbers_comparable(None));
        }
        assert!(!pid_numbers_comparable(Some("v1:from-another-context")));
    }

    #[test]
    fn earlier_boot_requires_the_same_machine_and_a_different_boot() {
        let current = BootContext {
            machine: "v1:machine".into(),
            boot: "v1:boot".into(),
        };
        let check = |machine, boot| earlier_boot(machine, boot, Some(&current));
        assert!(check(Some("v1:machine"), Some("v1:earlier")));
        // The same boot, another machine or incomplete records prove nothing.
        assert!(!check(Some("v1:machine"), Some("v1:boot")));
        assert!(!check(Some("v1:other"), Some("v1:earlier")));
        assert!(!check(None, Some("v1:earlier")));
        assert!(!check(Some("v1:machine"), None));
        assert!(!earlier_boot(Some("v1:machine"), Some("v1:earlier"), None));
        // A current context, when available, is complete and stable.
        if let Some(context) = boot_context() {
            assert!(context.machine.starts_with("v1:") && context.boot.starts_with("v1:"));
            assert!(!from_earlier_boot(
                Some(&context.machine),
                Some(&context.boot)
            ));
        }
    }

    #[test]
    fn other_group_members_exclude_the_caller_and_zombies() {
        use std::os::unix::process::CommandExt;
        #[cfg(target_os = "linux")]
        if !procfs_reports_own_namespace() {
            return;
        }
        struct Owned(std::process::Child);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let spawn = |group: i32| {
            Owned(
                std::process::Command::new("sleep")
                    .arg("30")
                    .process_group(group)
                    .spawn()
                    .unwrap(),
            )
        };
        let mut leader = spawn(0);
        let pgid = leader.0.id();
        assert!(!group_has_other_live_members(pgid, pgid).unwrap());
        let mut member = spawn(pgid as i32);
        assert!(group_has_other_live_members(pgid, pgid).unwrap());
        // An unreaped, exited member is a zombie, not a live member.
        member.0.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while group_has_other_live_members(pgid, pgid).unwrap() {
            assert!(Instant::now() < deadline, "zombie counted as live");
            thread::sleep(Duration::from_millis(10));
        }
        member.0.wait().unwrap();
        // A view that does not include the caller is never trusted.
        leader.0.kill().unwrap();
        leader.0.wait().unwrap();
        assert!(group_has_other_live_members(pgid, pgid).is_err());
    }

    #[test]
    fn bind_probe_removes_only_its_own_socket() {
        let mut builder = tempfile::Builder::new();
        builder.prefix("b");
        let dir = match std::env::var_os("PROCLEASE_TEST_ROOT") {
            Some(root) => {
                std::fs::create_dir_all(&root).unwrap();
                builder.tempdir_in(root).unwrap()
            }
            None => builder.tempdir().unwrap(),
        };
        let path = dir.path().join("probe");
        probe_unix_bind(&path).unwrap();
        assert!(std::fs::symlink_metadata(&path).is_err());
        std::fs::write(&path, b"keep").unwrap();
        let (step, error) = probe_unix_bind(&path).unwrap_err();
        assert_eq!(step, SocketStep::Bind);
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        let (step, _) = probe_unix_bind(&dir.path().join("missing/probe")).unwrap_err();
        assert_eq!(step, SocketStep::Bind);
    }
}
