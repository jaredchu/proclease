use crate::{
    model::{extend_deadline, now_ms, Control, Launch, Reason, Record, Reply, State},
    store::{RetentionGuard, Store},
};
use anyhow::{bail, Context, Result};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{FileExt, OpenOptionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::Path,
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

/// Called with the root launch lock held, after launch metadata is committed.
pub fn start_keeper(store: &Store, id: &str) -> Result<()> {
    let spawned = internal(store, "__keeper", id).and_then(|mut command| {
        command.stdin(Stdio::null());
        detach(&mut command);
        command.spawn().context("cannot start supervisor")
    });
    let mut keeper = match spawned {
        Ok(keeper) => keeper,
        Err(error) => {
            // No keeper process exists, so neither a guardian nor the command
            // can have started. Finish the committed record under the launch
            // lock; a retry then sees a failed launch rather than an unknown one.
            let finalized = (|| -> Result<()> {
                let mut record = store.record(id)?;
                if record.state != State::Starting
                    || record.keeper_pid != 0
                    || record.guardian_pid.is_some()
                    || record.command_pid.is_some()
                {
                    bail!("unexpected lifecycle state after failed supervisor spawn");
                }
                record.reason = Some(Reason::LaunchFailed);
                record.error = Some(format!("{error:#}"));
                record.finish();
                store.save(&record)
            })();
            return match finalized {
                Ok(()) => Ok(()),
                Err(save) => {
                    Err(error.context(format!("failed launch was not recorded: {save:#}")))
                }
            };
        }
    };
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

/// Guardian resources prepared before the command is spawned.
struct GuardSetup {
    interrupted: Arc<AtomicBool>,
    parent_gone: Arc<AtomicBool>,
    listener: UnixListener,
    sink: Arc<Mutex<LogSink>>,
    capture_marker: Arc<File>,
}

fn prepare_guard(
    store: &Store,
    dir: &Path,
    launch: &Launch,
    record: &mut Record,
) -> Result<GuardSetup> {
    record.guardian_pid = Some(std::process::id());
    store.save(record)?;
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
    Ok(GuardSetup {
        interrupted,
        parent_gone,
        listener,
        sink,
        capture_marker: Arc::new(capture_marker),
    })
}

pub fn guard(store: &Store, id: &str) -> Result<()> {
    let launch: Launch = store.launch(id)?;
    let dir = store.job(id)?;
    let mut record = store.record(id)?;
    let GuardSetup {
        interrupted,
        parent_gone,
        listener,
        sink,
        capture_marker,
    } = match prepare_guard(store, &dir, &launch, &mut record) {
        Ok(setup) => setup,
        Err(error) => {
            // The command has not been spawned, so this is a launch failure,
            // not lost supervision. The keeper preserves this terminal state.
            record.reason = Some(Reason::LaunchFailed);
            record.error = Some(format!("{error:#}"));
            record.finish();
            if let Err(save) = store.save(&record) {
                return Err(error.context(format!("launch failure was not recorded: {save:#}")));
            }
            return Err(error);
        }
    };
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
    // Startup can be delayed by scheduling, suspension or slow setup. A lease
    // already past its persisted deadline must not execute the command. A
    // deadline reached between this check and exec is handled by the first
    // supervision tick below, which stops the anchored group.
    if now_ms() >= record.expires_at_ms {
        record.reason = Some(Reason::Deadline);
        record.error = Some("lease expired before the command was started".into());
        record.finish();
        store.save(&record)?;
        return Ok(());
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

/// Absolute bound for one control exchange: connect, request and reply.
pub const CONTROL_EXCHANGE: Duration = Duration::from_secs(2);
/// Largest accepted control reply.
const CONTROL_REPLY_LIMIT: usize = 1_048_576;
/// Largest shutdown grace accepted by `run`. Waits derived from launch
/// metadata are capped here, so altered metadata cannot overflow a deadline.
pub const MAX_GRACE_MS: u64 = 10_000;

/// Bound for confirming a stop after dispatch: grace (capped) plus two seconds.
fn stop_bound(grace_ms: u64) -> Duration {
    Duration::from_millis(grace_ms.min(MAX_GRACE_MS) + 2_000)
}

fn exchange_control(store: &Store, id: &str, action: &Control) -> Result<Reply> {
    let deadline = Instant::now() + CONTROL_EXCHANGE;
    let mut stream = crate::platform::connect_unix(&store.job(id)?.join("ctl.sock"), deadline)
        .context("supervisor unavailable; refusing to signal a PID from disk")?;
    let mut request = serde_json::to_vec(action)?;
    request.push(b'\n');
    let line = crate::platform::exchange_line(&mut stream, &request, deadline, CONTROL_REPLY_LIMIT)
        .context("control exchange did not complete within its 2s bound")?;
    Ok(serde_json::from_slice(&line)?)
}

fn wait_for_stop(store: &Store, id: &str, mut record: Record) -> Result<Record> {
    let deadline = Instant::now() + stop_bound(store.launch(id)?.grace_ms);
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

/// Maximum concurrent supervisor control exchanges during a bulk stop.
pub const BULK_WORKERS: usize = 8;
/// Bound for waiting on a selected `starting` job before its first Stop.
const BULK_STARTUP_WAIT: Duration = Duration::from_secs(6);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BulkStatus {
    /// Dry run: selected and reachable state, nothing sent.
    WouldStop,
    /// Reached a terminal state after selection.
    Finished,
    /// Supervisor unavailable; no signal was sent to any PID from disk.
    Unverified,
    /// Stop not confirmed within its bound, or control failed.
    Failed,
}

impl BulkStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WouldStop => "would_stop",
            Self::Finished => "finished",
            Self::Unverified => "unverified",
            Self::Failed => "failed",
        }
    }
    pub fn ok(self) -> bool {
        matches!(self, Self::WouldStop | Self::Finished)
    }
}

pub struct BulkResult {
    pub selected: Record,
    pub record: Record,
    pub status: BulkStatus,
    pub error: Option<String>,
}

/// A fixed bulk selection, its scan warnings and, unless it is a dry run,
/// the retention protection that keeps selected records until consumed.
pub type BulkSelection = (Vec<Record>, Vec<String>, Option<RetentionGuard>);

/// Fixed selection of active and unverified jobs matching every given filter.
/// The root launch lock is held only for the scan, which synchronizes with
/// in-flight launches and collection; it is released before any control.
/// With `protect`, retention protection is acquired under that lock before the
/// scan; failure to acquire it aborts before any job is contacted. The caller
/// must hold the returned guard until every result is captured.
pub fn bulk_selection(
    store: &Store,
    owner: Option<&str>,
    task: Option<&str>,
    protect: bool,
) -> Result<BulkSelection> {
    let (jobs, warnings, guard) = {
        let _lock = store.lock()?;
        let guard = if protect {
            Some(store.protect_retention_locked()?)
        } else {
            None
        };
        let (jobs, warnings) = store.list()?;
        (jobs, warnings, guard)
    };
    let selected = jobs
        .into_iter()
        .filter(|r| r.state.active() || r.state == State::Unknown)
        .filter(|r| owner.is_none_or(|owner| r.owner == owner))
        .filter(|r| task.is_none_or(|task| r.task.as_deref() == Some(task)))
        .collect();
    Ok((selected, warnings, guard))
}

enum Dispatched {
    Wait(Instant),
    Done(Box<Record>),
    Failed(Box<(Record, BulkStatus, String)>),
}

fn failed(record: Record, status: BulkStatus, error: String) -> Dispatched {
    Dispatched::Failed(Box::new((record, status, error)))
}

/// Send at most a bounded number of idempotent Stop requests to one job.
/// Starting jobs are polled until running; stopping jobs are only awaited.
fn dispatch_stop(store: &Store, selected: &Record) -> Dispatched {
    let id = selected.id.as_str();
    // Unreadable or altered launch metadata only affects this job's wait
    // bound, which is capped; the supervisor keeps its own grace.
    let bound = stop_bound(
        store
            .launch(id)
            .map(|launch| launch.grace_ms)
            .unwrap_or(MAX_GRACE_MS),
    );
    let wait_until = || Instant::now() + bound;
    let started = Instant::now();
    let mut attempts = 0;
    let mut last_error = String::new();
    // Failures report the selected or last actually observed record.
    let mut record = selected.clone();
    loop {
        record = match store.observed_record(id) {
            Ok(observed) => observed,
            Err(error) => return failed(record, BulkStatus::Failed, format!("{error:#}")),
        };
        match record.state {
            State::Unknown => {
                return failed(
                    record,
                    BulkStatus::Unverified,
                    "supervisor unavailable; refusing to signal a PID from disk".into(),
                )
            }
            State::Stopping => return Dispatched::Wait(wait_until()),
            state if !state.active() => return Dispatched::Done(Box::new(record)),
            State::Starting => {
                if started.elapsed() > BULK_STARTUP_WAIT {
                    return failed(
                        record,
                        BulkStatus::Failed,
                        "startup did not complete".into(),
                    );
                }
                thread::sleep(TICK);
                continue;
            }
            _ => (),
        }
        if attempts >= 2 {
            return failed(
                record,
                BulkStatus::Failed,
                format!("control response unavailable: {last_error}"),
            );
        }
        attempts += 1;
        match exchange_control(store, id, &Control::Stop) {
            // An "already stopping" reply also means shutdown is under way.
            Ok(_) => return Dispatched::Wait(wait_until()),
            Err(error) => last_error = format!("{error:#}"),
        }
    }
}

/// Stop a fixed selection. Control exchanges run on a small worker pool, then
/// all dispatched jobs share one polling loop, each bounded by its own grace
/// (capped at 10s) plus two seconds from dispatch. Each Stop exchange has an
/// absolute 2s bound and at most two are attempted per job. Unknown jobs are
/// never signalled. A missing or unreadable record is never a success; the
/// caller's retention guard keeps finished selected records collectable only
/// after this returns.
pub fn stop_selected(store: &Store, selected: Vec<Record>) -> Vec<BulkResult> {
    use std::sync::atomic::AtomicUsize;
    let next = AtomicUsize::new(0);
    let outcomes: Mutex<Vec<(usize, Dispatched)>> = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..BULK_WORKERS.min(selected.len()) {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(record) = selected.get(index) else {
                    return;
                };
                let outcome = if record.state == State::Unknown {
                    failed(
                        record.clone(),
                        BulkStatus::Unverified,
                        "supervisor unavailable; refusing to signal a PID from disk".into(),
                    )
                } else {
                    dispatch_stop(store, record)
                };
                outcomes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((index, outcome));
            });
        }
    });
    let mut outcomes = outcomes.into_inner().unwrap_or_else(|e| e.into_inner());
    outcomes.sort_by_key(|(index, _)| *index);
    let mut results: Vec<BulkResult> = Vec::with_capacity(selected.len());
    let mut pending = Vec::new();
    for ((index, outcome), selected) in outcomes.into_iter().zip(selected) {
        debug_assert_eq!(index, results.len());
        let result = match outcome {
            Dispatched::Done(record) => BulkResult {
                record: *record,
                selected,
                status: BulkStatus::Finished,
                error: None,
            },
            Dispatched::Wait(deadline) => {
                pending.push((results.len(), deadline));
                BulkResult {
                    record: selected.clone(),
                    selected,
                    status: BulkStatus::Failed,
                    error: None,
                }
            }
            Dispatched::Failed(failure) => {
                let (record, status, error) = *failure;
                BulkResult {
                    record,
                    selected,
                    status,
                    error: Some(error),
                }
            }
        };
        results.push(result);
    }
    while !pending.is_empty() {
        let now = Instant::now();
        pending.retain(|&(index, deadline)| {
            let result = &mut results[index];
            match store.observed_record(&result.selected.id) {
                Ok(record) if !record.state.active() && record.state != State::Unknown => {
                    result.record = record;
                    result.status = BulkStatus::Finished;
                    result.error = None;
                    false
                }
                Ok(record) if record.state == State::Unknown => {
                    result.record = record;
                    result.status = BulkStatus::Unverified;
                    result.error = Some("control endpoint lost; cleanup is not verified".into());
                    false
                }
                Ok(record) => {
                    result.record = record;
                    if now >= deadline {
                        result.error = Some(
                            "job did not finish within its stop bound (grace up to 10s, plus 2s)"
                                .into(),
                        );
                        return false;
                    }
                    true
                }
                Err(error) => {
                    if now >= deadline {
                        result.error = Some(format!("{error:#}"));
                        return false;
                    }
                    true
                }
            }
        });
        if !pending.is_empty() {
            thread::sleep(TICK);
        }
    }
    results
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    fn store() -> (tempfile::TempDir, Store) {
        let mut builder = tempfile::Builder::new();
        builder.prefix("u");
        let dir = match std::env::var_os("PROCLEASE_TEST_ROOT") {
            Some(root) => {
                std::fs::create_dir_all(&root).unwrap();
                builder.tempdir_in(root).unwrap()
            }
            None => builder.tempdir().unwrap(),
        };
        let store = Store::open(dir.path().join("s")).unwrap();
        (dir, store)
    }

    /// Commit launch and record files for a job with no process behind it.
    fn seed(store: &Store, id: &str, state: State) -> Record {
        let now = now_ms();
        let launch = Launch {
            id: id.into(),
            argv: vec!["/usr/bin/true".into()],
            cwd: "/".into(),
            owner: "o".into(),
            task: Some("t".into()),
            name: None,
            url: None,
            key: None,
            fingerprint: String::new(),
            created_at_ms: now,
            expires_at_ms: now + 60_000,
            grace_ms: 100,
            log_limit_bytes: 1024,
        };
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(store.root.join(id))
            .unwrap();
        crate::store::atomic_json(&store.root.join(id).join("launch.json"), &launch).unwrap();
        let mut record = Record::new(&launch);
        record.state = state;
        store.save(&record).unwrap();
        record
    }

    #[test]
    fn metadata_bounds_are_capped_and_record_identity_is_checked() {
        assert_eq!(stop_bound(u64::MAX), Duration::from_millis(12_000));
        assert_eq!(stop_bound(100), Duration::from_millis(2_100));
        let (_dir, store) = store();
        let mut record = seed(&store, "00000000000b", State::Running);
        record.id = "00000000000c".into();
        crate::store::atomic_json(&store.root.join("00000000000b/record.json"), &record).unwrap();
        let error = store.record("00000000000b").unwrap_err().to_string();
        assert!(error.contains("different job"), "{error}");
    }

    #[test]
    fn job_finished_before_dispatch_is_success_without_control() {
        let (_dir, store) = store();
        let mut record = seed(&store, "00000000000a", State::Running);
        let selected = record.clone();
        record.reason = Some(Reason::CommandExit);
        record.exit_code = Some(0);
        record.finish();
        store.save(&record).unwrap();
        // No control socket exists, so any attempted exchange would fail.
        let results = stop_selected(&store, vec![selected]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, BulkStatus::Finished);
        assert_eq!(results[0].selected.state, State::Running);
        assert_eq!(results[0].record.state, State::Exited);
        assert!(results[0].error.is_none());
    }

    #[test]
    fn observation_failure_preserves_the_selected_record() {
        let (_dir, store) = store();
        let selected = seed(&store, "00000000000d", State::Running);
        std::fs::write(store.root.join("00000000000d/record.json"), b"not json").unwrap();
        let results = stop_selected(&store, vec![selected]);
        assert_eq!(results[0].status, BulkStatus::Failed);
        assert_eq!(results[0].record.id, "00000000000d");
        assert_eq!(results[0].record.state, State::Running);
        assert_eq!(results[0].record.owner, "o");
        assert_eq!(results[0].record.task.as_deref(), Some("t"));
        assert!(results[0].error.is_some());
    }

    /// A finished, never-supervised record: collectable by normal retention.
    fn seed_history(store: &Store, id: &str, finished_at_ms: u64) {
        let mut record = seed(store, id, State::Starting);
        record.reason = Some(Reason::LaunchFailed);
        record.finish();
        record.finished_at_ms = Some(finished_at_ms);
        store.save(&record).unwrap();
    }

    /// Simulate the selected job's keeper recording completion. Its own
    /// completion is collectable by the prune that keeper runs next.
    fn complete_as_keeper(store: &Store, id: &str) {
        let mut record = store.record(id).unwrap();
        record.state = State::Stopping;
        record.reason = Some(Reason::Requested);
        record.keeper_pid = std::process::id();
        record.finish();
        store.save(&record).unwrap();
    }

    #[test]
    fn retention_guard_preserves_selection_until_last_holder_releases() {
        let (_dir, store) = store();
        // One hundred newer finished records put any completing selected job
        // first in line for count-based collection.
        let newer = now_ms() + 600_000;
        for index in 0..100 {
            seed_history(&store, &format!("{index:012x}"), newer + index);
        }
        let protected = "0000000000a1";
        let unprotected = "0000000000a2";
        seed(&store, protected, State::Running);
        seed(&store, unprotected, State::Running);
        // Bound, never-accepting endpoints keep both observably running.
        let _endpoints: Vec<_> = [protected, unprotected]
            .iter()
            .map(|id| UnixListener::bind(store.root.join(id).join("ctl.sock")).unwrap())
            .collect();

        // Control: a dry-run style selection takes no pin, so real pruning
        // collects its completed job before results could be read.
        let (dry, _, none) = bulk_selection(&store, None, None, false).unwrap();
        assert!(none.is_none());
        complete_as_keeper(&store, unprotected);
        let report = store.prune(false, Some(unprotected)).unwrap();
        assert!(!report.deferred);
        assert_eq!(report.removed, vec![unprotected.to_owned()]);
        let lost: Vec<_> = dry.into_iter().filter(|r| r.id == unprotected).collect();
        let results = stop_selected(&store, lost);
        assert_eq!(
            results[0].status,
            BulkStatus::Failed,
            "missing is never success"
        );
        assert!(results[0].error.is_some());

        // Two concurrent bulk callers each hold protection.
        let (selected, warnings, first) = bulk_selection(&store, None, None, true).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let (_, _, second) = bulk_selection(&store, None, None, true).unwrap();
        assert_eq!(selected.len(), 1);
        complete_as_keeper(&store, protected);
        let report = store.prune(false, Some(protected)).unwrap();
        assert!(
            report.deferred && report.removed.is_empty(),
            "{:?}",
            report.removed
        );
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(store.root.join(protected).exists());
        let results = stop_selected(&store, selected);
        assert_eq!(
            results[0].status,
            BulkStatus::Finished,
            "{:?}",
            results[0].error
        );
        assert_eq!(results[0].record.state, State::Stopped);

        drop(first);
        assert!(store.prune(false, Some(protected)).unwrap().deferred);
        assert!(store.root.join(protected).exists());
        // A dry-run preview reports protection without collecting.
        let preview = store.prune(true, Some(protected)).unwrap();
        assert!(preview.deferred);
        assert_eq!(preview.candidates, vec![protected.to_owned()]);
        drop(second);
        // After the last holder releases, normal retention resumes.
        let report = store.prune(false, Some(protected)).unwrap();
        assert!(!report.deferred);
        assert_eq!(report.removed, vec![protected.to_owned()]);
        assert!(store.list().unwrap().0.len() == 100);
    }

    #[test]
    fn invalid_retention_guard_fails_before_control_and_is_not_followed() {
        let (dir, store) = store();
        let id = "0000000000b1";
        seed(&store, id, State::Running);
        let guard = store.root.join(".retention");
        let target = dir.path().join("elsewhere");
        std::os::unix::fs::symlink(&target, &guard).unwrap();
        let error = bulk_selection(&store, None, None, true)
            .err()
            .expect("symlinked guard must refuse protection");
        assert!(
            format!("{error:#}").contains("retention guard"),
            "{error:#}"
        );
        assert!(!target.exists());
        // Dry runs take no protection and remain available.
        let (selected, _, none) = bulk_selection(&store, None, None, false).unwrap();
        assert_eq!(selected.len(), 1);
        assert!(none.is_none());
        // Collection cannot verify protection, so it defers with a warning.
        let report = store.prune(false, None).unwrap();
        assert!(report.deferred);
        assert!(
            report.warnings[0].contains("retention guard"),
            "{:?}",
            report.warnings
        );
        assert!(!target.exists());

        // A FIFO neither blocks the open nor counts as a guard.
        std::fs::remove_file(&guard).unwrap();
        let path = std::ffi::CString::new(guard.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(bulk_selection(&store, None, None, true).is_err());
        assert!(store.prune(false, None).unwrap().deferred);
        std::fs::remove_file(&guard).unwrap();
        assert!(bulk_selection(&store, None, None, true)
            .unwrap()
            .2
            .is_some());
    }

    #[test]
    fn selected_starting_job_is_stopped_once_running() {
        let (_dir, store) = store();
        let id = "00000000000e";
        let selected = seed(&store, id, State::Starting);
        let socket = store.root.join(id).join("ctl.sock");
        // A test-owned endpoint plays the supervisor. It turns the job running
        // only after a probe proves the dispatcher observed it starting.
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server_store = store.clone();
        let server = thread::spawn(move || {
            let store = server_store;
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut probes_while_starting = 0;
            let mut stops = 0;
            let mut record = store.record(id).unwrap();
            while Instant::now() < deadline && stops == 0 {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                stream.set_nonblocking(true).unwrap();
                let request = read_control(&mut stream, Instant::now() + Duration::from_secs(1));
                match request {
                    Ok(Some(Control::Stop)) => {
                        assert_eq!(record.state, State::Running);
                        stops += 1;
                        record.state = State::Stopping;
                        record.reason = Some(Reason::Requested);
                        store.save(&record).unwrap();
                        let reply = Reply {
                            record: record.clone(),
                            error: None,
                        };
                        write_reply(&mut stream, &reply, Instant::now() + Duration::from_secs(1));
                        record.finish();
                        store.save(&record).unwrap();
                        std::fs::remove_file(&socket).unwrap();
                    }
                    Ok(None) if record.state == State::Starting => {
                        probes_while_starting += 1;
                        record.state = State::Running;
                        store.save(&record).unwrap();
                    }
                    Ok(None) => (),
                    other => panic!("unexpected request: {:?}", other.err()),
                }
            }
            (probes_while_starting, stops)
        });
        let results = stop_selected(&store, vec![selected]);
        let (probes, stops) = server.join().unwrap();
        assert_eq!((probes, stops), (1, 1));
        assert_eq!(results[0].selected.state, State::Starting);
        assert_eq!(
            results[0].status,
            BulkStatus::Finished,
            "{:?}",
            results[0].error
        );
        assert_eq!(results[0].record.state, State::Stopped);
    }
}
