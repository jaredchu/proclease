use crate::{
    model::{extend_deadline, now_ms, Control, Launch, Reason, Record, Reply, State},
    store::Store,
};
use anyhow::{bail, Context, Result};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileExt, OpenOptionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const TICK: Duration = Duration::from_millis(25);

fn detach(command: &mut Command) {
    // Only an async-signal-safe syscall is made between fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGCHLD, libc::SIG_DFL);
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

/// A post-spawn error must not abandon a process group. The keeper retains its
/// child leader until this guard is disarmed; the guardian is itself the leader.
struct CleanupOnDrop {
    pgid: u32,
    grace: Duration,
    armed: bool,
}

impl CleanupOnDrop {
    fn new(pgid: u32, grace_ms: u64) -> Self {
        Self {
            pgid,
            grace: Duration::from_millis(grace_ms),
            armed: true,
        }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CleanupOnDrop {
    fn drop(&mut self) {
        if self.armed {
            // Neither a failed state write nor a failed TERM may skip KILL.
            let _ = signal_group(self.pgid, libc::SIGTERM);
            thread::sleep(self.grace);
            if let Err(error) = signal_group(self.pgid, libc::SIGKILL) {
                eprintln!("group cleanup could not be verified: {error:#}");
            }
        }
    }
}

fn internal(store: &Store, kind: &str, id: &str) -> Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--state-dir"])
        .arg(&store.root)
        .args([kind, id]);
    let diagnostic = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(store.job(id)?.join("supervisor.log"))?;
    command.stdout(Stdio::null()).stderr(diagnostic);
    Ok(command)
}

pub fn start_keeper(store: &Store, id: &str) -> Result<()> {
    let mut command = internal(store, "__keeper", id)?;
    command.stdin(Stdio::null());
    detach(&mut command);
    let mut keeper = command.spawn().context("cannot start supervisor")?;
    let started = Instant::now();
    loop {
        let record = store.record(id)?;
        if record.state != State::Starting {
            return Ok(());
        }
        if let Some(status) = keeper.try_wait()? {
            bail!("supervisor exited during startup: {status}");
        }
        if started.elapsed() > Duration::from_secs(5) {
            bail!("supervisor startup timed out; inspect job {id} before retrying");
        }
        thread::sleep(TICK);
    }
}

fn child_exited_without_reaping(child: &Child) -> Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // Keep the session leader unreaped until its group is killed. This prevents
    // its numeric ID being reused between checking and signalling the group.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(error.into());
    }
    // Darwin can set si_signo even when WNOHANG reports no exited child.
    // POSIX defines si_pid == 0 as the no-event discriminator.
    Ok(unsafe { info.si_pid() } == child.id() as i32)
}

fn signal_group(pid: u32, signal: i32) -> Result<()> {
    if pid <= 1 {
        bail!("refusing invalid process group");
    }
    let result = unsafe { libc::kill(-(pid as i32), signal) };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EPERM)
            && matches!(crate::platform::group_has_live_members(pid), Ok(false))
        {
            return Ok(());
        }
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    Ok(())
}

pub fn keeper(store: &Store, id: &str) -> Result<()> {
    let launch = store.launch(id)?;
    let mut record = store.record(id)?;
    record.keeper_pid = std::process::id();
    store.save(&record)?;
    let mut command = internal(store, "__guard", id)?;
    command.stdin(Stdio::piped());
    detach(&mut command);
    let mut guardian = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            record.reason = Some(Reason::LaunchFailed);
            record.error = Some(error.to_string());
            record.finish();
            store.save(&record)?;
            return Err(error.into());
        }
    };
    let mut cleanup = CleanupOnDrop::new(guardian.id(), launch.grace_ms);
    // The pipe stays open for this scope. If the keeper is killed, EOF tells
    // the guardian to shut down; it never waits indefinitely for a dead parent.
    let _liveness = guardian
        .stdin
        .take()
        .context("missing guardian liveness pipe")?;
    loop {
        if child_exited_without_reaping(&guardian)? {
            break;
        }
        thread::sleep(TICK);
    }
    record = store.record(id)?;
    if record.state != State::Stopping && record.state.active() {
        record.reason = Some(Reason::SupervisionLost);
        record.state = State::Stopping;
        record.error =
            Some("guardian exited unexpectedly; group cleanup enforced by keeper".into());
        store.save(&record)?;
        signal_group(guardian.id(), libc::SIGTERM)?;
        thread::sleep(Duration::from_millis(launch.grace_ms));
    }
    // The guardian is an unreaped child, so this cannot target a reused PGID.
    signal_group(guardian.id(), libc::SIGKILL)?;
    // Never signal this numeric group after its leader has been reaped.
    cleanup.disarm();
    guardian.wait()?;
    record.logs_truncated |= store.capture_truncated(id)?;
    if record.state == State::Stopping || record.state.active() {
        record.finish();
        store.save(&record)?;
    }
    let _ = std::fs::remove_file(store.job(id)?.join("ctl.sock"));
    // Final state and socket cleanup are complete. Collection may now include
    // this job even though this keeper has not returned yet.
    match store.prune(false, Some(id)) {
        Ok(report) => {
            for warning in report.warnings {
                eprintln!("history cleanup: {warning}");
            }
        }
        Err(error) => eprintln!("history cleanup deferred: {error:#}"),
    }
    Ok(())
}

struct LogSink {
    file: File,
    written: u64,
    limit: u64,
}

fn pump(
    mut reader: impl Read + Send + 'static,
    sink: Arc<Mutex<LogSink>>,
    truncated: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    capture_marker: Arc<File>,
) {
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            let size = match reader.read(&mut buffer) {
                Ok(0) => return,
                Ok(size) => size,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    failed.store(true, Ordering::Relaxed);
                    return;
                }
            };
            let mut sink = match sink.lock() {
                Ok(sink) => sink,
                Err(_) => {
                    failed.store(true, Ordering::Relaxed);
                    return;
                }
            };
            let writable = size.min(sink.limit.saturating_sub(sink.written) as usize);
            if writable < size && !truncated.load(Ordering::Relaxed) {
                // The marker is opened before service launch, so a shutdown
                // handler can disclose discarded output even after the final
                // lifecycle snapshot, or when the directory becomes unwritable.
                truncated.store(true, Ordering::Relaxed);
                if capture_marker.write_at(b"1", 0).is_err() {
                    failed.store(true, Ordering::Relaxed);
                }
            }
            if sink.file.write_all(&buffer[..writable]).is_err() {
                failed.store(true, Ordering::Relaxed);
                return;
            }
            sink.written += writable as u64;
        }
    });
}

fn read_control(stream: &mut UnixStream, deadline: Instant) -> Result<Option<Control>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 512];
    while Instant::now() < deadline {
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(None),
            Ok(size) => {
                bytes.extend_from_slice(&chunk[..size]);
                if let Some(end) = bytes.iter().position(|&byte| byte == b'\n') {
                    if end > 4096 {
                        bail!("control request exceeds 4096 bytes");
                    }
                    return Ok(Some(serde_json::from_slice(&bytes[..end])?));
                }
                if bytes.len() > 4096 {
                    bail!("control request exceeds 4096 bytes");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1))
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

fn write_reply(stream: &mut UnixStream, reply: &Reply, deadline: Instant) {
    let Ok(mut bytes) = serde_json::to_vec(reply) else {
        return;
    };
    bytes.push(b'\n');
    let mut written = 0;
    while written < bytes.len() && Instant::now() < deadline {
        match stream.write(&bytes[written..]) {
            Ok(0) => return,
            Ok(size) => written += size,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1))
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

fn serve_control(listener: &UnixListener, record: &mut Record, store: &Store) -> Result<()> {
    // Both the entire batch and each connection have absolute budgets. Byte
    // trickling or a client that stops reading cannot reset those budgets.
    let remaining = Duration::from_millis(record.expires_at_ms.saturating_sub(now_ms()));
    let batch_deadline = Instant::now() + TICK.min(remaining);
    for _ in 0..8 {
        if Instant::now() >= batch_deadline || now_ms() >= record.expires_at_ms {
            return Ok(());
        }
        let (mut stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if stream.set_nonblocking(true).is_err() {
            continue;
        }
        let deadline = batch_deadline.min(Instant::now() + Duration::from_millis(10));
        let request = match read_control(&mut stream, deadline) {
            Ok(Some(request)) => request,
            Ok(None) | Err(_) => continue,
        };
        let result: Result<()> = match request {
            Control::Stop if record.state == State::Running => {
                record.state = State::Stopping;
                record.reason = Some(Reason::Requested);
                Ok(())
            }
            Control::Stop => Err(anyhow::anyhow!("job is already stopping")),
            Control::Extend { by_ms } => extend_deadline(record, by_ms),
        };
        if result.is_ok() {
            store.save(record)?;
        }
        let reply = Reply {
            record: record.clone(),
            error: result.err().map(|error| error.to_string()),
        };
        // Persistence may consume the read batch budget. Give an accepted
        // operation a fresh, still absolute reply budget so it is not silently
        // committed without an acknowledgement during ordinary disk latency.
        write_reply(&mut stream, &reply, Instant::now() + TICK);
        if record.state == State::Stopping {
            return Ok(());
        }
    }
    Ok(())
}

pub fn guard(store: &Store, id: &str) -> Result<()> {
    let launch: Launch = store.launch(id)?;
    let dir = store.job(id)?;
    let mut record = store.record(id)?;
    record.guardian_pid = Some(std::process::id());
    store.save(&record)?;
    if unsafe { libc::getpgrp() } != std::process::id() as i32 {
        bail!("guardian must own its process group");
    }

    let interrupted = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(libc::SIGTERM, interrupted.clone())?;
    signal_hook::flag::register(libc::SIGINT, interrupted.clone())?;
    signal_hook::flag::register(libc::SIGHUP, interrupted.clone())?;
    let parent_gone = Arc::new(AtomicBool::new(false));
    let parent_flag = parent_gone.clone();
    thread::spawn(move || {
        let mut byte = [0u8; 1];
        loop {
            match std::io::stdin().read(&mut byte) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                _ => {
                    parent_flag.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
    });

    let listener =
        UnixListener::bind(dir.join("ctl.sock")).context("cannot bind control socket")?;
    listener.set_nonblocking(true)?;
    let log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("output.log"))?;
    let sink = Arc::new(Mutex::new(LogSink {
        file: log,
        written: 0,
        limit: launch.log_limit_bytes,
    }));
    let mut capture_marker = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("capture.status"))?;
    capture_marker.write_all(b"0")?;
    let capture_marker = Arc::new(capture_marker);
    let truncated = Arc::new(AtomicBool::new(false));
    let log_failed = Arc::new(AtomicBool::new(false));
    let mut command = Command::new(&launch.argv[0]);
    command
        .args(&launch.argv[1..])
        .current_dir(&launch.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The guardian has signal handlers; the actual service must receive the
    // normal Unix defaults. The service inherits the guardian's process group.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGHUP, libc::SIG_DFL);
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            record.reason = Some(Reason::LaunchFailed);
            record.error = Some(error.to_string());
            record.finish();
            store.save(&record)?;
            return Ok(());
        }
    };
    let _cleanup = CleanupOnDrop::new(std::process::id(), launch.grace_ms);
    record.command_pid = Some(child.id());
    record.state = State::Running;
    store.save(&record)?;
    pump(
        child.stdout.take().context("missing stdout")?,
        sink.clone(),
        truncated.clone(),
        log_failed.clone(),
        capture_marker.clone(),
    );
    pump(
        child.stderr.take().context("missing stderr")?,
        sink,
        truncated.clone(),
        log_failed.clone(),
        capture_marker,
    );
    loop {
        if parent_gone.load(Ordering::Relaxed) {
            record.reason = Some(Reason::SupervisionLost);
            break;
        }
        if interrupted.load(Ordering::Relaxed) {
            record.reason = Some(Reason::Signal);
            break;
        }
        if now_ms() >= record.expires_at_ms {
            record.reason = Some(Reason::Deadline);
            break;
        }
        if log_failed.load(Ordering::Relaxed) {
            record.reason = Some(Reason::SupervisionLost);
            record.error =
                Some("log capture failed; shutting down instead of running unobserved".into());
            break;
        }
        let capped = truncated.load(Ordering::Relaxed);
        if capped != record.logs_truncated {
            record.logs_truncated = capped;
            store.save(&record)?;
        }
        if let Some(status) = child.try_wait()? {
            record.reason = Some(Reason::CommandExit);
            record.exit_code = status.code();
            break;
        }
        serve_control(&listener, &mut record, store)?;
        if record.state == State::Stopping {
            break;
        }
        thread::sleep(TICK);
    }
    record.state = State::Stopping;
    record.logs_truncated = truncated.load(Ordering::Relaxed);
    // Even a state-write failure must proceed to group cleanup.
    let _ = store.save(&record);
    let pid = std::process::id();
    signal_group(pid, libc::SIGTERM)?;
    thread::sleep(Duration::from_millis(launch.grace_ms));
    // The guardian intentionally remains the group leader through escalation.
    // Killing the group also kills this guardian; the keeper finalizes state.
    signal_group(pid, libc::SIGKILL)?;
    bail!("group termination unexpectedly returned")
}

fn exchange_control(store: &Store, id: &str, action: &Control) -> Result<Reply> {
    let mut stream = UnixStream::connect(store.job(id)?.join("ctl.sock"))
        .context("supervisor unavailable; refusing to signal a PID from disk")?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    serde_json::to_writer(&mut stream, action)?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream.take(1_048_577)).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

fn wait_for_stop(store: &Store, id: &str, mut record: Record) -> Result<Record> {
    let grace = store.launch(id)?.grace_ms;
    let deadline = Instant::now() + Duration::from_millis(grace + 2_000);
    while record.state.active() && Instant::now() < deadline {
        thread::sleep(TICK);
        record = store.observed_record(id)?;
    }
    Ok(record)
}

fn existing_control_state(
    store: &Store,
    id: &str,
    action: &Control,
    record: &Record,
) -> Result<Option<Record>> {
    match record.state {
        State::Unknown => bail!("supervisor unavailable; refusing to signal a PID from disk"),
        State::Stopping => match action {
            Control::Stop => Ok(Some(wait_for_stop(store, id, record.clone())?)),
            Control::Extend { .. } => {
                bail!("only an unexpired running lease can be extended; job is already stopping")
            }
        },
        state if !state.active() => match action {
            Control::Stop => Ok(Some(record.clone())),
            Control::Extend { .. } => bail!("job has already finished"),
        },
        _ => Ok(None),
    }
}

pub fn control(store: &Store, id: &str, action: Control) -> Result<Record> {
    let record = store.observed_record(id)?;
    if let Some(record) = existing_control_state(store, id, &action, &record)? {
        return Ok(record);
    }
    let reply = match exchange_control(store, id, &action) {
        Ok(reply) => reply,
        Err(error) => {
            // Shutdown can begin after the initial observation. Reconcile that
            // state instead of exposing a timeout/EOF as a failed stop. Never
            // replay an extension whose response may have been lost.
            let latest = store.observed_record(id)?;
            if let Some(record) = existing_control_state(store, id, &action, &latest)? {
                return Ok(record);
            }
            return Err(error.context("control response unavailable; inspect list before retrying"));
        }
    };
    if let Some(error) = reply.error {
        bail!("{error}");
    }
    match action {
        Control::Stop => wait_for_stop(store, id, reply.record),
        Control::Extend { .. } => Ok(reply.record),
    }
}
